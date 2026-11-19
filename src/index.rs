//! The indexer: `sync` (on-demand stat sweep) and `rebuild` (drop and re-index), doc 02 §4 and doc 05 §4.
//!
//! The whole sweep runs inside one `BEGIN IMMEDIATE` transaction, so concurrent processes queue instead of indexing
//! the same files twice.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::{self, SchemaState, meta_keys};
use crate::error::{Error, Result};
use crate::hash::sha256_hex;
use crate::parse::observation::NearMiss;
use crate::parse::{Gap, ParsedNote, Warning, WarningKind, parse_note, silence};
use crate::paths::{
    ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, DB_ENV, canonical_vault_root, database_path, display_path, file_stem,
    namespace_of_key, note_id, note_key, same_vault_root,
};
use crate::resolve::{Resolution, Step, link_key, names_of, resolve};
use crate::walk::{FileEntry, NameError, WalkPoint, WalkStats, Walked, walk_with_hook};

/// TTL gate: a sweep is skipped when the last one finished less than this long ago (doc 02 §4).
pub const SWEEP_TTL_MS: i64 = 2000;

/// Predicate of inline mentions and Markdown links (doc 03 §3).
pub const LINKS_TO: &str = "links_to";

#[derive(Debug, Clone, Copy, Default)]
pub struct SweepOptions {
    /// Collect the shadow-mode diagnostics (namespace table, ghost targets, gap counters, walk details).
    pub report: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepKind {
    Rebuild,
    Sync,
}

/// Why a sweep dropped and re-indexed everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullCause {
    /// `recognitor rebuild`.
    Requested,
    /// The database had no tables.
    FreshDatabase,
    /// The database had another `user_version` (doc 05 §4.3).
    SchemaVersion(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteWarning {
    /// The note's id as written.
    pub note: String,
    pub warning: Warning,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub notes: i64,
    pub observations: i64,
    pub edges: i64,
    pub typed_edges: i64,
    pub ghosts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRow {
    pub namespace: String,
    pub notes: i64,
    pub observations: i64,
    pub edges: i64,
    pub ghosts: i64,
    pub bytes: i64,
    pub has_index: bool,
}

/// One ghost, as doc 05 §3.5 groups them: by `ref_key`, shown with one spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostRow {
    pub target_ref: String,
    pub mentions: i64,
    pub predicates: String,
}

/// Shadow-mode diagnostics, collected with [`SweepOptions::report`].
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub db_path: String,
    pub gaps: BTreeMap<Gap, usize>,
    pub steps: BTreeMap<Step, usize>,
    pub ghost_links: usize,
    pub ambiguous_links: usize,
    pub suppressed_archive_observations: usize,
    pub suppressed_archive_links: usize,
    pub suppressed_bootstrap_observations: usize,
    /// Bracket-led list items that are not observations, by reason.
    pub near_misses: BTreeMap<NearMiss, usize>,
    /// Links merged into an existing row with the same predicate and link key (doc 05 §2.2).
    pub collapsed_links: usize,
    pub walk: WalkStats,
    pub namespaces: Vec<NamespaceRow>,
    pub top_ghosts: Vec<GhostRow>,
}

#[derive(Debug, Clone)]
pub struct SweepReport {
    pub kind: SweepKind,
    pub full: Option<FullCause>,
    /// Files that passed the name rules and may be indexed.
    pub files_seen: usize,
    pub files_read: usize,
    /// Ids as written.
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub deleted: Vec<String>,
    pub retitled: Vec<String>,
    /// Notes whose spelling of the path changed while the key stayed (a case-only rename).
    pub renamed: Vec<String>,
    /// Files read because their metadata changed (or they were racy) whose checksum had not changed.
    pub rehashed: usize,
    /// Edges of unchanged notes that targeted re-resolution looked at, found by `ref_key` (doc 05 §2.2). Zero after
    /// a full re-index and after a sweep that added, removed or retitled nothing.
    pub reresolved: usize,
    /// Names the sweep refused to index: non-ASCII names and case twins (doc 03 §4).
    pub name_errors: Vec<NameError>,
    pub warnings: Vec<NoteWarning>,
    pub totals: Totals,
    /// Time inside the sweep transaction, walk included. Totals and diagnostics are not timed.
    pub elapsed: Duration,
    pub diagnostics: Option<Diagnostics>,
}

/// Reads one file for the sweep.
pub type FileReader<'a> = dyn Fn(&Path) -> std::io::Result<Vec<u8>> + 'a;

/// An open index over one vault.
pub struct Index {
    conn: Connection,
    root: PathBuf,
    root_display: String,
    db_path: PathBuf,
}

/// A stored note, as the sweep sees it.
struct Known {
    key: String,
    path: String,
    title: String,
    checksum: String,
    mtime: i64,
    size: i64,
}

/// One `edges` row before resolution.
struct EdgeRow {
    predicate: String,
    target_ref: String,
    ref_key: String,
    is_inline: bool,
    position: u32,
    context: Option<String>,
}

/// A note saved in this sweep: its id as written and its edge rows, whose own links get resolved.
struct Saved {
    id: String,
    rows: Vec<EdgeRow>,
}

#[derive(Default)]
struct Counters {
    gaps: BTreeMap<Gap, usize>,
    steps: BTreeMap<Step, usize>,
    ghost: usize,
    ambiguous: usize,
    archive_obs: usize,
    archive_links: usize,
    bootstrap_obs: usize,
    near_misses: BTreeMap<NearMiss, usize>,
    collapsed_links: usize,
}

impl Index {
    /// Opens the index for `vault`. The database path is `db_path`, else `RECOGNITOR_DB`, else the per-user cache
    /// directory (doc 05 §5).
    pub fn open(vault: &Path, db_path: Option<&Path>) -> Result<Index> {
        let root = canonical_vault_root(vault)?;
        let db_path = match db_path {
            Some(p) => p.to_path_buf(),
            None => database_path(&root, std::env::var_os(DB_ENV))?,
        };
        let conn = db::open(&db_path)?;
        let root_display = display_path(&root);
        Ok(Index {
            conn,
            root,
            root_display,
            db_path,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn vault_root(&self) -> &Path {
        &self.root
    }

    /// Takes the write lock between processes (doc 07 §5.1 step 1): `BEGIN IMMEDIATE`, waiting up to `busy_timeout`
    /// for another writer. Every write tool runs inside it.
    pub(crate) fn write_transaction(&mut self) -> Result<Transaction<'_>> {
        Ok(self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// `recognitor rebuild`: drop every table and re-index the whole vault.
    pub fn rebuild(&mut self, opts: SweepOptions) -> Result<SweepReport> {
        self.sweep(SweepKind::Rebuild, opts, &|p| fs::read(p), &|_| {})
    }

    /// `recognitor sync`: a forced stat sweep, ignoring the TTL gate.
    pub fn sync(&mut self, opts: SweepOptions) -> Result<SweepReport> {
        self.sweep(SweepKind::Sync, opts, &|p| fs::read(p), &|_| {})
    }

    /// [`Index::sync`] with the file reader replaced and a hook in the walk. A test seam for files that vanish or lock
    /// between the walk and the read, or during the walk, which no test can time on a real file system.
    #[doc(hidden)]
    pub fn sync_with_seams<F: Fn(WalkPoint<'_>) + Sync>(
        &mut self,
        opts: SweepOptions,
        read: &FileReader<'_>,
        hook: &F,
    ) -> Result<SweepReport> {
        self.sweep(SweepKind::Sync, opts, read, hook)
    }

    /// The sweep queries run before they read the index: skipped when another process swept less than 2 seconds
    /// ago. The gate time lives in `meta`, so all processes share it (doc 02 §4).
    pub fn sync_if_stale(&mut self, opts: SweepOptions) -> Result<Option<SweepReport>> {
        if db::schema_state(&self.conn)? == SchemaState::Current {
            let last = db::meta_get(&self.conn, meta_keys::LAST_SWEEP_FINISHED_MS)?.and_then(|v| v.parse::<i64>().ok());
            if last.is_some_and(|t| now_ms() - t < SWEEP_TTL_MS) {
                return Ok(None);
            }
        }
        self.sweep(SweepKind::Sync, opts, &|p| fs::read(p), &|_| {}).map(Some)
    }

    /// `recognitor search`: the stale-gated sweep (doc 02 §4), then the full-text query (doc 07 §4). `page` counts
    /// from 1.
    pub fn search(&mut self, query: &str, ns: Option<&str>, page: usize) -> Result<crate::search::SearchPage> {
        self.sync_if_stale(SweepOptions::default())?;
        crate::search::search(&self.conn, query, ns, page)
    }

    /// `recognitor context`: the stale-gated sweep (doc 02 §4), then the links around one note (doc 07 §8). `page`
    /// counts from 1.
    pub fn context(&mut self, note: &str, page: usize) -> Result<crate::context::Context> {
        self.sync_if_stale(SweepOptions::default())?;
        crate::context::context(&self.conn, note, page)
    }

    /// `recognitor read`: the stale-gated sweep (doc 02 §4), then the note's file (doc 07 §7). `max_tokens` is
    /// `read.max_tokens`; `section` and `query` are doc 07 §7's arguments.
    pub fn read(
        &mut self,
        note: &str,
        section: Option<&str>,
        query: Option<&str>,
        max_tokens: usize,
    ) -> Result<crate::read::Read> {
        self.sync_if_stale(SweepOptions::default())?;
        crate::read::read(&self.conn, &self.root, note, section, query, max_tokens)
    }

    /// `recognitor bootstrap`: the stale-gated sweep (doc 02 §4), then the bootstrap walk (doc 04 §2.2).
    pub fn bootstrap(&mut self) -> Result<crate::bootstrap::Bootstrap> {
        self.sync_if_stale(SweepOptions::default())?;
        crate::bootstrap::load(&self.conn)
    }

    fn sweep<F: Fn(WalkPoint<'_>) + Sync>(
        &mut self,
        kind: SweepKind,
        opts: SweepOptions,
        read: &FileReader<'_>,
        hook: &F,
    ) -> Result<SweepReport> {
        let timer = Instant::now();
        let root = self.root.clone();
        let root_display = self.root_display.clone();
        // The state is read after the lock is taken: a process queued behind another one sees what that one
        // committed, so it does not re-index what is already there.
        let mut resets = 0;
        let mut forced: Option<FullCause> = None;
        let (tx, full) = loop {
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let state = db::schema_state(&tx)?;
            if state == SchemaState::Foreign {
                return Err(Error::ForeignDatabase(self.db_path.clone()));
            }
            if state != SchemaState::Fresh {
                // `Current` and `Outdated` are Recognitor databases, so `vault_root` is there (doc 05 §2.6).
                let stored = db::meta_get(&tx, meta_keys::VAULT_ROOT)?.unwrap_or_default();
                if !same_vault_root(&stored, &root_display) {
                    return Err(Error::VaultMismatch {
                        db: self.db_path.clone(),
                        stored,
                        current: root_display,
                    });
                }
            }
            let full = match (kind, state) {
                // After a reset the file is new, but the reason for the rebuild is still the first one.
                _ if forced.is_some() => forced,
                (SweepKind::Rebuild, _) => Some(FullCause::Requested),
                (_, SchemaState::Fresh) => Some(FullCause::FreshDatabase),
                (_, SchemaState::Outdated(v)) => Some(FullCause::SchemaVersion(v)),
                (_, _) => None,
            };
            if full.is_some() && state != SchemaState::Fresh {
                // The database is a disposable projection: start from a new file (doc 05 §4.3).
                drop(tx);
                forced = full;
                resets += 1;
                if resets > 3 {
                    return Err(Error::Vault("the database keeps changing under the sweep".to_string()));
                }
                self.reset_database()?;
                continue;
            }
            break (tx, full);
        };
        let result = run_sweep(tx, &root, &root_display, kind, full, opts, read, hook);
        let elapsed = timer.elapsed();
        let (mut report, counters, walk_stats) = result?;
        report.elapsed = elapsed;
        report.totals = totals(&self.conn)?;
        if opts.report {
            report.diagnostics = Some(self.diagnostics(counters, walk_stats)?);
        }
        Ok(report)
    }

    /// Closes the connection, deletes the database file with its `-wal` and `-shm`, and opens a new empty one.
    fn reset_database(&mut self) -> Result<()> {
        let placeholder = Connection::open_in_memory()?;
        let old = std::mem::replace(&mut self.conn, placeholder);
        old.close().map_err(|(_, e)| Error::Sql(e))?;
        let name = self.db_path.as_os_str();
        for suffix in ["", "-wal", "-shm"] {
            let mut file = name.to_os_string();
            file.push(suffix);
            let file = PathBuf::from(file);
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(file, e)),
            }
        }
        self.conn = db::open(&self.db_path)?;
        Ok(())
    }

    fn diagnostics(&self, c: Counters, walk: WalkStats) -> Result<Diagnostics> {
        let conn = &self.conn;
        let mut rows: BTreeMap<String, NamespaceRow> = BTreeMap::new();
        let mut put = |sql: &str, set: fn(&mut NamespaceRow, i64)| -> Result<()> {
            let mut stmt = conn.prepare(sql)?;
            let it = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in it {
                let (ns, n) = row?;
                let entry = rows.entry(ns.clone()).or_insert_with(|| NamespaceRow {
                    namespace: ns,
                    notes: 0,
                    observations: 0,
                    edges: 0,
                    ghosts: 0,
                    bytes: 0,
                    has_index: false,
                });
                set(entry, n);
            }
            Ok(())
        };
        put("SELECT namespace, COUNT(*) FROM notes GROUP BY namespace", |r, n| {
            r.notes = n
        })?;
        put("SELECT namespace, SUM(size) FROM notes GROUP BY namespace", |r, n| {
            r.bytes = n
        })?;
        put(
            "SELECT n.namespace, COUNT(*) FROM observations o JOIN notes n ON n.key = o.note_key GROUP BY n.namespace",
            |r, n| r.observations = n,
        )?;
        put(
            "SELECT n.namespace, COUNT(*) FROM edges e JOIN notes n ON n.key = e.source_key GROUP BY n.namespace",
            |r, n| r.edges = n,
        )?;
        put(
            "SELECT n.namespace, COUNT(*) FROM edges e JOIN notes n ON n.key = e.source_key \
             WHERE e.target_key IS NULL GROUP BY n.namespace",
            |r, n| r.ghosts = n,
        )?;
        let mut namespaces: Vec<NamespaceRow> = rows.into_values().collect();
        for row in &mut namespaces {
            // `index.md` is found whatever its case: the key is `<ns>/index` (doc 05 §3.2).
            row.has_index = conn
                .query_row(
                    "SELECT 1 FROM notes WHERE key = ?1",
                    [format!("{}/index", row.namespace)],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
        }
        // doc 05 §3.5, first 10 rows: ghosts grouped by `ref_key`, shown with one spelling.
        let mut stmt = conn.prepare(
            "SELECT MIN(e.target_ref) AS name, COUNT(*) AS mention_count, GROUP_CONCAT(DISTINCT e.predicate) \
             FROM edges e WHERE e.target_key IS NULL GROUP BY e.ref_key ORDER BY mention_count DESC, name LIMIT 10",
        )?;
        let top_ghosts = stmt
            .query_map([], |r| {
                Ok(GhostRow {
                    target_ref: r.get(0)?,
                    mentions: r.get(1)?,
                    predicates: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Diagnostics {
            db_path: self.db_path.display().to_string(),
            gaps: c.gaps,
            steps: c.steps,
            ghost_links: c.ghost,
            ambiguous_links: c.ambiguous,
            suppressed_archive_observations: c.archive_obs,
            suppressed_archive_links: c.archive_links,
            suppressed_bootstrap_observations: c.bootstrap_obs,
            near_misses: c.near_misses,
            collapsed_links: c.collapsed_links,
            walk,
            namespaces,
            top_ghosts,
        })
    }
}

/// The sweep proper, inside the transaction `tx` that already holds the write lock.
#[allow(clippy::too_many_arguments)]
fn run_sweep<F: Fn(WalkPoint<'_>) + Sync>(
    tx: Transaction<'_>,
    root: &Path,
    root_display: &str,
    kind: SweepKind,
    full: Option<FullCause>,
    opts: SweepOptions,
    read: &FileReader<'_>,
    hook: &F,
) -> Result<(SweepReport, Counters, WalkStats)> {
    let start_ms = now_ms();
    if full.is_some() {
        db::create_schema(&tx)?;
    }
    let prev_start = match full {
        Some(_) => None,
        None => db::meta_get(&tx, meta_keys::LAST_SWEEP_STARTED_MS)?.and_then(|v| v.parse::<i64>().ok()),
    };
    let known = load_known(&tx)?;
    let Walked {
        files,
        name_errors,
        unreadable,
        stats: walk_stats,
    } = walk_with_hook(root, opts.report, hook)?;

    let mut report = SweepReport {
        kind,
        full,
        files_seen: files.len(),
        files_read: 0,
        added: Vec::new(),
        changed: Vec::new(),
        deleted: Vec::new(),
        retitled: Vec::new(),
        renamed: Vec::new(),
        rehashed: 0,
        reresolved: 0,
        name_errors,
        warnings: Vec::new(),
        totals: Totals::default(),
        elapsed: Duration::ZERO,
        diagnostics: None,
    };
    let mut counters = Counters::default();
    let mut saved: HashMap<String, Saved> = HashMap::new();
    // Every name a link can use to reach a note that was added, removed or retitled (doc 03 §4).
    let mut names: HashSet<String> = HashSet::new();
    let mut seen: HashSet<&str> = HashSet::with_capacity(files.len());
    // Listed but not statable: not read this time, and not deleted either.
    let unreadable_keys: Vec<String> = unreadable.iter().map(|(rel, _)| note_key(rel)).collect();
    seen.extend(unreadable_keys.iter().map(String::as_str));
    for (rel, message) in &unreadable {
        report.warnings.push(NoteWarning {
            note: note_id(rel).to_string(),
            warning: Warning {
                kind: WarningKind::Unreadable,
                line: None,
                message: format!("file could not be read ({message}); skipped, the stored note is kept"),
            },
        });
    }

    for f in &files {
        seen.insert(f.key.as_str());
        let prior = known.get(&f.key);
        // A new spelling of a known key (a case-only rename) is stored even when (mtime, size) are unchanged.
        let renamed = prior.is_some_and(|k| k.path != f.rel);
        // A note without a title in its file takes the filename as its title, so a new spelling of the filename is a
        // new title. The stored title equal to the old filename is the sign; the file is read to be sure.
        let title_from_name = renamed && prior.is_some_and(|k| k.title == file_stem(note_id(&k.path)));
        if let Some(k) = prior {
            // Inequality, not ordering; racy files (mtime not older than the last sweep's start) are re-hashed.
            let racy = prev_start.is_some_and(|s| k.mtime >= s);
            if k.mtime == f.mtime_ms && k.size == f.size && !racy && !title_from_name {
                if renamed {
                    tx.execute("UPDATE notes SET path = ?1 WHERE key = ?2", params![f.rel, f.key])?;
                    report.renamed.push(note_id(&f.rel).to_string());
                }
                continue;
            }
        }
        let bytes = match read(&f.abs) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Gone since the walk (an atomic save's rename, or a delete): the same as deleted.
                seen.remove(f.key.as_str());
                report.files_seen = report.files_seen.saturating_sub(1);
                continue;
            }
            Err(e) => {
                // Locked or unreadable: skipped, and its stored rows stay as they were.
                report.warnings.push(unreadable_warning(&f.rel, &e));
                continue;
            }
        };
        report.files_read += 1;
        let checksum = sha256_hex(&bytes);
        if renamed {
            report.renamed.push(note_id(&f.rel).to_string());
        }
        if let Some(k) = prior
            && k.checksum == checksum
            && !title_from_name
        {
            // Checksum gate: timestamps changed, content did not.
            tx.execute(
                "UPDATE notes SET mtime = ?1, size = ?2, path = ?3 WHERE key = ?4",
                params![f.mtime_ms, f.size, f.rel, f.key],
            )?;
            report.rehashed += 1;
            continue;
        }
        let ix = index_note(
            &tx,
            f,
            bytes,
            &checksum,
            prior.is_some(),
            &mut counters,
            &mut report.warnings,
        )?;
        let ns = namespace_of_key(&f.key).unwrap_or_default();
        match prior {
            Some(k) => {
                if k.title != ix.title {
                    report.retitled.push(ix.id.clone());
                    names.extend(names_of(&f.key, ns, &k.title));
                    names.extend(names_of(&f.key, ns, &ix.title));
                }
                if k.checksum != checksum {
                    report.changed.push(ix.id.clone());
                }
            }
            None => {
                names.extend(names_of(&f.key, ns, &ix.title));
                report.added.push(ix.id.clone());
            }
        }
        saved.insert(
            f.key.clone(),
            Saved {
                id: ix.id,
                rows: ix.rows,
            },
        );
    }

    // Deleted files, and files that now break a name rule: the cascade removes their rows, the trigger turns
    // incoming links into ghosts, and their names go to re-resolution.
    let mut gone: Vec<&Known> = known.values().filter(|k| !seen.contains(k.key.as_str())).collect();
    gone.sort_by(|a, b| a.key.cmp(&b.key));
    for k in gone {
        names.extend(names_of(&k.key, namespace_of_key(&k.key).unwrap_or_default(), &k.title));
        tx.execute("DELETE FROM notes WHERE key = ?1", [&k.key])?;
        report.deleted.push(note_id(&k.path).to_string());
    }

    // Re-resolution (doc 03 §4). After a full re-index every edge belongs to a saved note. Otherwise only the
    // edges whose key matches a changed name are looked at, then the saved notes' own links. No full pass.
    if full.is_none() {
        let mut sorted: Vec<&String> = names.iter().collect();
        sorted.sort();
        for name in sorted {
            report.reresolved += reresolve_by_name(&tx, name, &saved, &mut counters, &mut report.warnings)?;
        }
    }
    let mut keys: Vec<&String> = saved.keys().collect();
    keys.sort();
    for key in keys {
        let note = &saved[key];
        let ns = namespace_of_key(key).unwrap_or_default();
        for row in &note.rows {
            let edge = EdgeRef {
                source_key: key,
                source_id: &note.id,
                predicate: &row.predicate,
                ref_key: &row.ref_key,
                target_ref: &row.target_ref,
                position: row.position,
                current: None,
            };
            resolve_edge(&tx, ns, &edge, &mut counters, &mut report.warnings)?;
        }
    }

    db::meta_set(&tx, meta_keys::VAULT_ROOT, root_display)?;
    db::meta_set(&tx, meta_keys::LAST_SWEEP_STARTED_MS, &start_ms.to_string())?;
    db::meta_set(&tx, meta_keys::LAST_SWEEP_FINISHED_MS, &now_ms().to_string())?;
    tx.commit()?;
    Ok((report, counters, walk_stats))
}

fn unreadable_warning(rel: &str, e: &std::io::Error) -> NoteWarning {
    NoteWarning {
        note: note_id(rel).to_string(),
        warning: Warning {
            kind: WarningKind::Unreadable,
            line: None,
            message: format!("file could not be read ({e}); skipped, the stored note is kept"),
        },
    }
}

/// What `index_note` hands back to the sweep.
struct Indexed {
    id: String,
    title: String,
    rows: Vec<EdgeRow>,
}

/// Parses one file and writes its `notes`, `observations` and `edges` rows (targets unresolved).
fn index_note(
    tx: &Transaction<'_>,
    f: &FileEntry,
    bytes: Vec<u8>,
    checksum: &str,
    existed: bool,
    counters: &mut Counters,
    warnings: &mut Vec<NoteWarning>,
) -> Result<Indexed> {
    let id = note_id(&f.rel).to_string();
    let key = f.key.as_str();
    // The walk never returns a file in the vault root, so a key always has a folder.
    let ns = namespace_of_key(key).unwrap_or_default();
    let (content, utf8_ok) = match String::from_utf8(bytes) {
        Ok(s) => (s, true),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), false),
    };
    let mut note = parse_note(&content);
    if !utf8_ok {
        note.gaps.push(Gap::InvalidUtf8);
    }
    let suppressed = silence(&mut note, ns);
    if ns == ARCHIVE_NAMESPACE {
        counters.archive_obs += suppressed.observations;
        counters.archive_links += suppressed.links;
    } else if ns == BOOTSTRAP_NAMESPACE {
        counters.bootstrap_obs += suppressed.observations;
    }
    let title = match &note.title {
        Some(t) => t.clone(),
        None => {
            note.gaps.push(Gap::TitleFromFilename);
            file_stem(&id).to_string()
        }
    };
    for g in &note.gaps {
        *counters.gaps.entry(*g).or_default() += 1;
    }
    for m in &note.near_misses {
        *counters.near_misses.entry(*m).or_default() += 1;
    }
    let note_type = note.note_type.as_deref().unwrap_or("note");

    // doc 05 §4.2: the update sets every column except `pk` and `key`.
    tx.prepare_cached(
        "INSERT INTO notes (key, namespace, type, title, title_key, name_key, path, summary, content, checksum, \
                            mtime, size) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT(key) DO UPDATE SET \
             namespace = excluded.namespace, type = excluded.type, title = excluded.title, \
             title_key = excluded.title_key, name_key = excluded.name_key, path = excluded.path, \
             summary = excluded.summary, content = excluded.content, checksum = excluded.checksum, \
             mtime = excluded.mtime, size = excluded.size",
    )?
    .execute(params![
        key,
        ns,
        note_type,
        title,
        title.to_lowercase(),
        file_stem(key),
        f.rel,
        note.summary,
        content,
        checksum,
        f.mtime_ms,
        f.size
    ])?;
    if existed {
        tx.execute("DELETE FROM observations WHERE note_key = ?1", [key])?;
        tx.execute("DELETE FROM edges WHERE source_key = ?1", [key])?;
    }

    let mut insert_obs = tx.prepare_cached(
        "INSERT INTO observations (note_key, category, content, qualifiers, tags, line_number) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for o in &note.observations {
        insert_obs.execute(params![key, o.category, o.content, o.qualifiers, o.tags, o.line])?;
    }

    let (rows, eligible) = edge_rows(key, &note);
    counters.collapsed_links += eligible - rows.len();
    let mut insert_edge = tx.prepare_cached(
        "INSERT INTO edges (source_key, predicate, target_ref, ref_key, is_inline, position, context) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
    )?;
    for r in &rows {
        insert_edge.execute(params![
            key,
            r.predicate,
            r.target_ref,
            r.ref_key,
            r.is_inline,
            r.position,
            r.context
        ])?;
    }

    warnings.extend(note.warnings.into_iter().map(|warning| NoteWarning {
        note: id.clone(),
        warning,
    }));
    Ok(Indexed { id, title, rows })
}

/// Re-indexes one note whose file a write tool has just replaced (doc 07 §5.1 step 5), inside that tool's
/// transaction. `rel` is the stored path and `bytes` what was written. Works like the sweep for one saved file: the
/// note's rows are rewritten, its own links resolved, and when its title changed, the links that use the old or the
/// new title are resolved again (doc 03 §4).
pub(crate) fn reindex_written(tx: &Transaction<'_>, root: &Path, rel: &str, bytes: Vec<u8>) -> Result<()> {
    let abs = root.join(rel);
    let meta = fs::metadata(&abs).map_err(|e| Error::io(&abs, e))?;
    let key = note_key(rel);
    let f = FileEntry {
        rel: rel.to_string(),
        abs,
        key: key.clone(),
        mtime_ms: crate::walk::mtime_ms(&meta),
        size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
    };
    let prior_title: Option<String> = tx
        .query_row("SELECT title FROM notes WHERE key = ?1", [&key], |r| r.get(0))
        .optional()?;
    let checksum = sha256_hex(&bytes);
    let mut counters = Counters::default();
    let mut warnings = Vec::new();
    let ix = index_note(
        tx,
        &f,
        bytes,
        &checksum,
        prior_title.is_some(),
        &mut counters,
        &mut warnings,
    )?;
    let ns = namespace_of_key(&key).unwrap_or_default();
    let mut names: Vec<String> = Vec::new();
    if prior_title.as_deref() != Some(ix.title.as_str()) {
        if let Some(old) = &prior_title {
            names.extend(names_of(&key, ns, old));
        }
        names.extend(names_of(&key, ns, &ix.title));
        names.sort();
        names.dedup();
    }
    let saved = HashMap::from([(
        key.clone(),
        Saved {
            id: ix.id,
            rows: ix.rows,
        },
    )]);
    for name in &names {
        reresolve_by_name(tx, name, &saved, &mut counters, &mut warnings)?;
    }
    let note = &saved[&key];
    for row in &note.rows {
        let edge = EdgeRef {
            source_key: &key,
            source_id: &note.id,
            predicate: &row.predicate,
            ref_key: &row.ref_key,
            target_ref: &row.target_ref,
            position: row.position,
            current: None,
        };
        resolve_edge(tx, ns, &edge, &mut counters, &mut warnings)?;
    }
    Ok(())
}

/// A link that pointed to a note a write tool has just removed, and now points nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokenEdge {
    /// The linking note's id as doc 07 §4.2 shows it, with its title when that differs from the filename.
    pub source: String,
    pub predicate: String,
    /// The link's target as the linking note wrote it.
    pub target_ref: String,
    pub now: LinkNow,
}

/// Where a link that pointed to a deleted note leads now (doc 07 §10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkNow {
    /// No note: a ghost.
    Nowhere,
    /// The target now matches several notes, so the link is a ghost until one is named (doc 03 §4).
    Ambiguous,
    /// Another note, by the same link: its id as written.
    Note(String),
}

/// Removes the rows of a note a write tool has just moved out of the vault (doc 07 §5.1 step 5, §10), inside that
/// tool's transaction. Works like the sweep for one deleted file: the cascade removes the note's rows, the trigger
/// turns the links that pointed to it into ghosts, and the links that use one of its names are resolved again, since
/// a name the note shared with another note may now reach that one (doc 03 §4).
///
/// Returns the links that pointed to the note, with where each leads now (doc 07 §10): nowhere, to several notes, or
/// to another note. Linking notes come in id order (ignoring case) and each note's links in written order. The
/// note's links to itself go with it.
pub(crate) fn unindex_deleted(tx: &Transaction<'_>, key: &str) -> Result<Vec<BrokenEdge>> {
    let title: String = tx.query_row("SELECT title FROM notes WHERE key = ?1", [key], |r| r.get(0))?;
    let ns = namespace_of_key(key).unwrap_or_default();
    let incoming: Vec<(String, String, String)> = {
        let mut stmt = tx.prepare_cached(
            "SELECT source_key, predicate, ref_key FROM edges WHERE target_key = ?1 AND source_key != ?1",
        )?;
        let rows = stmt.query_map([key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    tx.execute("DELETE FROM notes WHERE key = ?1", [key])?;
    let mut names = names_of(key, ns, &title);
    names.sort();
    names.dedup();
    let saved = HashMap::new();
    let mut counters = Counters::default();
    let mut warnings = Vec::new();
    for name in &names {
        reresolve_by_name(tx, name, &saved, &mut counters, &mut warnings)?;
    }
    struct Row {
        path: String,
        title: String,
        title_key: String,
        name_key: String,
        source_key: String,
        predicate: String,
        target_ref: String,
        ref_key: String,
        position: u32,
        target_key: Option<String>,
    }
    let mut broken: Vec<Row> = Vec::new();
    let mut stmt = tx.prepare_cached(concat!(
        "SELECT n.path, n.title, n.title_key, n.name_key, e.source_key, e.predicate, e.target_ref, e.ref_key, ",
        "e.position, e.target_key FROM edges e JOIN notes n ON n.key = e.source_key ",
        "WHERE e.source_key = ?1 AND e.predicate = ?2 AND e.ref_key = ?3",
    ))?;
    for (source_key, predicate, ref_key) in &incoming {
        let found = stmt
            .query_row(params![source_key, predicate, ref_key], |r| {
                Ok(Row {
                    path: r.get(0)?,
                    title: r.get(1)?,
                    title_key: r.get(2)?,
                    name_key: r.get(3)?,
                    source_key: r.get(4)?,
                    predicate: r.get(5)?,
                    target_ref: r.get(6)?,
                    ref_key: r.get(7)?,
                    position: r.get(8)?,
                    target_key: r.get(9)?,
                })
            })
            .optional()?;
        broken.extend(found);
    }
    // Doc 07 §10: by linking note id ignoring case, then line, predicate and target as written, as `move_note` sorts.
    // An id, not a path: `ops/Run book.md` sorts before `ops/Run.md`, but `ops/Run` comes before `ops/Run book`.
    broken.sort_by(|a, b| {
        (note_id(&a.path).to_lowercase(), a.position, &a.predicate, &a.target_ref).cmp(&(
            note_id(&b.path).to_lowercase(),
            b.position,
            &b.predicate,
            &b.target_ref,
        ))
    });
    broken
        .into_iter()
        .map(|r| {
            let now = match &r.target_key {
                Some(target) => {
                    let path: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [target], |x| x.get(0))?;
                    LinkNow::Note(note_id(&path).to_string())
                }
                None => match resolve(tx, namespace_of_key(&r.source_key).unwrap_or_default(), &r.ref_key)? {
                    Resolution::Ambiguous(_) => LinkNow::Ambiguous,
                    _ => LinkNow::Nowhere,
                },
            };
            let shown_title = (r.title_key != r.name_key).then_some(r.title.as_str());
            Ok(BrokenEdge {
                source: crate::paths::titled_id(note_id(&r.path), shown_title),
                predicate: r.predicate,
                target_ref: r.target_ref,
                now,
            })
        })
        .collect()
}

/// A link a move changed: where it leads now, who wrote it, and whether `relink` can fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MovedLink {
    pub edge: BrokenEdge,
    /// The linking note's id without its title: what `relink`'s `notes` takes.
    pub source_id: String,
    /// The link pointed to the moved note before the move and was written in another note, so `relink` rewrites it. The
    /// moved note's own links never are (doc 07 §6.1: they are fixed with `edit_note`).
    pub into_note: bool,
}

/// One link as it was before a move.
struct LinkBefore {
    source_key: String,
    predicate: String,
    ref_key: String,
    target_ref: String,
    position: u32,
    target_key: Option<String>,
}

/// Re-indexes a note a write tool has just moved (doc 07 §5.1 step 5, §6.1), inside that tool's transaction. `old_key`
/// is the note's key before the move, `new_rel` its path now (the file is already there) and `bytes` its content.
///
/// The note's rows under the old key go and the rows under the new key come in; when the key is the same (a case-only
/// rename) the rows are rewritten in place. Links are resolved again where the move can change them (doc 03 §4): the
/// ones whose key matches a name or path the note had or has now, and the note's own. Returns every link whose target
/// changed, by the rule of doc 07 §6.1, "Broken means changed by the move": the link led to a note before, and now leads
/// nowhere, to several notes, or to another note than it did. A link that led nowhere before is not listed, even when
/// it reaches a note now. The moved note counts as the same note at both keys. Linking notes come in path order
/// (ignoring case) and each note's links in written order.
pub(crate) fn reindex_moved(
    tx: &Transaction<'_>,
    root: &Path,
    old_key: &str,
    new_rel: &str,
    bytes: Vec<u8>,
) -> Result<Vec<MovedLink>> {
    let new_key = note_key(new_rel);
    let old_title: String = tx.query_row("SELECT title FROM notes WHERE key = ?1", [old_key], |r| r.get(0))?;
    let old_ns = namespace_of_key(old_key).unwrap_or_default().to_string();
    let new_ns = namespace_of_key(&new_key).unwrap_or_default().to_string();
    let new_title = parse_note(&String::from_utf8_lossy(&bytes))
        .title
        .unwrap_or_else(|| file_stem(note_id(new_rel)).to_string());
    let mut names = names_of(old_key, &old_ns, &old_title);
    names.extend(names_of(&new_key, &new_ns, &new_title));
    names.sort();
    names.dedup();

    // Before: the links into the note, from other notes, found by every name it had or has and by the target they
    // stored; and the note's own links.
    let mut incoming: BTreeMap<(String, String, String), LinkBefore> = BTreeMap::new();
    let mut own: HashMap<(String, String), Option<String>> = HashMap::new();
    {
        let mut by_ref = tx.prepare_cached(
            "SELECT source_key, predicate, ref_key, target_ref, position, target_key FROM edges \
             WHERE ref_key = ?1 AND source_key != ?2",
        )?;
        let mut by_target = tx.prepare_cached(
            "SELECT source_key, predicate, ref_key, target_ref, position, target_key FROM edges \
             WHERE target_key = ?1 AND source_key != ?1",
        )?;
        let row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<LinkBefore> {
            Ok(LinkBefore {
                source_key: r.get(0)?,
                predicate: r.get(1)?,
                ref_key: r.get(2)?,
                target_ref: r.get(3)?,
                position: r.get(4)?,
                target_key: r.get(5)?,
            })
        };
        let mut found: Vec<LinkBefore> = Vec::new();
        for name in &names {
            let rows = by_ref.query_map(params![name, old_key], row)?;
            found.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        let rows = by_target.query_map([old_key], row)?;
        found.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        for l in found {
            incoming.insert((l.source_key.clone(), l.predicate.clone(), l.ref_key.clone()), l);
        }
        let mut stmt =
            tx.prepare_cached("SELECT predicate, target_ref, target_key FROM edges WHERE source_key = ?1")?;
        let rows = stmt.query_map([old_key], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?;
        for r in rows {
            let (k, v) = r?;
            own.insert(k, v);
        }
    }

    // The move: the old rows go (their cascade and trigger leave the links into the note unresolved), the new rows come
    // in, and the links that could reach the note, by either place, are resolved again.
    if old_key != new_key {
        tx.execute("DELETE FROM notes WHERE key = ?1", [old_key])?;
    }
    reindex_written(tx, root, new_rel, bytes)?;
    let stored_target = |source_key: &str, predicate: &str, ref_key: &str| -> Result<Option<String>> {
        Ok(tx
            .query_row(
                "SELECT target_key FROM edges WHERE source_key = ?1 AND predicate = ?2 AND ref_key = ?3",
                params![source_key, predicate, ref_key],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    };
    let mut counters = Counters::default();
    let mut warnings = Vec::new();
    for l in incoming.values() {
        let source_id: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [&l.source_key], |r| r.get(0))?;
        let current = stored_target(&l.source_key, &l.predicate, &l.ref_key)?;
        let edge = EdgeRef {
            source_key: &l.source_key,
            source_id: note_id(&source_id),
            predicate: &l.predicate,
            ref_key: &l.ref_key,
            target_ref: &l.target_ref,
            position: l.position,
            current: current.as_deref(),
        };
        resolve_edge(
            tx,
            namespace_of_key(&l.source_key).unwrap_or_default(),
            &edge,
            &mut counters,
            &mut warnings,
        )?;
    }

    // After: a link is listed when it led to a note before and does not lead to the same note now.
    struct Changed {
        source_key: String,
        predicate: String,
        target_ref: String,
        ref_key: String,
        position: u32,
        into_note: bool,
    }
    let unchanged = |before: &str, after: Option<&str>| match after {
        Some(a) => a == before || (before == old_key && a == new_key),
        None => false,
    };
    let mut changed: Vec<Changed> = Vec::new();
    for l in incoming.values() {
        let Some(before) = l.target_key.as_deref() else {
            continue;
        };
        let after = stored_target(&l.source_key, &l.predicate, &l.ref_key)?;
        if !unchanged(before, after.as_deref()) {
            changed.push(Changed {
                source_key: l.source_key.clone(),
                predicate: l.predicate.clone(),
                target_ref: l.target_ref.clone(),
                ref_key: l.ref_key.clone(),
                position: l.position,
                into_note: before == old_key,
            });
        }
    }
    {
        let mut stmt = tx.prepare_cached(
            "SELECT predicate, target_ref, ref_key, position, target_key FROM edges WHERE source_key = ?1",
        )?;
        let rows = stmt.query_map([&new_key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for r in rows {
            let (predicate, target_ref, ref_key, position, after) = r?;
            let Some(Some(before)) = own.get(&(predicate.clone(), target_ref.clone())) else {
                continue;
            };
            if !unchanged(before, after.as_deref()) {
                changed.push(Changed {
                    source_key: new_key.clone(),
                    predicate,
                    target_ref,
                    ref_key,
                    position,
                    into_note: false,
                });
            }
        }
    }

    let mut out: Vec<(String, MovedLink, u32)> = Vec::new();
    for c in changed {
        let (path, title, title_key, name_key): (String, String, String, String) = tx.query_row(
            "SELECT path, title, title_key, name_key FROM notes WHERE key = ?1",
            [&c.source_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let source_ns = namespace_of_key(&c.source_key).unwrap_or_default();
        let now = match stored_target(&c.source_key, &c.predicate, &c.ref_key)? {
            Some(target) => {
                let path: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [&target], |x| x.get(0))?;
                LinkNow::Note(note_id(&path).to_string())
            }
            None => match resolve(tx, source_ns, &c.ref_key)? {
                Resolution::Ambiguous(_) => LinkNow::Ambiguous,
                _ => LinkNow::Nowhere,
            },
        };
        let shown_title = (title_key != name_key).then_some(title.as_str());
        out.push((
            note_id(&path).to_lowercase(),
            MovedLink {
                edge: BrokenEdge {
                    source: crate::paths::titled_id(note_id(&path), shown_title),
                    predicate: c.predicate,
                    target_ref: c.target_ref,
                    now,
                },
                source_id: note_id(&path).to_string(),
                into_note: c.into_note,
            },
            c.position,
        ));
    }
    // Two links on one line have no further order in the index; the target text keeps the list the same on every call.
    out.sort_by(|a, b| {
        (&a.0, a.2, &a.1.edge.predicate, &a.1.edge.target_ref).cmp(&(
            &b.0,
            b.2,
            &b.1.edge.predicate,
            &b.1.edge.target_ref,
        ))
    });
    Ok(out.into_iter().map(|(_, link, _)| link).collect())
}

/// Edge rows in insertion order: explicit relations first, then inline links, one row per `(predicate, ref_key)` with
/// the first spelling kept (doc 05 §2.2). Markdown links that leave the vault are dropped (doc 03 §3). Also returns
/// how many links could become rows, so the report can count the merged ones.
fn edge_rows(source_key: &str, note: &ParsedNote) -> (Vec<EdgeRow>, usize) {
    let mut rows = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut eligible = 0;
    for explicit in [true, false] {
        for link in note.links.iter().filter(|l| l.relation.is_some() == explicit) {
            let Some(ref_key) = link_key(source_key, link.syntax, &link.target) else {
                continue;
            };
            eligible += 1;
            let (predicate, context) = match &link.relation {
                Some(r) => (r.predicate.clone(), r.context.clone()),
                None => (LINKS_TO.to_string(), None),
            };
            if seen.insert((predicate.clone(), ref_key.clone())) {
                rows.push(EdgeRow {
                    predicate,
                    target_ref: link.target.clone(),
                    ref_key,
                    is_inline: !explicit,
                    position: link.line,
                    context,
                });
            }
        }
    }
    (rows, eligible)
}

/// An edge to resolve.
struct EdgeRef<'a> {
    source_key: &'a str,
    source_id: &'a str,
    predicate: &'a str,
    ref_key: &'a str,
    target_ref: &'a str,
    position: u32,
    /// The stored `target_key`, to skip the write when nothing changes.
    current: Option<&'a str>,
}

/// Resolves one edge and stores the new `target_key` when it differs from `edge.current`. Returns the resolution.
fn resolve_edge(
    tx: &Transaction<'_>,
    source_ns: &str,
    edge: &EdgeRef<'_>,
    counters: &mut Counters,
    warnings: &mut Vec<NoteWarning>,
) -> Result<()> {
    let resolution = resolve(tx, source_ns, edge.ref_key)?;
    if resolution.target_key() != edge.current {
        tx.prepare_cached(
            "UPDATE edges SET target_key = ?4 WHERE source_key = ?1 AND predicate = ?2 AND ref_key = ?3",
        )?
        .execute(params![
            edge.source_key,
            edge.predicate,
            edge.ref_key,
            resolution.target_key()
        ])?;
    }
    match resolution {
        Resolution::Resolved { step, .. } => *counters.steps.entry(step).or_default() += 1,
        Resolution::Ghost => counters.ghost += 1,
        Resolution::Ambiguous(ids) => {
            counters.ambiguous += 1;
            warnings.push(NoteWarning {
                note: edge.source_id.to_string(),
                warning: Warning {
                    kind: WarningKind::AmbiguousLink,
                    line: Some(edge.position),
                    message: crate::resolve::ambiguous_message(edge.target_ref, &ids),
                },
            });
        }
    }
    Ok(())
}

/// Re-resolves the edges whose `ref_key` is `name`, found through `idx_edges_ref`. Edges of notes saved in this
/// sweep are left to the pass over saved notes. Returns how many edges it looked at.
fn reresolve_by_name(
    tx: &Transaction<'_>,
    name: &str,
    saved: &HashMap<String, Saved>,
    counters: &mut Counters,
    warnings: &mut Vec<NoteWarning>,
) -> Result<usize> {
    struct Row {
        source_key: String,
        source_path: String,
        predicate: String,
        target_ref: String,
        position: u32,
        current: Option<String>,
    }
    let rows: Vec<Row> = {
        let mut stmt = tx.prepare_cached(
            "SELECT e.source_key, n.path, e.predicate, e.target_ref, e.position, e.target_key \
             FROM edges e JOIN notes n ON n.key = e.source_key WHERE e.ref_key = ?1",
        )?;
        let it = stmt.query_map([name], |r| {
            Ok(Row {
                source_key: r.get(0)?,
                source_path: r.get(1)?,
                predicate: r.get(2)?,
                target_ref: r.get(3)?,
                position: r.get(4)?,
                current: r.get(5)?,
            })
        })?;
        it.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut looked_at = 0;
    for row in rows.iter().filter(|r| !saved.contains_key(&r.source_key)) {
        looked_at += 1;
        let source_id = note_id(&row.source_path);
        let edge = EdgeRef {
            source_key: &row.source_key,
            source_id,
            predicate: &row.predicate,
            ref_key: name,
            target_ref: &row.target_ref,
            position: row.position,
            current: row.current.as_deref(),
        };
        resolve_edge(
            tx,
            namespace_of_key(&row.source_key).unwrap_or_default(),
            &edge,
            counters,
            warnings,
        )?;
    }
    Ok(looked_at)
}

/// The sweep's stat load. It selects only the columns of `idx_notes_sweep` (doc 05 §2.1), so SQLite answers it from
/// the index and never reads the note text.
pub const LOAD_KNOWN_SQL: &str = "SELECT key, path, title, checksum, mtime, size FROM notes";

fn load_known(tx: &Transaction<'_>) -> Result<HashMap<String, Known>> {
    let mut stmt = tx.prepare(LOAD_KNOWN_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(Known {
            key: r.get(0)?,
            path: r.get(1)?,
            title: r.get(2)?,
            checksum: r.get(3)?,
            mtime: r.get(4)?,
            size: r.get(5)?,
        })
    })?;
    let mut known = HashMap::new();
    for k in rows {
        let k = k?;
        known.insert(k.key.clone(), k);
    }
    Ok(known)
}

fn totals(conn: &Connection) -> Result<Totals> {
    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    Ok(Totals {
        notes: one("SELECT COUNT(*) FROM notes")?,
        observations: one("SELECT COUNT(*) FROM observations")?,
        edges: one("SELECT COUNT(*) FROM edges")?,
        typed_edges: one("SELECT COUNT(*) FROM edges WHERE is_inline = 0")?,
        ghosts: one("SELECT COUNT(*) FROM edges WHERE target_key IS NULL")?,
    })
}

/// Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

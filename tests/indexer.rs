//! Indexer tests: `rebuild` and `sync` against the synthetic vault in `tests/fixtures/vault`, plus mutations made on a
//! temporary copy. Each test cites the doc rule it checks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use recognitor::db::{self, meta_keys};
use recognitor::index::FullCause;
use recognitor::parse::WarningKind;
use recognitor::walk::{NameError, NameErrorKind, WalkPoint, Walked, walk};
use recognitor::{Index, SweepOptions, SweepReport};
use rusqlite::OptionalExtension;

/// 2026-01-01T00:00:00Z: fixture files get this mtime so no file is racy after a rebuild.
const PAST: u64 = 1_767_225_600;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-test-{}-{n}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        TestVault {
            root,
            db: base.join("index.db"),
        }
    }

    fn fixture(name: &str) -> Self {
        let v = Self::new(name);
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault"),
            &v.root,
        );
        v
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes a file and gives it the fixed past mtime plus `offset_s` seconds.
    fn write(&self, rel: &str, content: &str, offset_s: u64) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        set_mtime(&p, UNIX_EPOCH + Duration::from_secs(PAST + offset_s));
    }

    fn open(&self) -> Index {
        Index::open(&self.root, Some(&self.db)).unwrap()
    }

    /// Makes the vault root a place where `A` and `a` are two files, when the platform allows it: always on Linux,
    /// per folder on Windows (`fsutil`), never on a default macOS volume. Returns false when it cannot, and the test
    /// skips. Call it before the first file is written, so subfolders inherit the setting.
    fn case_sensitive(&self) -> bool {
        if cfg!(windows) {
            let _ = Command::new("fsutil.exe")
                .args(["file", "setCaseSensitiveInfo"])
                .arg(&self.root)
                .arg("enable")
                .output();
        }
        let (a, b) = (self.path("probe_a"), self.path("PROBE_A"));
        fs::write(&a, "1").unwrap();
        fs::write(&b, "2").unwrap();
        let both = fs::read_dir(&self.root).unwrap().count() == 2;
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
        if !both {
            eprintln!("skipped: this file system folds case, so two files cannot be case twins here");
        }
        both
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            set_mtime(&target, UNIX_EPOCH + Duration::from_secs(PAST));
        }
    }
}

fn set_mtime(p: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn opts() -> SweepOptions {
    SweepOptions::default()
}

/// `Some(target_key)` for the edge `(source_key, predicate, ref_key)`, `None` when no such edge exists. A ghost is
/// `Some(None)`.
fn target_of(index: &Index, source_key: &str, predicate: &str, ref_key: &str) -> Option<Option<String>> {
    index
        .connection()
        .query_row(
            "SELECT target_key FROM edges WHERE source_key = ?1 AND predicate = ?2 AND ref_key = ?3",
            [source_key, predicate, ref_key],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
}

fn some(key: &str) -> Option<Option<String>> {
    Some(Some(key.to_string()))
}

fn count(index: &Index, sql: &str) -> i64 {
    index.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn note_keys(index: &Index) -> Vec<String> {
    let mut stmt = index
        .connection()
        .prepare("SELECT key FROM notes ORDER BY key")
        .unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

fn path_of(index: &Index, key: &str) -> String {
    index
        .connection()
        .query_row("SELECT path FROM notes WHERE key = ?1", [key], |r| r.get(0))
        .unwrap()
}

fn fts_hits(index: &Index, word: &str) -> Vec<String> {
    let mut stmt = index
        .connection()
        .prepare("SELECT n.key FROM notes_fts f JOIN notes n ON n.pk = f.rowid WHERE notes_fts MATCH ?1")
        .unwrap();
    stmt.query_map([word], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn warning_kinds(r: &SweepReport) -> Vec<WarningKind> {
    r.warnings.iter().map(|w| w.warning.kind).collect()
}

fn twin_error(paths: &[&str], left_out: usize) -> NameError {
    NameError {
        kind: NameErrorKind::CaseTwin,
        paths: paths.iter().map(|p| p.to_string()).collect(),
        notes_left_out: left_out,
    }
}

fn non_ascii_error(path: &str, left_out: usize) -> NameError {
    NameError {
        kind: NameErrorKind::NonAscii,
        paths: vec![path.to_string()],
        notes_left_out: left_out,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Whole vault

#[test]
fn rebuild_indexes_the_fixture_vault() {
    let v = TestVault::fixture("rebuild");
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(r.full, Some(FullCause::Requested));
    assert_eq!(
        r.files_seen, 14,
        "hidden folders, non-.md files and root files are skipped (doc 03 §4)"
    );
    assert_eq!(r.files_read, 14);
    assert_eq!(r.totals.notes, 14);
    assert_eq!(r.totals.observations, 5);
    assert_eq!(r.totals.edges, 13);
    assert_eq!(r.totals.typed_edges, 10);
    assert_eq!(r.totals.ghosts, 5);
    assert_eq!(warning_kinds(&r), vec![WarningKind::AmbiguousLink]);
    assert!(r.name_errors.is_empty());

    // The columns of doc 05 §2.1: key, namespace, title and filename keys, spelling in `path`.
    let row: (String, String, String, String, String, String) = index
        .connection()
        .query_row(
            "SELECT key, namespace, title, title_key, name_key, path FROM notes WHERE key = 'backend/release'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "backend/release".into(),
            "backend".into(),
            "Release Process".into(),
            "release process".into(),
            "release".into(),
            "backend/Release.md".into()
        )
    );

    // Type defaults to 'note'; title falls back to the H1 (doc 02 §8).
    let (ty, title): (String, String) = index
        .connection()
        .query_row(
            "SELECT type, title FROM notes WHERE key = 'backend/guides/setup'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((ty.as_str(), title.as_str()), ("note", "Setup"));
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM notes WHERE key LIKE '.obsidian%'"),
        0
    );
}

#[test]
fn schema_has_the_columns_and_indexes_of_doc_05() {
    let v = TestVault::fixture("schema");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let cols = |table: &str| -> Vec<String> {
        let mut stmt = index
            .connection()
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid"))
            .unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(
        cols("notes"),
        [
            "pk",
            "key",
            "namespace",
            "type",
            "title",
            "title_key",
            "name_key",
            "path",
            "summary",
            "checksum",
            "mtime",
            "size",
            "content"
        ]
    );
    assert_eq!(
        cols("edges"),
        [
            "source_key",
            "predicate",
            "target_ref",
            "ref_key",
            "target_key",
            "is_inline",
            "position",
            "context"
        ]
    );
    assert_eq!(
        cols("observations"),
        [
            "pk",
            "note_key",
            "category",
            "content",
            "qualifiers",
            "tags",
            "line_number"
        ],
        "no `id` column (DR2-13)"
    );
    let indexes: Vec<String> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(
        indexes,
        [
            "idx_edges_ref",
            "idx_edges_target",
            "idx_notes_mtime",
            "idx_notes_name",
            "idx_notes_namespace",
            "idx_notes_sweep",
            "idx_notes_title",
            "idx_notes_type",
            "idx_obs_note"
        ]
    );
    let title_index: Vec<String> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT name FROM pragma_index_info('idx_notes_title')")
            .unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert_eq!(title_index, ["title_key"], "title index is on the key alone");
}

#[test]
fn the_sweep_stat_load_is_answered_from_the_covering_index() {
    let v = TestVault::fixture("covering");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let plan: Vec<String> = {
        let mut stmt = index
            .connection()
            .prepare(&format!("EXPLAIN QUERY PLAN {}", recognitor::index::LOAD_KNOWN_SQL))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(
        plan.iter().any(|d| d.contains("USING COVERING INDEX idx_notes_sweep")),
        "{plan:?}"
    );
}

#[test]
fn bootstrap_and_archive_rules() {
    let v = TestVault::fixture("stores");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    // Bootstrap: edges kept and resolved inside the bootstrap, no observations, no full-text rows (doc 05 §2.3-2.4).
    // The fixture's bootstrap is a small tree: index -> Profile (hub) -> Identity.
    assert_eq!(
        target_of(&index, "bootstrap/index", "indexes", "profile"),
        some("bootstrap/profile/profile")
    );
    assert_eq!(
        target_of(&index, "bootstrap/profile/profile", "has_part", "identity"),
        some("bootstrap/profile/identity")
    );
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM observations WHERE note_key LIKE 'bootstrap/%'"
        ),
        0
    );
    assert!(fts_hits(&index, "zebraword").is_empty());
    // Archive: full-text searchable, graph-silent (doc 03 §3).
    assert_eq!(
        fts_hits(&index, "quokkaword"),
        vec!["archive/2026-11-01 session".to_string()]
    );
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM observations WHERE note_key LIKE 'archive/%'"
        ),
        0
    );
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM edges WHERE source_key LIKE 'archive/%'"),
        0
    );
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM edges WHERE ref_key = 'never written'"),
        0
    );
}

#[test]
fn store_rules_follow_the_lowercase_namespace() {
    // `Bootstrap/` and `Archive/` are the reserved namespaces whatever their case (doc 03 §4).
    let v = TestVault::new("store-case");
    v.write("Bootstrap/index.md", "# B\n\nS.\n\n- [rule] hidden rule\n", 0);
    v.write(
        "Archive/Log.md",
        "# Log\n\nSeen quokkaword.\n\n- [fact] x\n\nSee [[Nothing Here]].\n",
        0,
    );
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM notes WHERE namespace IN ('bootstrap', 'archive')"
        ),
        2
    );
    assert_eq!(count(&index, "SELECT COUNT(*) FROM observations"), 0);
    assert_eq!(count(&index, "SELECT COUNT(*) FROM edges"), 0);
    assert_eq!(fts_hits(&index, "quokkaword"), vec!["archive/log".to_string()]);
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH 'hidden'"),
        0
    );
}

#[test]
fn edges_one_row_per_link_key_explicit_first() {
    let v = TestVault::fixture("edges");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let conn = index.connection();
    // `links_to [[Auth Flow]]` under Relations plus two inline `[[Auth Flow]]`: one row, the explicit one
    // (doc 05 §2.2).
    let (is_inline, position): (i64, i64) = conn
        .query_row(
            "SELECT is_inline, position FROM edges WHERE source_key = 'backend/release' AND ref_key = 'auth flow'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((is_inline, position), (0, 15));
    // The link as written is kept for output; the key is for matching.
    let (target_ref, ref_key): (String, String) = conn
        .query_row(
            "SELECT target_ref, ref_key FROM edges WHERE source_key = 'backend/release' AND predicate = 'links_to' \
             AND target_ref LIKE '%Deploy%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (target_ref.as_str(), ref_key.as_str()),
        ("../ops/Deploy%20Checklist.md", "ops/deploy checklist")
    );
    // A Markdown link out of the vault makes no edge (doc 03 §3).
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM edges WHERE target_ref LIKE '%outside%'"),
        0
    );
    // Context is stored for a Relations line (doc 03 §3).
    let context: Option<String> = index
        .connection()
        .query_row("SELECT context FROM edges WHERE predicate = 'supersedes'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(context.as_deref(), Some("replaced after the 2026 audit"));
    // Resolution steps (doc 03 §4).
    assert_eq!(
        target_of(&index, "backend/index", "has_part", "auth flow"),
        some("backend/auth flow")
    );
    assert_eq!(
        target_of(&index, "backend/index", "relates_to", "cognition::index"),
        some("cognition/index")
    );
    assert_eq!(
        target_of(&index, "backend/index", "relates_to", "caching"),
        Some(None),
        "ambiguous stays a ghost"
    );
    assert_eq!(
        target_of(&index, "backend/release", "depends_on", "deploy checklist"),
        some("ops/deploy checklist")
    );
    assert_eq!(
        target_of(&index, "backend/release", "links_to", "backend/guides/setup"),
        some("backend/guides/setup")
    );
    assert_eq!(
        target_of(&index, "backend/release", "links_to", "ops/deploy checklist"),
        some("ops/deploy checklist"),
        "the Markdown link, resolved by its key"
    );
}

#[test]
fn one_row_per_key_keeps_the_first_spelling() {
    // `[[Caching]]` and `[[caching]]` are one link (doc 05 §2.2); the explicit row wins over an inline one.
    let v = TestVault::new("spelling");
    v.write("a/Caching.md", "# Caching\n\nS.\n", 0);
    v.write(
        "a/Src.md",
        "# Src\n\nS.\n\nSee [[caching]] and [[Caching]] and [[Gall's law]] and [[Gall's Law]].\n\n\
         ## Relations\n- relates_to [[CACHING]]\n- relates_to [[Caching]]\n",
        0,
    );
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(count(&index, "SELECT COUNT(*) FROM edges"), 3);
    let rows: Vec<(String, String, i64)> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT predicate, target_ref, is_inline FROM edges ORDER BY predicate, ref_key")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            ("links_to".to_string(), "caching".to_string(), 1),
            ("links_to".to_string(), "Gall's law".to_string(), 1),
            ("relates_to".to_string(), "CACHING".to_string(), 0),
        ]
    );
    assert_eq!(r.totals.ghosts, 1);
}

#[test]
fn ghost_written_in_two_spellings_is_counted_once() {
    // doc 05 §3.5: ghosts group by `ref_key`; `[[Gall's law]]` and `[[Gall's Law]]` are one ghost.
    let v = TestVault::new("ghost-group");
    v.write("a/One.md", "# One\n\nS.\n\nSee [[Gall's law]].\n", 0);
    v.write("a/Two.md", "# Two\n\nS.\n\nSee [[Gall's Law]].\n", 0);
    v.write("a/Three.md", "# Three\n\nS.\n\nSee [[Rare]].\n", 0);
    let mut index = v.open();
    let r = index
        .rebuild(SweepOptions { report: true })
        .unwrap()
        .diagnostics
        .unwrap();
    let ghosts = r.top_ghosts;
    assert_eq!(ghosts.len(), 2, "two ghosts, not three: {ghosts:?}");
    assert_eq!(ghosts[0].mentions, 2);
    assert!(ghosts[0].target_ref.eq_ignore_ascii_case("Gall's law"));
    assert_eq!(ghosts[0].predicates, "links_to");
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(DISTINCT ref_key) FROM edges WHERE target_key IS NULL"
        ),
        2
    );
}

#[test]
fn observations_are_replaced_with_their_note() {
    let v = TestVault::fixture("obs");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let key = "common/working-style/adhd profile";
    assert_eq!(
        count(
            &index,
            &format!("SELECT COUNT(*) FROM observations WHERE note_key = '{key}'")
        ),
        2
    );
    v.write(
        "common/working-style/ADHD profile.md",
        "# ADHD profile\n\nHow the user works best.\n\n- [rule] Short tasks work best.\n",
        100,
    );
    index.sync(opts()).unwrap();
    let cats: Vec<String> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT category FROM observations WHERE note_key = ?1")
            .unwrap();
        stmt.query_map([key], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(cats, vec!["rule".to_string()]);
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM observations_fts WHERE observations_fts MATCH 'tasks'"
        ),
        1,
        "the full-text rows follow through the triggers"
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Names (doc 03 §4, doc 07 §6)

#[test]
fn files_in_the_vault_root_are_not_notes() {
    let v = TestVault::fixture("root-files");
    v.write("README.md", "# Readme\n\nS.\n\n- [fact] x\n\nSee [[Auth Flow]].\n", 0);
    v.write(
        "backend/Links Root.md",
        "# Links Root\n\nSee [[Root note]], [[README]] and [up](../../README.md) and [root](/Root%20note.md).\n",
        0,
    );
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(r.files_seen, 15, "14 fixture notes plus Links Root; no root file");
    assert!(r.name_errors.is_empty(), "a root file is not an error either");
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM notes WHERE key IN ('readme', 'root note') OR key NOT LIKE '%/%'"
        ),
        0
    );
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM observations WHERE note_key = 'readme'"),
        0
    );
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM edges WHERE source_key = 'readme'"),
        0
    );
    // A link to a root file is a ghost, because there is no such note.
    assert_eq!(
        target_of(&index, "backend/links root", "links_to", "root note"),
        Some(None)
    );
    assert_eq!(
        target_of(&index, "backend/links root", "links_to", "readme"),
        Some(None)
    );
}

#[test]
fn path_links_into_the_root_or_outside_the_vault_are_ignored() {
    // doc 03 §3-4: a path link that lands in the vault root or leaves the vault can never reach a note, so it makes
    // no edge and no ghost. A name link such as `[[README]]` is a name lookup and stays a ghost.
    let v = TestVault::new("path-links");
    v.write(
        "a/Real.md",
        "# Real

S.
",
        0,
    );
    v.write(
        "a/Src.md",
        "# Src

S.

See [[../README]], [root](/README.md), [rel](../README.md), [[../../x]], [out](../../x.md), \
[[README]], [[./Real]] and [md](Real.md).
",
        0,
    );
    v.write(
        "README.md",
        "# Readme

A root file.
",
        0,
    );
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    let rows: Vec<(String, String, Option<String>)> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT target_ref, ref_key, target_key FROM edges ORDER BY ref_key")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            ("./Real".to_string(), "a/real".to_string(), Some("a/real".to_string())),
            ("README".to_string(), "readme".to_string(), None),
        ],
        "only the path link into a folder and the name link remain"
    );
    assert_eq!(r.totals.ghosts, 1);
}

#[test]
fn default_is_an_ordinary_namespace() {
    // doc 03 §4: there is no `default` namespace; `default/` is a folder like any other.
    let v = TestVault::new("default-ns");
    v.write("default/index.md", "# Default\n\nS.\n", 0);
    v.write("default/Plain.md", "# Plain\n\nS.\n\n- [fact] ordinary\n", 0);
    v.write(
        "other/Linker.md",
        "# Linker\n\nS.\n\nSee [[default::plain]] and [[Plain]].\n",
        0,
    );
    v.write("loose.md", "# Loose\n\nA root file.\n", 0);
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(
        note_keys(&index),
        vec!["default/index", "default/plain", "other/linker"]
    );
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM notes WHERE namespace = 'default'"),
        2
    );
    assert_eq!(r.totals.observations, 1);
    assert_eq!(
        target_of(&index, "other/linker", "links_to", "default::plain"),
        some("default/plain")
    );
    assert_eq!(
        target_of(&index, "other/linker", "links_to", "plain"),
        some("default/plain")
    );
}

#[test]
fn extension_in_any_case_is_a_note() {
    let v = TestVault::new("ext-case");
    v.write("ns/Shout.MD", "# Shout\n\nS.\n", 0);
    v.write("ns/Other.md", "# Other\n\nSee [[shout.md]].\n", 0);
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(note_keys(&index), vec!["ns/other", "ns/shout"]);
    assert_eq!(path_of(&index, "ns/shout"), "ns/Shout.MD");
    assert_eq!(target_of(&index, "ns/other", "links_to", "shout"), some("ns/shout"));
}

#[test]
fn non_ascii_names_are_errors_and_not_indexed() {
    let v = TestVault::new("non-ascii");
    v.write("ns/index.md", "# Ns\n\nS.\n", 0);
    v.write("ns/k\u{131}rm\u{131}z\u{131}.md", "# Bad file\n\nS.\n", 0);
    // The ASCII file name carries the Turkish title (doc 03 §4).
    v.write("ns/kirmizi.md", "# K\u{131}rm\u{131}z\u{131}\n\nS.\n", 0);
    v.write("\u{c7}al\u{131}\u{15f}ma/a.md", "# A\n\nS.\n", 0);
    v.write("\u{c7}al\u{131}\u{15f}ma/deep/b.md", "# B\n\nS.\n", 0);
    v.write(
        "ns/Link.md",
        "# Link\n\nSee [[KIRMIZI]], [[K\u{131}rm\u{131}z\u{131}]] and [[a]].\n",
        0,
    );
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(note_keys(&index), vec!["ns/index", "ns/kirmizi", "ns/link"]);
    assert_eq!(
        r.name_errors,
        vec![
            non_ascii_error("ns/k\u{131}rm\u{131}z\u{131}.md", 1),
            non_ascii_error("\u{c7}al\u{131}\u{15f}ma/", 2),
        ]
    );
    // Both link spellings reach the note; the link into the left-out folder is a plain ghost.
    assert_eq!(target_of(&index, "ns/link", "links_to", "kirmizi"), some("ns/kirmizi"));
    assert_eq!(
        target_of(&index, "ns/link", "links_to", "k\u{131}rm\u{131}z\u{131}"),
        some("ns/kirmizi")
    );
    assert_eq!(target_of(&index, "ns/link", "links_to", "a"), Some(None));
    // Nothing records the names: the database holds no trace of them.
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM notes WHERE path LIKE '%\u{131}%' OR path LIKE '%\u{15f}%'"
        ),
        0
    );

    // The next sweep reports them again, and indexes them once they are ASCII.
    let again = index.sync(opts()).unwrap();
    assert_eq!(again.name_errors.len(), 2);
    assert_eq!(again.files_read, 0);
    fs::rename(v.path("\u{c7}al\u{131}\u{15f}ma"), v.path("calisma")).unwrap();
    let healed = index.sync(opts()).unwrap();
    assert_eq!(healed.name_errors.len(), 1);
    assert_eq!(
        healed.added,
        vec!["calisma/a".to_string(), "calisma/deep/b".to_string()]
    );
    assert_eq!(target_of(&index, "ns/link", "links_to", "a"), some("calisma/a"));
}

#[test]
fn case_twin_files_are_all_left_out() {
    let v = TestVault::new("twin-files");
    if !v.case_sensitive() {
        return;
    }
    v.write("baseline/index.md", "# Baseline\n\nS.\n", 0);
    v.write("baseline/Nutrition.md", "# Nutrition Upper\n\nUpper twin.\n", 0);
    v.write("baseline/nutrition.md", "# Nutrition\n\nLower twin.\n", 0);
    v.write(
        "baseline/Sleep.md",
        "# Sleep\n\nSee [[baseline/nutrition]] and [[Nutrition]].\n",
        0,
    );
    v.write("baseline/Note.md", "# Note\n\nS.\n", 0);
    v.write("baseline/Note.MD", "# Note\n\nS.\n", 0);
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    // One or none: neither twin is indexed, and the error names them all.
    assert_eq!(note_keys(&index), vec!["baseline/index", "baseline/sleep"]);
    assert_eq!(
        r.name_errors,
        vec![
            twin_error(&["baseline/Note.MD", "baseline/Note.md"], 2),
            twin_error(&["baseline/Nutrition.md", "baseline/nutrition.md"], 2),
        ]
    );
    // A link to a twin is a plain ghost (doc 03 §4).
    assert_eq!(
        target_of(&index, "baseline/sleep", "links_to", "baseline/nutrition"),
        Some(None)
    );
    assert_eq!(target_of(&index, "baseline/sleep", "links_to", "nutrition"), Some(None));
    assert_eq!(r.warnings.len(), 0, "no ambiguity warning for a twin");

    // The next sweep finds them again.
    assert_eq!(index.sync(opts()).unwrap().name_errors.len(), 2);

    // Renaming one twin makes the other indexable, though that file did not change (doc 02 §4).
    fs::rename(v.path("baseline/Nutrition.md"), v.path("baseline/Nutrition old.md")).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.name_errors.len(), 1);
    assert_eq!(
        note_keys(&index),
        vec![
            "baseline/index",
            "baseline/nutrition",
            "baseline/nutrition old",
            "baseline/sleep"
        ]
    );
    assert_eq!(path_of(&index, "baseline/nutrition"), "baseline/nutrition.md");
    // The ghosts healed through the keys.
    assert_eq!(
        target_of(&index, "baseline/sleep", "links_to", "baseline/nutrition"),
        some("baseline/nutrition")
    );
    assert_eq!(
        target_of(&index, "baseline/sleep", "links_to", "nutrition"),
        some("baseline/nutrition")
    );
}

#[test]
fn a_new_twin_removes_the_indexed_note() {
    let v = TestVault::new("twin-arrives");
    if !v.case_sensitive() {
        return;
    }
    v.write("a/Foo.md", "# Foo\n\nS.\n", 0);
    v.write("a/Src.md", "# Src\n\nSee [[Foo]].\n", 0);
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(target_of(&index, "a/src", "links_to", "foo"), some("a/foo"));
    // A twin arrives (a git checkout on Linux, say). The note that was indexed leaves the index too.
    v.write("a/foo.md", "# Foo\n\nS.\n", 5);
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.deleted, vec!["a/Foo".to_string()]);
    assert_eq!(r.name_errors, vec![twin_error(&["a/Foo.md", "a/foo.md"], 2)]);
    assert_eq!(note_keys(&index), vec!["a/src"]);
    assert_eq!(target_of(&index, "a/src", "links_to", "foo"), Some(None));
    // Removing one twin brings the other back.
    fs::remove_file(v.path("a/Foo.md")).unwrap();
    let r = index.sync(opts()).unwrap();
    assert!(r.name_errors.is_empty());
    assert_eq!(r.added, vec!["a/foo".to_string()]);
    assert_eq!(target_of(&index, "a/src", "links_to", "foo"), some("a/foo"));
}

#[test]
fn case_twin_folders_leave_out_every_note_under_both() {
    let v = TestVault::new("twin-folders");
    if !v.case_sensitive() {
        return;
    }
    v.write("Baseline/a.md", "# A\n\nS.\n", 0);
    v.write("Baseline/deep/c.md", "# C\n\nS.\n", 0);
    v.write("baseline/b.md", "# B\n\nS.\n", 0);
    v.write("other/index.md", "# Other\n\nSee [[baseline::a]] and [[A]].\n", 0);
    // Not twins: a file and a folder with one stem.
    v.write("other/guide.md", "# Guide\n\nS.\n", 0);
    v.write("other/Guide/page.md", "# Page\n\nS.\n", 0);
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(
        note_keys(&index),
        vec!["other/guide", "other/guide/page", "other/index"]
    );
    assert_eq!(r.name_errors, vec![twin_error(&["Baseline/", "baseline/"], 3)]);
    assert_eq!(target_of(&index, "other/index", "links_to", "baseline::a"), Some(None));
}

// ---------------------------------------------------------------------------------------------------------------
// The sweep

#[test]
fn second_sync_with_no_changes_reads_no_files() {
    let v = TestVault::fixture("no-change");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.full, None);
    assert_eq!(r.files_seen, 14);
    assert_eq!(r.files_read, 0);
    assert!(r.added.is_empty() && r.changed.is_empty() && r.deleted.is_empty() && r.renamed.is_empty());
    assert_eq!(r.reresolved, 0);
}

#[test]
fn first_sync_on_a_fresh_database_indexes_everything() {
    let v = TestVault::fixture("fresh");
    let r = v.open().sync(opts()).unwrap();
    assert_eq!(r.full, Some(FullCause::FreshDatabase));
    assert_eq!(r.totals.notes, 14);
}

#[test]
fn checksum_gate_skips_reparse_when_only_mtime_changes() {
    let v = TestVault::fixture("touch");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    set_mtime(&v.path("ops/index.md"), UNIX_EPOCH + Duration::from_secs(PAST + 500));
    let r = index.sync(opts()).unwrap();
    assert_eq!((r.files_read, r.rehashed), (1, 1));
    assert!(r.changed.is_empty());
    // The new mtime is stored, so the next sync reads nothing.
    assert_eq!(index.sync(opts()).unwrap().files_read, 0);
}

#[test]
fn older_mtime_with_new_content_is_dirty() {
    let v = TestVault::fixture("older");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    // Inequality, not ordering (doc 02 §4): an older timestamp still marks the file dirty.
    let p = v.path("ops/index.md");
    fs::write(&p, "# Ops\n\nOperations notes, restored from a backup.\n").unwrap();
    set_mtime(&p, UNIX_EPOCH + Duration::from_secs(PAST - 86_400));
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.changed, vec!["ops/index".to_string()]);
}

#[test]
fn racy_file_is_rehashed_on_the_next_sweep() {
    let v = TestVault::fixture("racy");
    // A file whose mtime is not older than the sweep that indexed it (doc 02 §4, "Racy files").
    let future = UNIX_EPOCH + Duration::from_secs(4_102_444_800); // 2100-01-01
    let p = v.path("ops/index.md");
    fs::write(&p, "# Ops\n\nOperations notes.\n").unwrap();
    set_mtime(&p, future);
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    // Same size, same mtime, new content: only the racy rule can catch it.
    fs::write(&p, "# Ops\n\nOperations Notes.\n").unwrap();
    set_mtime(&p, future);
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.files_read, 1);
    assert_eq!(r.changed, vec!["ops/index".to_string()]);
}

#[test]
fn case_only_rename_is_stored_when_mtime_and_size_are_unchanged() {
    // doc 02 §4, "Names, not only stats"; doc 03 §4, "Case-only renames are not moves".
    let v = TestVault::fixture("case-rename");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let pk_before: i64 = index
        .connection()
        .query_row("SELECT pk FROM notes WHERE key = 'ops/deploy checklist'", [], |r| {
            r.get(0)
        })
        .unwrap();
    fs::rename(v.path("ops/Deploy Checklist.md"), v.path("ops/deploy CHECKLIST.md")).unwrap();
    let r = index.sync(opts()).unwrap();
    // The H1 equals the old filename, so the stored title could have come from the filename: the file is read once
    // to tell, and the H1 keeps the title.
    assert_eq!(r.files_read, 1);
    assert_eq!(r.renamed, vec!["ops/deploy CHECKLIST".to_string()]);
    assert!(r.added.is_empty() && r.deleted.is_empty() && r.changed.is_empty() && r.retitled.is_empty());
    assert_eq!(title_of(&index, "ops/deploy checklist"), "Deploy Checklist");
    assert_eq!(path_of(&index, "ops/deploy checklist"), "ops/deploy CHECKLIST.md");
    // Same key: the row, its observations and the links to it are untouched, so every link keeps resolving.
    let pk_after: i64 = index
        .connection()
        .query_row("SELECT pk FROM notes WHERE key = 'ops/deploy checklist'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(pk_before, pk_after);
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM observations WHERE note_key = 'ops/deploy checklist'"
        ),
        1
    );
    assert_eq!(
        target_of(&index, "backend/release", "depends_on", "deploy checklist"),
        some("ops/deploy checklist")
    );
    assert_eq!(r.reresolved, 0);
    // Stored once: the next sweep has nothing to do.
    assert!(index.sync(opts()).unwrap().renamed.is_empty());
}

#[test]
fn case_only_rename_of_a_folder_keeps_the_namespace() {
    let v = TestVault::fixture("case-rename-folder");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    fs::rename(v.path("ops"), v.path("OPS")).unwrap();
    let r = index.sync(opts()).unwrap();
    // Only `Deploy Checklist` has an H1 equal to its filename, which is what forces a read (see the test above).
    assert_eq!(r.files_read, 1);
    assert_eq!(r.renamed.len(), 2);
    assert_eq!(path_of(&index, "ops/index"), "OPS/index.md");
    assert_eq!(
        count(&index, "SELECT COUNT(*) FROM notes WHERE namespace = 'ops'"),
        2,
        "the namespace is the lowercase key"
    );
    assert_eq!(r.totals.notes, 14);
}

#[test]
fn ghosts_heal_when_the_target_is_created_anywhere() {
    let v = TestVault::fixture("heal");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(
        target_of(&index, "backend/auth flow", "part_of", "security architecture"),
        Some(None)
    );
    // Written at backend/specs/, not a guessed backend/security-architecture (doc 03 §4).
    v.write(
        "backend/specs/Security Architecture.md",
        "# Security Architecture\n\nHow the backend is secured.\n",
        10,
    );
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.added, vec!["backend/specs/Security Architecture".to_string()]);
    // Targeted: exactly the two edges whose key is `security architecture` were looked at, not the other 10.
    assert_eq!(r.reresolved, 2);
    let healed = some("backend/specs/security architecture");
    assert_eq!(
        target_of(&index, "backend/auth flow", "part_of", "security architecture"),
        healed
    );
    assert_eq!(
        target_of(&index, "backend/release", "links_to", "security architecture"),
        healed
    );
}

#[test]
fn link_written_first_heals_when_the_file_appears_by_every_name() {
    // A link written before its target existed resolves once the target is created (doc 03 §4).
    let v = TestVault::new("link-first");
    v.write("a/index.md", "# A\n\nS.\n", 0);
    v.write(
        "a/Src.md",
        "# Src\n\nS.\n\nSee [[Later Title]], [[later-file]], [[b::Later Title]], [[b/deep/later-file]], \
         [[Unrelated]].\n",
        0,
    );
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert_eq!(r.totals.ghosts, 5);
    v.write(
        "b/deep/later-file.md",
        "---\ntitle: Later Title\n---\n\n# Ignored\n\nS.\n",
        10,
    );
    let r = index.sync(opts()).unwrap();
    // The title, the filename, the prefixed title and the path match; `Unrelated` is not looked at.
    assert_eq!(r.reresolved, 4);
    assert_eq!(r.totals.ghosts, 1);
    for ref_key in ["later title", "later-file", "b::later title", "b/deep/later-file"] {
        assert_eq!(
            target_of(&index, "a/src", "links_to", ref_key),
            some("b/deep/later-file"),
            "{ref_key}"
        );
    }
    assert_eq!(target_of(&index, "a/src", "links_to", "unrelated"), Some(None));
}

#[test]
fn adding_a_second_note_with_a_name_makes_the_link_ambiguous() {
    let v = TestVault::new("ambiguous-on-add");
    v.write("a/Src.md", "# Src\n\nSee [[Caching]].\n", 0);
    v.write("b/Caching.md", "# Caching\n\nS.\n", 0);
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(target_of(&index, "a/src", "links_to", "caching"), some("b/caching"));
    v.write("c/Caching.md", "# Caching\n\nS.\n", 10);
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.reresolved, 1);
    assert_eq!(target_of(&index, "a/src", "links_to", "caching"), Some(None));
    assert_eq!(warning_kinds(&r), vec![WarningKind::AmbiguousLink]);
    // Deleting one brings the link back.
    fs::remove_file(v.path("c/Caching.md")).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.reresolved, 1);
    assert_eq!(target_of(&index, "a/src", "links_to", "caching"), some("b/caching"));
}

#[test]
fn deletes_revert_links_to_ghosts() {
    let v = TestVault::fixture("delete");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    fs::remove_file(v.path("ops/Deploy Checklist.md")).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.deleted, vec!["ops/Deploy Checklist".to_string()]);
    assert_eq!(
        target_of(&index, "backend/release", "depends_on", "deploy checklist"),
        Some(None)
    );
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM observations WHERE note_key = 'ops/deploy checklist'"
        ),
        0
    );
    assert!(
        fts_hits(&index, "Checklist")
            .iter()
            .all(|key| key != "ops/deploy checklist")
    );
}

#[test]
fn title_rename_breaks_title_links_but_not_filename_links() {
    let v = TestVault::fixture("retitle");
    v.write(
        "backend/Linker.md",
        "# Linker\n\nLinks by title and by filename.\n\nSee [[Release Process]] and [[Release]].\n",
        0,
    );
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(
        target_of(&index, "backend/linker", "links_to", "release process"),
        some("backend/release")
    );
    let pk_before: i64 = index
        .connection()
        .query_row("SELECT pk FROM notes WHERE key = 'backend/release'", [], |r| r.get(0))
        .unwrap();
    let content = fs::read_to_string(v.path("backend/Release.md"))
        .unwrap()
        .replace("Release Process", "Shipping");
    v.write("backend/Release.md", &content, 50);
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.retitled, vec!["backend/Release".to_string()]);
    assert_eq!(
        target_of(&index, "backend/linker", "links_to", "release process"),
        Some(None)
    );
    assert_eq!(
        target_of(&index, "backend/linker", "links_to", "release"),
        some("backend/release")
    );
    // The upsert refreshed `title_key` and kept the row (doc 05 §4.2).
    let (title_key, pk_after): (String, i64) = index
        .connection()
        .query_row(
            "SELECT title_key, pk FROM notes WHERE key = 'backend/release'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((title_key.as_str(), pk_after), ("shipping", pk_before));
    // And the new title heals links that were waiting for it.
    v.write("backend/Waiting.md", "# Waiting\n\nSee [[Shipping Later]].\n", 60);
    index.sync(opts()).unwrap();
    assert_eq!(
        target_of(&index, "backend/waiting", "links_to", "shipping later"),
        Some(None)
    );
    let content = fs::read_to_string(v.path("backend/Release.md"))
        .unwrap()
        .replace("Shipping", "Shipping Later");
    v.write("backend/Release.md", &content, 70);
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.retitled, vec!["backend/Release".to_string()]);
    assert_eq!(
        target_of(&index, "backend/waiting", "links_to", "shipping later"),
        some("backend/release")
    );
}

#[test]
fn a_move_outside_recognitor_breaks_path_links_and_is_not_reported() {
    // doc 07 §6.2: the sweep sees one file deleted and one created, and reports no move.
    let v = TestVault::fixture("move");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    // `[[backend/guides/Setup]]` is a path link; it breaks when the file moves (doc 03 §4).
    let content = fs::read(v.path("backend/guides/Setup.md")).unwrap();
    fs::remove_file(v.path("backend/guides/Setup.md")).unwrap();
    fs::create_dir_all(v.path("ops/guides")).unwrap();
    fs::write(v.path("ops/guides/Setup.md"), content).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.deleted, vec!["backend/guides/Setup".to_string()]);
    assert_eq!(r.added, vec!["ops/guides/Setup".to_string()]);
    assert!(r.renamed.is_empty());
    assert_eq!(
        target_of(&index, "backend/release", "links_to", "backend/guides/setup"),
        Some(None)
    );
}

#[test]
fn name_links_survive_a_move() {
    let v = TestVault::fixture("move-name");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let content = fs::read(v.path("ops/Deploy Checklist.md")).unwrap();
    fs::remove_file(v.path("ops/Deploy Checklist.md")).unwrap();
    fs::create_dir_all(v.path("ops/lists")).unwrap();
    fs::write(v.path("ops/lists/Deploy Checklist.md"), content).unwrap();
    index.sync(opts()).unwrap();
    assert_eq!(
        target_of(&index, "backend/release", "depends_on", "deploy checklist"),
        some("ops/lists/deploy checklist")
    );
    // The Markdown link is an exact path, so it breaks.
    assert_eq!(
        target_of(&index, "backend/release", "links_to", "ops/deploy checklist"),
        Some(None)
    );
}

#[test]
fn markdown_links_follow_the_key() {
    // doc 03 §3-4: all percent-encoding is decoded, `.md` is optional, relative paths are made full.
    let v = TestVault::new("markdown-links");
    v.write("common/guides/Setup.md", "# Setup\n\nS.\n", 0);
    v.write("common/guides/Note (draft).md", "# Draft\n\nS.\n", 0);
    v.write("common/Top.md", "# Top\n\nS.\n", 0);
    v.write(
        "common/guides/Linker.md",
        "# Linker\n\nS.\n\n[a](Setup) [b](Setup.md) [c](Note%20%28draft%29.md) [d](../Top.md) [e](/common/Top.md) \
         [f](/COMMON/top) [g](report.pdf) [h](folder/) [i](Setup.md#part)\n",
        0,
    );
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    let rows: Vec<(String, String, Option<String>)> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT target_ref, ref_key, target_key FROM edges ORDER BY position, ref_key")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        rows,
        vec![
            (
                "Note%20%28draft%29.md".to_string(),
                "common/guides/note (draft)".to_string(),
                Some("common/guides/note (draft)".to_string())
            ),
            (
                "Setup".to_string(),
                "common/guides/setup".to_string(),
                Some("common/guides/setup".to_string())
            ),
            (
                "../Top.md".to_string(),
                "common/top".to_string(),
                Some("common/top".to_string())
            ),
        ],
        "[b], [i] share [a]'s key; [e] and [f] share [d]'s; [g] and [h] are not notes"
    );
    assert_eq!(r.totals.ghosts, 0);
}

#[test]
fn schema_version_mismatch_rebuilds() {
    let v = TestVault::fixture("version");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    index.connection().pragma_update(None, "user_version", 99).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.full, Some(FullCause::SchemaVersion(99)));
    assert_eq!(r.totals.notes, 14);
    assert_eq!(db::schema_state(index.connection()).unwrap(), db::SchemaState::Current);
}

#[test]
fn ttl_gate_is_shared_through_meta() {
    let v = TestVault::fixture("ttl");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    // A second process opening the same database sees the gate.
    let mut other = v.open();
    assert!(other.sync_if_stale(opts()).unwrap().is_none());
    db::meta_set(other.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    assert!(other.sync_if_stale(opts()).unwrap().is_some());
}

#[test]
fn meta_records_the_vault_root_and_sweep_times_only() {
    let v = TestVault::fixture("meta");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    let conn = index.connection();
    let started: i64 = db::meta_get(conn, meta_keys::LAST_SWEEP_STARTED_MS)
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();
    let finished: i64 = db::meta_get(conn, meta_keys::LAST_SWEEP_FINISHED_MS)
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();
    assert!(started <= finished);
    let root = db::meta_get(conn, meta_keys::VAULT_ROOT).unwrap().unwrap();
    assert!(!root.starts_with(r"\\?\"));
    // There is no custom bootstrap directory (DR2-11), so no `bootstrap_root` key.
    let mut stmt = conn.prepare("SELECT key FROM meta ORDER BY key").unwrap();
    let keys: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    assert_eq!(keys, ["last_sweep_finished_ms", "last_sweep_started_ms", "vault_root"]);
}

#[test]
fn warnings_carry_note_and_line() {
    let v = TestVault::new("warnings");
    v.write(
        "ns/index.md",
        "---\ntitle: [bad\n---\n# Index\n\nS.\n\n## Relations\n- see also [[Other]]\n",
        0,
    );
    v.write("ns/Other.md", "# Other\n\nS.\n\n- [fact] @someday later\n", 0);
    let r = v.open().rebuild(opts()).unwrap();
    let mut got: Vec<(String, Option<u32>, WarningKind)> = r
        .warnings
        .iter()
        .map(|w| (w.note.clone(), w.warning.line, w.warning.kind))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("ns/Other".to_string(), Some(5), WarningKind::QualifierUnparsed),
            ("ns/index".to_string(), Some(1), WarningKind::InvalidFrontmatter),
            ("ns/index".to_string(), Some(9), WarningKind::RelationShape),
        ]
    );
}

#[test]
fn cli_rebuild_then_sync() {
    let v = TestVault::fixture("cli");
    v.write("\u{e7}/x.md", "# X\n\nS.\n", 0);
    let bin = env!("CARGO_BIN_EXE_recognitor");
    let run = |cmd: &str| {
        let out = Command::new(bin)
            .args([cmd, "--vault"])
            .arg(&v.root)
            .env("RECOGNITOR_DB", &v.db)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let rebuild = run("rebuild");
    assert!(
        rebuild
            .starts_with("rebuild: 14 notes, 5 observations, 13 edges (10 typed, 3 inline, 5 ghosts); read 14 of 14"),
        "{rebuild}"
    );
    // A non-ASCII name is an error line naming the path (doc 03 §4, doc 07 §6).
    assert!(
        rebuild.contains("warning: non-ASCII name, not indexed: \u{e7}/ (1 notes left out)"),
        "{rebuild}"
    );
    let sync = run("sync");
    assert!(
        sync.starts_with("sync: 14 notes") && sync.contains("read 0 of 14 files"),
        "{sync}"
    );
    assert!(sync.contains("warning: non-ASCII name"), "{sync}");

    let out = Command::new(bin).arg("sync").output().unwrap();
    assert_eq!(out.status.code(), Some(2), "missing --vault is a usage error");
}

// Fix-pass regressions.

fn title_of(index: &Index, key: &str) -> String {
    index
        .connection()
        .query_row("SELECT title FROM notes WHERE key = ?1", [key], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_file_gone_after_the_walk_counts_as_deleted() {
    let v = TestVault::fixture("vanished");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    v.write("ops/index.md", "# Ops\n\nOperations notes, edited.\n", 100);
    v.write("ops/Deploy Checklist.md", "# Deploy Checklist\n\nEdited.\n", 100);
    let r = index
        .sync_with_seams(
            opts(),
            &|p| {
                if p.ends_with("ops/index.md") {
                    Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                } else {
                    fs::read(p)
                }
            },
            &|_| {},
        )
        .unwrap();
    assert_eq!(r.deleted, vec!["ops/index".to_string()]);
    assert_eq!(
        r.changed,
        vec!["ops/Deploy Checklist".to_string()],
        "the rest of the sweep committed"
    );
    assert_eq!(r.totals.notes, 13);
    assert_eq!(count(&index, "SELECT COUNT(*) FROM notes WHERE key = 'ops/index'"), 0);
}

#[test]
fn an_unreadable_file_is_skipped_with_a_warning() {
    let v = TestVault::fixture("locked");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    v.write("ops/index.md", "# Ops\n\nOperations notes, edited.\n", 100);
    v.write("ops/Deploy Checklist.md", "# Deploy Checklist\n\nEdited.\n", 100);
    let r = index
        .sync_with_seams(
            opts(),
            &|p| {
                if p.ends_with("ops/index.md") {
                    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                } else {
                    fs::read(p)
                }
            },
            &|_| {},
        )
        .unwrap();
    assert_eq!(warning_kinds(&r), vec![WarningKind::Unreadable]);
    assert_eq!(r.warnings[0].note, "ops/index");
    assert!(r.deleted.is_empty());
    assert_eq!(r.changed, vec!["ops/Deploy Checklist".to_string()]);
    assert_eq!(r.totals.notes, 14, "the stored note is kept");
    // The next sweep retries it: its stored mtime was not updated.
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.changed, vec!["ops/index".to_string()]);
}

#[cfg(windows)]
#[test]
fn a_file_locked_by_another_process_is_skipped_with_a_warning() {
    use std::os::windows::fs::OpenOptionsExt;
    let v = TestVault::fixture("locked-real");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    v.write("ops/index.md", "# Ops\n\nOperations notes, edited.\n", 100);
    // share_mode(0): no other handle may read the file.
    let _lock = fs::File::options()
        .read(true)
        .share_mode(0)
        .open(v.path("ops/index.md"))
        .unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(warning_kinds(&r), vec![WarningKind::Unreadable]);
    assert_eq!(r.totals.notes, 14);
}

/// A small vault of its own: three notes in `docs/`, one of them `docs/doomed.md`.
fn vanish_vault(name: &str) -> TestVault {
    let v = TestVault::new(name);
    v.write(
        "docs/alpha.md",
        "# Alpha

First note.
",
        0,
    );
    v.write(
        "docs/beta.md",
        "# Beta

Second note.
",
        0,
    );
    v.write(
        "docs/doomed.md",
        "# Doomed

This one vanishes.
",
        0,
    );
    v
}

/// Deletes `docs/doomed.md` the moment the walk is about to stat it. Returns how often the hook fired for it.
fn vanish_at_stat(fired: &Mutex<usize>) -> impl Fn(WalkPoint<'_>) + Sync {
    move |point| {
        // `ends_with`, not `==`: the index may hold the vault root in another spelling than the test's.
        if let WalkPoint::BeforeStat(p) = point
            && p.ends_with("docs/doomed.md")
        {
            *fired.lock().unwrap() += 1;
            let _ = fs::remove_file(p);
        }
    }
}

#[test]
fn a_file_that_vanishes_during_the_walk_is_never_indexed() {
    let v = vanish_vault("vanish-new");
    let mut index = v.open();
    let fired = Mutex::new(0);
    let r = index
        .sync_with_seams(opts(), &|p| fs::read(p), &vanish_at_stat(&fired))
        .unwrap();
    assert_eq!(*fired.lock().unwrap(), 1, "the hook saw the file");
    assert_eq!(
        note_keys(&index),
        vec!["docs/alpha".to_string(), "docs/beta".to_string()]
    );
    assert_eq!(r.totals.notes, 2);
    assert!(r.deleted.is_empty(), "it was never indexed, so nothing was deleted");
    assert!(r.warnings.is_empty());
}

#[test]
fn an_indexed_file_that_vanishes_during_the_walk_counts_as_deleted() {
    let v = vanish_vault("vanish-known");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(count(&index, "SELECT COUNT(*) FROM notes"), 3);
    // An edit makes the file dirty, so the sweep needs it; the hook removes it before the walk stats it.
    v.write(
        "docs/doomed.md",
        "# Doomed

Edited, then gone.
",
        100,
    );
    let fired = Mutex::new(0);
    let r = index
        .sync_with_seams(opts(), &|p| fs::read(p), &vanish_at_stat(&fired))
        .unwrap();
    assert_eq!(*fired.lock().unwrap(), 1);
    assert_eq!(r.deleted, vec!["docs/doomed".to_string()]);
    assert_eq!(
        note_keys(&index),
        vec!["docs/alpha".to_string(), "docs/beta".to_string()]
    );
    assert_eq!(r.totals.notes, 2);
    assert!(r.warnings.is_empty());
}

fn make_foreign_db(path: &Path, user_version: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO users (name) VALUES ('a');",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", user_version).unwrap();
}

#[test]
fn a_database_that_is_not_ours_is_never_dropped() {
    for version in [0, 3, 4, 99] {
        let v = TestVault::fixture(&format!("foreign-{version}"));
        make_foreign_db(&v.db, version);
        for rebuild in [false, true] {
            let mut index = v.open();
            let err = if rebuild {
                index.rebuild(opts())
            } else {
                index.sync(opts())
            }
            .unwrap_err();
            assert!(matches!(err, recognitor::Error::ForeignDatabase(_)), "{err}");
            assert_eq!(count(&index, "SELECT COUNT(*) FROM users"), 1, "user_version {version}");
        }
    }
}

#[test]
fn a_database_of_another_vault_is_refused() {
    let a = TestVault::fixture("vault-a");
    let mut index = a.open();
    index.rebuild(opts()).unwrap();
    drop(index);
    // The second vault points at the first one's database.
    let b = TestVault::fixture("vault-b");
    for rebuild in [false, true] {
        let mut other = Index::open(&b.root, Some(&a.db)).unwrap();
        let err = if rebuild {
            other.rebuild(opts())
        } else {
            other.sync(opts())
        }
        .unwrap_err();
        assert!(matches!(err, recognitor::Error::VaultMismatch { .. }), "{err}");
        assert_eq!(count(&other, "SELECT COUNT(*) FROM notes"), 14, "nothing was dropped");
    }
    // The owner still syncs.
    let mut owner = a.open();
    assert_eq!(owner.sync(opts()).unwrap().full, None);
}

#[test]
fn the_schema_state_is_decided_after_the_lock() {
    let v = TestVault::fixture("lock-order");
    let mut queued = v.open();
    // Another process holds the write lock on a still empty database.
    let holder = db::open(&v.db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = std::thread::spawn(move || queued.sync(opts()).unwrap());
    std::thread::sleep(Duration::from_millis(300));
    // The holder builds the database and commits, as a finished sweep would.
    db::create_schema(&holder).unwrap();
    let root = recognitor::paths::display_path(&recognitor::paths::canonical_vault_root(&v.root).unwrap());
    db::meta_set(&holder, meta_keys::VAULT_ROOT, &root).unwrap();
    holder.execute_batch("COMMIT").unwrap();
    let r = worker.join().unwrap();
    assert_eq!(
        r.full, None,
        "the queued sweep saw a current database, not an empty one"
    );
    assert_eq!(r.added.len(), 14);
}

#[test]
fn case_only_rename_refreshes_a_filename_title() {
    let v = TestVault::fixture("case-title");
    v.write("ops/Plain Note.md", "Just text, no heading.\n\n- [fact] A fact.\n", 0);
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    assert_eq!(title_of(&index, "ops/plain note"), "Plain Note");
    fs::rename(v.path("ops/Plain Note.md"), v.path("ops/plain NOTE.md")).unwrap();
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.renamed, vec!["ops/plain NOTE".to_string()]);
    assert_eq!(r.retitled, vec!["ops/plain NOTE".to_string()]);
    assert!(r.changed.is_empty(), "the content did not change");
    assert_eq!(title_of(&index, "ops/plain note"), "plain NOTE");
    assert_eq!(path_of(&index, "ops/plain note"), "ops/plain NOTE.md");
    assert_eq!(
        count(
            &index,
            "SELECT COUNT(*) FROM observations WHERE note_key = 'ops/plain note'"
        ),
        1
    );
    // Stored once.
    let r = index.sync(opts()).unwrap();
    assert!(r.renamed.is_empty() && r.retitled.is_empty() && r.files_read == 0);
}

#[test]
fn a_database_with_a_module_that_is_not_loaded_is_rebuilt() {
    // A future `notes_vec` (a `vec0` virtual table) cannot be dropped without its module: the file is replaced.
    let v = TestVault::fixture("missing-module");
    let mut index = v.open();
    index.rebuild(opts()).unwrap();
    drop(index);
    {
        let conn = rusqlite::Connection::open(&v.db).unwrap();
        conn.execute_batch("PRAGMA writable_schema = ON").unwrap();
        conn.execute(
            "INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql) VALUES \
             ('table', 'notes_vec', 'notes_vec', 0, 'CREATE VIRTUAL TABLE notes_vec USING vec0(embedding float[4])')",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    let mut index = v.open();
    let vec_tables = "SELECT COUNT(*) FROM sqlite_master WHERE name = 'notes_vec'";
    assert_eq!(count(&index, vec_tables), 1, "the setup left a table without a module");
    let r = index.sync(opts()).unwrap();
    assert_eq!(r.full, Some(FullCause::SchemaVersion(99)));
    assert_eq!(r.totals.notes, 14);
    assert_eq!(count(&index, vec_tables), 0);
}

/// Everything a walk reports, in the order it reports it, as one comparable value.
fn walk_snapshot(w: &Walked) -> String {
    let files: Vec<_> = w.files.iter().map(|f| (&f.rel, &f.key, f.mtime_ms, f.size)).collect();
    let errors: Vec<_> = w
        .name_errors
        .iter()
        .map(|e| (e.kind, &e.paths, e.notes_left_out))
        .collect();
    format!(
        "{files:?}\n{errors:?}\n{:?}\n{:?}\n{:?} {} {} {:?}",
        w.unreadable,
        w.stats.hidden_dirs,
        w.stats.other_files,
        w.stats.root_md_files,
        w.stats.symlinks,
        w.stats.hidden_md_files
    )
}

/// The walk runs folders in parallel; its result must not depend on thread timing (doc 02 §4, doc 03 §4).
#[test]
fn the_walk_result_is_the_same_on_every_run() {
    let v = TestVault::new("walk-determinism");
    if !v.case_sensitive() {
        return;
    }
    let mut paths: Vec<String> = Vec::new();
    for ns in ["alpha", "beta", "gamma", "delta"] {
        for sub in ["one", "two", "three", "four"] {
            for leaf in ["x", "y"] {
                for i in 0..3 {
                    paths.push(format!("{ns}/{sub}/{leaf}/n{i}.md"));
                }
            }
            paths.push(format!("{ns}/{sub}/top.md"));
        }
        paths.push(format!("{ns}/index.md"));
        paths.push(format!("{ns}/data.json"));
        paths.push(format!("{ns}/.hidden/h.md"));
    }
    // A file twin and a folder twin in different folders, a non-ASCII file and folder in another, a hidden folder
    // at the top, a file in the root, and a folder whose name is a prefix of a sibling's, to test path order.
    for rel in [
        "beta/one/Twin.md",
        "beta/one/twin.MD",
        "gamma/Same/a.md",
        "gamma/same/b.md",
        "gamma/same/deep/c.md",
        "delta/two/k\u{131}rm\u{131}z\u{131}.md",
        "delta/\u{c7}al\u{131}\u{15f}ma/d.md",
        "alpha/three/a-b/e.md",
        "alpha/three/a/f.md",
        "root-note.md",
        ".top-hidden/k.md",
    ] {
        paths.push(rel.to_string());
    }
    for rel in &paths {
        v.write(rel, "# x\n", 0);
    }

    let first = walk(&v.root, true).unwrap();
    assert_eq!(first.name_errors.len(), 4, "{:?}", first.name_errors);
    let kinds: Vec<_> = first.name_errors.iter().map(|e| e.kind).collect();
    assert_eq!(kinds.iter().filter(|k| **k == NameErrorKind::CaseTwin).count(), 2);
    assert_eq!(kinds.iter().filter(|k| **k == NameErrorKind::NonAscii).count(), 2);
    assert!(first.files.windows(2).all(|w| w[0].rel < w[1].rel), "sorted by path");
    assert!(first.stats.hidden_dirs.windows(2).all(|w| w[0] < w[1]), "sorted");
    assert_eq!(first.stats.hidden_dirs.len(), 5);
    assert_eq!(first.stats.root_md_files, 1);
    let expected = walk_snapshot(&first);
    for run in 1..20 {
        let again = walk(&v.root, true).unwrap();
        assert_eq!(walk_snapshot(&again), expected, "run {run} differs from the first");
    }
}

/// Doc 03 §2: frontmatter is the block at the very start of the file. A `---` block later in the document is body
/// text, so it sets neither title nor type, and the observations and links between two horizontal rules are indexed.
#[test]
fn a_dashed_block_after_the_start_is_not_frontmatter() {
    let v = TestVault::new("mid-frontmatter");
    v.write(
        "notes/Real.md",
        "# Real\n\nSummary.\n\nText.\n\n---\ntitle: Mid\ntype: x\n---\n\nAfter [[Target]].\n",
        0,
    );
    v.write(
        "notes/Between.md",
        "# Between\n\nSummary.\n\n---\n- [fact] Kept between two rules.\nSee [[Target]].\n---\n",
        0,
    );
    v.write(
        "notes/Start.md",
        "---\ntitle: Front\ntype: spec\n---\n# Heading\n\nSummary.\n\n---\ntitle: Mid\n---\n",
        0,
    );
    v.write("notes/Target.md", "# Target\n\nS.\n", 0);
    let mut index = v.open();
    let r = index.rebuild(opts()).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let row = |key: &str| -> (String, String) {
        index
            .connection()
            .query_row("SELECT title, type FROM notes WHERE key = ?1", [key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
    };
    assert_eq!(row("notes/real"), ("Real".to_string(), "note".to_string()));
    assert_eq!(row("notes/between"), ("Between".to_string(), "note".to_string()));
    // Real frontmatter at the start still counts, and a second block after it does not override it.
    assert_eq!(row("notes/start"), ("Front".to_string(), "spec".to_string()));
    // The link after the stray block, and the link and fact between two rules, are indexed.
    assert_eq!(
        target_of(&index, "notes/real", "links_to", "target"),
        Some(Some("notes/target".to_string()))
    );
    assert_eq!(
        target_of(&index, "notes/between", "links_to", "target"),
        Some(Some("notes/target".to_string()))
    );
    let facts: i64 = index
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM observations WHERE note_key = 'notes/between'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(facts, 1);
}

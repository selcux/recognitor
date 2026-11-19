// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor ghosts [<link>]` (doc 07 §2, §6.2, doc 05 §3.5): the backlog of links that reach no note, with a hint
//! where a note that moved outside Recognitor probably went. CLI only, read only.
//!
//! **Two shapes.** Plain `ghosts` is the summary of doc 07 §6.2, one row per ghost. `ghosts <link>` is the detail of
//! the ghosts that name `<link>`: the row, the linking notes with their lines, and, when there is a hint, the
//! `recognitor relink` command that fixes them. The docs give the summary only; the detail is the slice's own
//! provisional design (see the slice report).
//!
//! **What counts as one ghost.** Links grouped by `ref_key`, as doc 05 §3.5 says: `[[Gall's law]]` and
//! `[[Gall's Law]]` are one. Ambiguous links are ghosts by doc 03 §4, step 5, so they are listed and marked
//! `(ambiguous)`, as `delete_note` marks them (doc 07 §10).
//!
//! **The relink command.** `relink`'s `old` is the id the moved note had. A ghost's `ref_key` says it for every link
//! that names a path: `/ns/a/name` (a relative or Markdown link, from the vault root), `ns::a/name` and `ns::name`
//! are `ns/a/name`, and a path with no prefix (`a/name`) starts at the linking note's namespace, so its `old` can
//! differ from note to note and each `old` gets its own command. `old` is spelled as the link writes it (the spelling
//! the summary line shows when links differ in case); a relative or Markdown link has only its key. A bare name (`Name`) does not say where the note
//! was, so no command is printed for it; the hint never fires for one anyway, since a note with that filename would
//! have resolved the link.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, params};

use crate::error::Result;
use crate::move_cli::finish_command;
use crate::paths::{note_id, strip_md_ignore_case, titled_id};
use crate::resolve::{Resolution, link_key, resolve};

/// The note a ghost probably became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    /// The id as written: `relink`'s `new`.
    pub id: String,
    /// Set only when the title differs from the filename by key (doc 07 §4.2).
    pub title: Option<String>,
}

/// One link that makes a ghost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The linking note's id as written.
    pub id: String,
    pub title: Option<String>,
    pub predicate: String,
    /// The link as the note wrote it.
    pub target_ref: String,
    pub line: i64,
}

/// A `recognitor relink` call that fixes some of a ghost's links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub old: String,
    pub new: String,
    /// Linking note ids, sorted ignoring case.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ghost {
    /// What the first column shows: one spelling of the link as written, or for a path from the vault root, that
    /// path with its leading `/`.
    pub name: String,
    pub links: usize,
    /// The notes the link matches, when it matches several (doc 03 §4, step 5).
    pub ambiguous: Option<Vec<String>>,
    pub hint: Option<Hint>,
    /// The linking notes; filled only for `ghosts <link>`.
    pub sources: Vec<Source>,
    /// Filled only for `ghosts <link>`, and only when there is a hint.
    pub fixes: Vec<Fix>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ghosts {
    /// The `<link>` asked for, if any.
    pub link: Option<String>,
    pub ghosts: Vec<Ghost>,
}

impl crate::Index {
    /// `recognitor ghosts`: the stale-gated sweep (doc 02 §4), then the ghosts (doc 05 §3.5).
    pub fn ghosts(&mut self, link: Option<&str>) -> Result<Ghosts> {
        self.sync_if_stale(crate::SweepOptions::default())?;
        ghosts(self.connection(), link)
    }
}

/// The ghosts, most mentioned first, then by link key. With `link`, only the ghosts that name it, with their sources.
pub fn ghosts(conn: &Connection, link: Option<&str>) -> Result<Ghosts> {
    // Doc 05 §3.5. The `predicates` column is not read: the summary does not show it.
    let mut stmt = conn.prepare(
        "SELECT MIN(e.target_ref), e.ref_key, COUNT(*) FROM edges e WHERE e.target_key IS NULL \
         GROUP BY e.ref_key ORDER BY COUNT(*) DESC, e.ref_key",
    )?;
    let groups = stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let wanted = link.map(|l| Wanted::new(conn, l)).transpose()?;
    let mut out = Vec::new();
    for (target_ref, ref_key, count) in groups {
        let name = if ref_key.starts_with('/') {
            ref_key.clone()
        } else {
            target_ref
        };
        if wanted.as_ref().is_some_and(|w| !w.matches(&name, &ref_key)) {
            continue;
        }
        let mut ghost = Ghost {
            name,
            links: usize::try_from(count).unwrap_or_default(),
            ambiguous: ambiguous_notes(conn, &ref_key)?,
            hint: None,
            sources: Vec::new(),
            fixes: Vec::new(),
        };
        // A link that matches several notes names none of them, so a note with the same filename is no hint.
        if ghost.ambiguous.is_none() {
            ghost.hint = hint(conn, &ref_key)?;
        }
        if wanted.is_some() {
            ghost.sources = sources(conn, &ref_key)?;
            if let Some(h) = &ghost.hint {
                ghost.fixes = fixes(conn, &ref_key, &ghost.name, &h.id)?;
            }
        }
        out.push(ghost);
    }
    Ok(Ghosts {
        link: link.map(str::to_string),
        ghosts: out,
    })
}

/// The `<link>` argument: matched against the link key, against the name the summary shows, and against any spelling
/// a note wrote, so text copied from either place finds its ghost.
struct Wanted {
    lower: String,
    key: Option<String>,
    as_written: Vec<String>,
}

impl Wanted {
    fn new(conn: &Connection, link: &str) -> Result<Wanted> {
        let link = link.trim();
        let mut stmt =
            conn.prepare("SELECT DISTINCT ref_key FROM edges WHERE target_key IS NULL AND target_ref = ?1")?;
        let as_written = stmt
            .query_map([link], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Wanted {
            lower: link.to_lowercase(),
            key: link_key("", crate::parse::LinkSyntax::Wiki, link),
            as_written,
        })
    }

    fn matches(&self, name: &str, ref_key: &str) -> bool {
        self.key.as_deref() == Some(ref_key)
            || name.to_lowercase() == self.lower
            || self.as_written.iter().any(|k| k == ref_key)
    }
}

/// Every note the link matches, when it matches several from some linking note's namespace.
fn ambiguous_notes(conn: &Connection, ref_key: &str) -> Result<Option<Vec<String>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT n.namespace FROM edges e JOIN notes n ON n.key = e.source_key \
         WHERE e.target_key IS NULL AND e.ref_key = ?1",
    )?;
    let namespaces = stmt
        .query_map([ref_key], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ids: Vec<String> = Vec::new();
    for ns in namespaces {
        if let Resolution::Ambiguous(found) = resolve(conn, &ns, ref_key)? {
            ids.extend(found);
        }
    }
    ids.sort_by_key(|id| id.to_lowercase());
    ids.dedup();
    Ok((!ids.is_empty()).then_some(ids))
}

/// Doc 05 §3.5: the last part of the link key, after the last `/` or `::`, looked up in `notes.name_key`. A hint
/// only when exactly one note matches.
fn hint(conn: &Connection, ref_key: &str) -> Result<Option<Hint>> {
    let after_slash = ref_key.rsplit('/').next().unwrap_or(ref_key);
    let last = after_slash.rsplit("::").next().unwrap_or(after_slash);
    let mut stmt = conn.prepare_cached("SELECT path, title, title_key, name_key FROM notes WHERE name_key = ?1")?;
    let rows = stmt
        .query_map([last], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(match rows.as_slice() {
        [(path, title, title_key, name_key)] => Some(Hint {
            id: note_id(path).to_string(),
            title: (title_key != name_key).then(|| title.clone()),
        }),
        _ => None,
    })
}

/// A linking note's row: id, title shown, namespace, predicate, link as written, line.
struct SourceRow {
    source: Source,
    /// The namespace folder as the note's path spells it.
    folder: String,
}

fn source_rows(conn: &Connection, ref_key: &str) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT n.path, n.title, n.title_key, n.name_key, e.predicate, e.target_ref, e.position \
         FROM edges e JOIN notes n ON n.key = e.source_key WHERE e.target_key IS NULL AND e.ref_key = ?1",
    )?;
    let mut rows = stmt
        .query_map(params![ref_key], |r| {
            let path: String = r.get(0)?;
            let folder = path.split('/').next().unwrap_or_default().to_string();
            let title: String = r.get(1)?;
            let title_key: String = r.get(2)?;
            let name_key: String = r.get(3)?;
            Ok(SourceRow {
                source: Source {
                    id: note_id(&path).to_string(),
                    title: (title_key != name_key).then_some(title),
                    predicate: r.get(4)?,
                    target_ref: r.get(5)?,
                    line: r.get(6)?,
                },
                folder,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // As `move_note` and `delete_note` sort their lists (doc 07 §10): linking note id ignoring case, then line,
    // predicate and link as written.
    rows.sort_by_cached_key(|r| {
        (
            r.source.id.to_lowercase(),
            r.source.line,
            r.source.predicate.clone(),
            r.source.target_ref.clone(),
        )
    });
    Ok(rows)
}

fn sources(conn: &Connection, ref_key: &str) -> Result<Vec<Source>> {
    Ok(source_rows(conn, ref_key)?.into_iter().map(|r| r.source).collect())
}

/// The id a note had when a link, written in a note whose namespace folder is `folder`, reached it: `relink`'s `old`.
/// It is spelled as the link writes it (doc 07 §12.3, "Spelling of `old`"): `name` is the spelling the summary line
/// shows. A relative or Markdown link has only its key, so its `old` is the key without the leading `/`. `None` for a
/// bare name, which does not say where the note was, and for an id `relink` would refuse.
fn old_id(ref_key: &str, name: &str, folder: &str) -> Option<String> {
    let old = if let Some(full) = ref_key.strip_prefix('/') {
        full.to_string()
    } else {
        let written = strip_md_ignore_case(name).unwrap_or(name);
        match written.split_once("::") {
            Some((ns, rest)) => format!("{ns}/{rest}"),
            None if written.contains('/') => format!("{folder}/{written}"),
            None => return None,
        }
    };
    (old.contains('/') && !old.contains("::")).then_some(old)
}

/// One `relink` call per `old`, naming every linking note that reaches the ghost by it.
fn fixes(conn: &Connection, ref_key: &str, name: &str, new: &str) -> Result<Vec<Fix>> {
    let mut by_old: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in source_rows(conn, ref_key)? {
        let Some(old) = old_id(ref_key, name, &row.folder) else {
            continue;
        };
        let notes = by_old.entry(old).or_default();
        if !notes.contains(&row.source.id) {
            notes.push(row.source.id);
        }
    }
    Ok(by_old
        .into_iter()
        .map(|(old, notes)| Fix {
            old,
            new: new.to_string(),
            notes,
        })
        .collect())
}

fn links_text(n: usize) -> String {
    if n == 1 {
        "1 link".to_string()
    } else {
        format!("{n} links")
    }
}

/// `vault` is the `--vault` the user gave, repeated in each `relink` command so it works as printed (doc 07 §12.1).
pub fn render(g: &Ghosts, vault: Option<&Path>) -> String {
    if g.ghosts.is_empty() {
        return match &g.link {
            Some(link) => format!("No ghost matches \"{link}\".\n"),
            None => "No ghosts.\n".to_string(),
        };
    }
    // The summary lines its columns up across every row; each block of the detail stands alone, so it lines up its own.
    let widths = |rows: &[Ghost]| {
        (
            rows.iter().map(|x| x.name.chars().count()).max().unwrap_or(0),
            rows.iter()
                .map(|x| links_text(x.links).chars().count())
                .max()
                .unwrap_or(0),
        )
    };
    let all = widths(&g.ghosts);
    let mut out = String::new();
    for (i, ghost) in g.ghosts.iter().enumerate() {
        if g.link.is_some() && i > 0 {
            out.push('\n');
        }
        let note = match (&ghost.hint, &ghost.ambiguous) {
            (Some(h), _) => format!("   \u{2192} maybe moved to {}", titled_id(&h.id, h.title.as_deref())),
            (None, Some(_)) => "   (ambiguous)".to_string(),
            (None, None) => String::new(),
        };
        let (name_width, links_width) = if g.link.is_some() {
            widths(std::slice::from_ref(ghost))
        } else {
            all
        };
        let row = format!(
            "{:<name_width$}   {:<links_width$}{note}",
            ghost.name,
            links_text(ghost.links)
        );
        out.push_str(row.trim_end());
        out.push('\n');
        if g.link.is_none() {
            continue;
        }
        if let Some(ids) = &ghost.ambiguous {
            out.push_str(&format!("matches {} notes: {}\n", ids.len(), ids.join(", ")));
        }
        for s in &ghost.sources {
            out.push_str(&format!(
                "- {}: line {}: {} {}\n",
                titled_id(&s.id, s.title.as_deref()),
                s.line,
                s.predicate,
                s.target_ref
            ));
        }
        for fix in &ghost.fixes {
            out.push_str(&finish_command(&fix.old, &fix.new, &fix.notes, vault));
            out.push('\n');
        }
    }
    out
}

// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `context(note, page?)` (doc 07 §8): what surrounds one note, one level deep, without reading its neighbors.
//!
//! Links out come from the query of doc 05 §3.3, links in from §3.4. Bootstrap notes are never listed and archive
//! notes carry their marker (doc 07 §1.1).

use std::collections::HashSet;

use rusqlite::{Connection, params};

use crate::bootstrap::estimate_tokens;
use crate::error::{Error, Result};
use crate::paths::{ARCHIVE_NAMESPACE, note_id, titled_id};
use crate::resolve::{NoteLookup, find_note, several_text};

/// Most links on one page, links out and links in counted together (doc 07 §8, "Limit"). A tuning parameter, set
/// with the retrieval benchmark.
pub const PAGE_SIZE: usize = 20;
/// A page stays at or under this many tokens, estimated as characters ÷ 4 (doc 07 §8, "Limit"). A tuning parameter,
/// set with the retrieval benchmark.
pub const PAGE_TOKEN_BUDGET: usize = 1000;

/// The two limits of a page (doc 07 §8, "Limit").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub links: usize,
    pub tokens: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        links: PAGE_SIZE,
        tokens: PAGE_TOKEN_BUDGET,
    };
}

const OUT_HEADING: &str = "\nLinks out:\n";
const IN_HEADING: &str = "\nLinks in:\n";

/// A note as a context line shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRef {
    /// The id as written (doc 03 §4).
    pub id: String,
    /// Set only when the title differs from the filename by key (doc 07 §4.2).
    pub title: Option<String>,
    pub summary: Option<String>,
    /// The note is in the archive: its line carries `(archive)`.
    pub archive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Note(NoteRef),
    /// A link to a note that does not exist yet: the link exactly as written (doc 05 §2.2, `target_ref`).
    Ghost(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOut {
    /// As written.
    pub predicate: String,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkIn {
    /// As written in the linking note.
    pub predicate: String,
    pub source: NoteRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPage {
    /// The note itself, for the header line.
    pub note: NoteRef,
    /// The links out on this page, in written order.
    pub out: Vec<LinkOut>,
    /// The links in on this page, most recently changed first.
    pub incoming: Vec<LinkIn>,
    /// 1-based page number.
    pub page: usize,
    /// Links after this page.
    pub more: usize,
    /// Links over all pages.
    pub total: usize,
    /// Pages over all links.
    pub pages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Context {
    Page(ContextPage),
    /// The name matched several notes (doc 07 §6.3): the tool runs on none of them.
    Several {
        name: String,
        ids: Vec<String>,
    },
}

/// Runs `context` on the note `name` (an id, path or link name, doc 07 §6.3). `page` is 1-based.
pub fn context(conn: &Connection, name: &str, page: usize) -> Result<Context> {
    context_with_limits(conn, name, page, Limits::DEFAULT)
}

/// [`context`] with other page limits, for tests of the paging rules.
pub fn context_with_limits(conn: &Connection, name: &str, page: usize, limits: Limits) -> Result<Context> {
    let key = match find_note(conn, name)? {
        NoteLookup::One(key) => key,
        NoteLookup::Several(ids) => {
            return Ok(Context::Several {
                name: name.to_string(),
                ids,
            });
        }
        NoteLookup::None => return Err(Error::NoteNotFound(name.to_string())),
    };
    let note = note_ref(conn, &key)?.ok_or_else(|| Error::NoteNotFound(name.to_string()))?;
    let out = links_out(conn, &key)?;
    let incoming = links_in(conn, &key, &out)?;

    let out_len = out.len();
    let total = out_len + incoming.len();
    let blocks: Vec<usize> = out
        .iter()
        .map(|o| out_block(&o.link))
        .chain(incoming.iter().map(in_block))
        .map(|b| b.chars().count())
        .collect();
    let starts = page_starts(header(&note).chars().count(), &blocks, out_len, limits);
    let pages = starts.len();
    // A page past the end holds no links; the render prints its one line.
    let (start, end) = match starts.get(page.wrapping_sub(1)) {
        Some(&start) => (start, starts.get(page).copied().unwrap_or(total)),
        None => (total, total),
    };
    let out_page: Vec<LinkOut> = out
        .into_iter()
        .skip(start)
        .take(end.min(out_len).saturating_sub(start))
        .map(|o| o.link)
        .collect();
    let in_page: Vec<LinkIn> = incoming
        .into_iter()
        .skip(start.saturating_sub(out_len))
        .take(end.saturating_sub(start.max(out_len)))
        .collect();
    Ok(Context::Page(ContextPage {
        note,
        out: out_page,
        incoming: in_page,
        page,
        more: total - end,
        total,
        pages,
    }))
}

/// Where each page starts, as an index over links out followed by links in (doc 07 §8, "Limit"). A page takes links
/// while it holds fewer than `limits.links` and its text, closing line included, stays within `limits.tokens`. A link
/// is never cut: one that does not fit starts the next page, and every page holds at least one link.
///
/// `header` is the length of the note's id line and `blocks` the length of each link's lines, in characters.
fn page_starts(header: usize, blocks: &[usize], out_len: usize, limits: Limits) -> Vec<usize> {
    let total = blocks.len();
    let mut starts = Vec::new();
    let mut start = 0;
    while start < total {
        starts.push(start);
        let next_page = starts.len() + 1;
        let mut used = header;
        let mut i = start;
        while i < total && i - start < limits.links.max(1) {
            let heading = if i < out_len {
                if i == start { OUT_HEADING.len() } else { 0 }
            } else if i == start || i == out_len {
                IN_HEADING.len()
            } else {
                0
            };
            let left = total - (i + 1);
            let closing = if left > 0 {
                more_line(left, next_page).chars().count()
            } else {
                0
            };
            let size = used + heading + blocks[i];
            if i > start && estimate_tokens(size + closing) > limits.tokens {
                break;
            }
            used = size;
            i += 1;
        }
        start = i;
    }
    starts
}

/// A note that links here, with its key.
struct Row {
    key: String,
    note: NoteRef,
}

/// The note `key` as the header line shows it.
fn note_ref(conn: &Connection, key: &str) -> Result<Option<NoteRef>> {
    let mut stmt =
        conn.prepare_cached("SELECT path, namespace, title, title_key, name_key, summary FROM notes WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    let path: String = r.get(0)?;
    let namespace: String = r.get(1)?;
    let title: String = r.get(2)?;
    let title_key: String = r.get(3)?;
    let name_key: String = r.get(4)?;
    let summary: Option<String> = r.get(5)?;
    Ok(Some(make_ref(&path, &namespace, title, &title_key, &name_key, summary)))
}

fn make_ref(
    path: &str,
    namespace: &str,
    title: String,
    title_key: &str,
    name_key: &str,
    summary: Option<String>,
) -> NoteRef {
    NoteRef {
        id: note_id(path).to_string(),
        title: (title_key != name_key).then_some(title),
        summary: summary.filter(|s| !s.trim().is_empty()),
        archive: namespace == ARCHIVE_NAMESPACE,
    }
}

/// Doc 05 §3.3, with the target key and name keys added for the inverse-pair check and the title rule. The extra
/// order terms only settle ties between links on one line.
const LINKS_OUT_SQL: &str = "\
SELECT e.predicate, e.target_ref, e.target_key, n.path, n.namespace, n.title, n.title_key, n.name_key, n.summary
FROM edges e
LEFT JOIN notes n ON e.target_key = n.key
WHERE e.source_key = ?1
  AND (n.namespace IS NULL OR n.namespace != 'bootstrap')
ORDER BY e.position, e.is_inline, e.predicate, e.ref_key";

/// Doc 05 §3.4, with the source key added for the inverse-pair check. The extra order terms only settle ties between
/// notes changed in the same millisecond, and between links from one note.
const LINKS_IN_SQL: &str = "\
SELECT e.predicate, e.source_key, n.path, n.namespace, n.title, n.title_key, n.name_key, n.summary
FROM edges e
JOIN notes n ON e.source_key = n.key
WHERE e.target_key = ?1
  AND n.namespace != 'bootstrap'
ORDER BY n.mtime DESC, n.key, e.position, e.predicate";

/// A link out, with the key of its target (`None` for a ghost).
struct Out {
    link: LinkOut,
    target_key: Option<String>,
}

fn links_out(conn: &Connection, key: &str) -> Result<Vec<Out>> {
    let mut stmt = conn.prepare_cached(LINKS_OUT_SQL)?;
    let rows = stmt.query_map(params![key], |r| {
        let predicate: String = r.get(0)?;
        let target_ref: String = r.get(1)?;
        let target_key: Option<String> = r.get(2)?;
        let path: Option<String> = r.get(3)?;
        let target = match path {
            Some(path) => {
                let namespace: String = r.get(4)?;
                let title: String = r.get(5)?;
                let title_key: String = r.get(6)?;
                let name_key: String = r.get(7)?;
                let summary: Option<String> = r.get(8)?;
                Target::Note(make_ref(&path, &namespace, title, &title_key, &name_key, summary))
            }
            None => Target::Ghost(target_ref),
        };
        let target_key = matches!(target, Target::Note(_)).then_some(target_key).flatten();
        Ok(Out {
            link: LinkOut { predicate, target },
            target_key,
        })
    })?;
    // One fact, one line (doc 07 §8): two spellings of one link that resolve to the same note, with the same
    // predicate, are one fact. The first in written order stays.
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if let Some(target) = &row.target_key
            && !seen.insert((row.link.predicate.to_lowercase(), target.clone()))
        {
            continue;
        }
        out.push(row);
    }
    Ok(out)
}

/// The predicate that states the same fact from the other end (doc 03 §3, "Hierarchical").
fn inverse(predicate: &str) -> Option<&'static str> {
    match predicate.to_lowercase().as_str() {
        "has_part" => Some("part_of"),
        "part_of" => Some("has_part"),
        _ => None,
    }
}

fn links_in(conn: &Connection, key: &str, out: &[Out]) -> Result<Vec<LinkIn>> {
    // Facts the links out already show: `(source of the link in, its predicate)` for each inverse pair.
    let shown: HashSet<(String, &str)> = out
        .iter()
        .filter_map(|o| {
            let target = o.target_key.as_ref()?;
            Some((target.clone(), inverse(&o.link.predicate)?))
        })
        .collect();
    let mut stmt = conn.prepare_cached(LINKS_IN_SQL)?;
    let rows = stmt.query_map(params![key], |r| {
        let predicate: String = r.get(0)?;
        let source_key: String = r.get(1)?;
        let path: String = r.get(2)?;
        let namespace: String = r.get(3)?;
        let title: String = r.get(4)?;
        let title_key: String = r.get(5)?;
        let name_key: String = r.get(6)?;
        let summary: Option<String> = r.get(7)?;
        Ok((
            predicate,
            Row {
                key: source_key,
                note: make_ref(&path, &namespace, title, &title_key, &name_key, summary),
            },
        ))
    })?;
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut incoming = Vec::new();
    for row in rows {
        let (predicate, source) = row?;
        let lower = predicate.to_lowercase();
        // A link from the note to itself is one fact, already shown under links out.
        if source.key == key {
            continue;
        }
        // One fact, one line (doc 07 §8): `B part_of A` in B and `A has_part B` here are one fact.
        if shown.contains(&(source.key.clone(), lower.as_str())) {
            continue;
        }
        // As for links out: two spellings of one link from the same note are one fact.
        if !seen.insert((source.key.clone(), lower)) {
            continue;
        }
        incoming.push(LinkIn {
            predicate,
            source: source.note,
        });
    }
    Ok(incoming)
}

/// The text of doc 07 §8, or the list of doc 07 §6.3 when the name matched several notes.
pub fn render(context: &Context) -> String {
    match context {
        Context::Several { name, ids } => several_text(name, ids),
        Context::Page(page) => render_page(page),
    }
}

fn render_page(page: &ContextPage) -> String {
    if page.total == 0 {
        // Doc 07 §8, "No links": one line, on every page number.
        return format!(
            "No links to or from {}.\n",
            titled_id(&page.note.id, page.note.title.as_deref())
        );
    }
    let mut text = header(&page.note);
    if page.out.is_empty() && page.incoming.is_empty() {
        // Doc 07 §8, "Closing lines": search's past-the-end line (doc 07 §4.2), counting links.
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        return format!(
            "Page {} is past the end: {} on {}.\n",
            page.page,
            plural(page.total, "link"),
            plural(page.pages, "page")
        );
    }
    if !page.out.is_empty() {
        text.push_str(OUT_HEADING);
        for link in &page.out {
            text.push_str(&out_block(link));
        }
    }
    if !page.incoming.is_empty() {
        text.push_str(IN_HEADING);
        for link in &page.incoming {
            text.push_str(&in_block(link));
        }
    }
    if page.more > 0 {
        text.push_str(&more_line(page.more, page.page + 1));
    }
    text
}

/// The id line that starts every page.
fn header(note: &NoteRef) -> String {
    format!("{}\n", titled_id(&note.id, note.title.as_deref()))
}

/// Doc 07 §8, "Closing lines": search's "more" line (doc 07 §4.2).
fn more_line(more: usize, next_page: usize) -> String {
    format!("{more} more: page {next_page}\n")
}

/// The lines of one link out: the link, then its summary when it has one.
fn out_block(link: &LinkOut) -> String {
    match &link.target {
        Target::Note(note) => note_block(&format!("- {}: ", link.predicate), note),
        Target::Ghost(written) => format!("- {}: {written}  (not written yet)\n", link.predicate),
    }
}

/// The lines of one link in: the link, then the linking note's summary when it has one.
fn in_block(link: &LinkIn) -> String {
    note_block(&format!("- {}, from ", link.predicate), &link.source)
}

fn note_block(lead: &str, note: &NoteRef) -> String {
    let mut text = format!("{lead}{}", titled_id(&note.id, note.title.as_deref()));
    if note.archive {
        text.push_str("  (archive)");
    }
    text.push('\n');
    if let Some(summary) = &note.summary {
        text.push_str(&format!("  {summary}\n"));
    }
    text
}

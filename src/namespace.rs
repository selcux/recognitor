// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `load_namespace` (doc 07 §3, doc 05 §3.2): a namespace's entry note, or its note list when it has none.
//!
//! Doc 07 §3 is the spec.

use rusqlite::{Connection, OptionalExtension, params};

use crate::bootstrap::{trim_blank_edges, without_frontmatter};
use crate::error::{Error, Result};
use crate::index::{Index, SweepOptions};
use crate::paths::{BOOTSTRAP_NAMESPACE, note_id, titled_id};

/// Notes listed for a namespace without an index note (doc 07 §3). Not the page size of `search`.
pub const LIST_SIZE: usize = 10;

/// The line the tool adds after an entry note (doc 07 §3, "Output"). `ns` is spelled as the folder is on disk
/// (doc 07 §3, "Spelling in the line").
fn pass_line(ns: &str) -> String {
    format!("Pass ns: \"{ns}\" to search.")
}

/// The namespace as its folder is spelled on disk: the first segment of a note's path.
fn folder_spelling(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

/// The first line when the namespace has no entry note (doc 07 §3, "No `index.md`").
fn no_index_line(ns: &str) -> String {
    format!("No index.md in \"{ns}\". Its notes:")
}

/// The text of `load_namespace(ns)`, without the sweep.
///
/// Fails with [`Error::BootstrapNotANamespace`] for `bootstrap` (doc 07 §1.1: refused, and the error points to
/// `load_bootstrap`).
pub fn load(conn: &Connection, ns: &str) -> Result<String> {
    // Doc 07 §3, "No nested namespaces": a namespace is a top-level folder, so a name with a `/` is no namespace.
    // This runs before the doc 05 §3.2 query, which would match a nested `<ns>/index` note.
    if ns.contains('/') {
        return Err(Error::NoNamespace {
            name: ns.to_string(),
            namespaces: namespaces(conn)?,
        });
    }
    // Names are matched by lowercase key (doc 05 §3.2: "?1 is the namespace key").
    let key = ns.to_lowercase();
    if key == BOOTSTRAP_NAMESPACE {
        return Err(Error::BootstrapNotANamespace);
    }
    let entry: Option<(String, String, String)> = conn
        .query_row(
            "SELECT title, content, path FROM notes WHERE key = ?1 || '/index'",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((title, content, path)) = entry {
        // Doc 07 §3 "Output": the file as written without its frontmatter, `## Relations` kept, `# <title>` added
        // when it does not start with a level-1 heading line (as in the bootstrap payload, doc 04 §2.2 step 5),
        // then a blank line and the hint. Blank lines around the text are dropped, as the bootstrap payload does.
        let body = trim_blank_edges(without_frontmatter(&content));
        let note = if body.is_empty() {
            format!("# {title}")
        } else if body.starts_with("# ") {
            body.to_string()
        } else {
            format!("# {title}\n\n{body}")
        };
        return Ok(format!("{note}\n\n{}\n", pass_line(folder_spelling(&path))));
    }
    // Doc 07 §3, "No index.md": a normal result. At most one search page of notes (doc 07 §4.2), sorted by id
    // ignoring case (keys are lowercase ids), a closing line when more exist.
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM notes WHERE namespace = ?1", params![key], |row| {
        row.get(0)
    })?;
    let total = usize::try_from(total).unwrap_or(0);
    // Doc 07 §3, "No such namespace": no note in `<ns>` is an error that lists the namespaces that exist.
    if total == 0 {
        return Err(Error::NoNamespace {
            name: ns.to_string(),
            namespaces: namespaces(conn)?,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT path, title, title_key, name_key, summary FROM notes WHERE namespace = ?1 ORDER BY key LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![key, LIST_SIZE as i64], |row| {
        let path: String = row.get(0)?;
        let title: String = row.get(1)?;
        let title_key: String = row.get(2)?;
        let name_key: String = row.get(3)?;
        let summary: Option<String> = row.get(4)?;
        Ok((path, title, title_key == name_key, summary))
    })?;
    let mut list = String::new();
    // The folder's spelling, from the first note: every note of a namespace is under the same folder.
    let mut spelled = None;
    for row in rows {
        let (path, title, same, summary) = row?;
        spelled.get_or_insert_with(|| folder_spelling(&path).to_string());
        list.push_str(&format!(
            "- {}\n",
            titled_id(note_id(&path), (!same).then_some(title.as_str()))
        ));
        if let Some(summary) = summary {
            list.push_str(&format!("  {summary}\n"));
        }
    }
    let spelled = spelled.unwrap_or_else(|| ns.to_string());
    let mut out = format!("{}\n{list}", no_index_line(&spelled));
    if total > LIST_SIZE {
        out.push_str(&format!(
            "{} more. Search with ns: \"{spelled}\" to find them.\n",
            total - LIST_SIZE
        ));
    }
    Ok(out)
}

/// Every namespace but the bootstrap, each spelled as its folder is on disk, sorted by name ignoring case (doc 07 §3,
/// "No such namespace"). Keys are lowercase, so the key order is the order ignoring case.
fn namespaces(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT MIN(path) FROM notes WHERE namespace != ?1 GROUP BY namespace ORDER BY namespace")?;
    let rows = stmt.query_map(params![BOOTSTRAP_NAMESPACE], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for path in rows {
        out.push(folder_spelling(&path?).to_string());
    }
    Ok(out)
}

impl Index {
    /// `load_namespace`: the stale-gated sweep (doc 02 §4), then the entry note of the namespace (doc 07 §3).
    pub fn load_namespace(&mut self, ns: &str) -> Result<String> {
        self.sync_if_stale(SweepOptions::default())?;
        load(self.connection(), ns)
    }
}

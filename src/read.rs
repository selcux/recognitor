// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `read_note(note, section?, query?)` (doc 07 §7): one note, the whole file exactly as on disk, under a header line
//! with its short checksum (§7.1); a long note cut to the limit (§7.2); or one section (§7.3).

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::{Connection, params};

use crate::bootstrap::estimate_tokens;
use crate::error::{Error, Result};
use crate::hash::sha256_prefix;
use crate::outline::{Outline, Section, outline};
use crate::parse::parse_note;
use crate::paths::{file_stem, note_id, titled_id};
use crate::resolve::{NoteLookup, find_note, several_text};
use crate::search::{expression_of, query_parts};

/// Default of `read.max_tokens` (doc 07 §7.2).
pub const DEFAULT_MAX_TOKENS: usize = 4000;
/// Hex characters of the checksum a tool shows and accepts (doc 07 §7.1).
pub const CHECKSUM_LEN: usize = 12;
/// Between a section's path parts (doc 07 §7.3).
const PATH_SEPARATOR: &str = " > ";

/// What a read returns, as the text under the header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteText {
    /// The id as written (doc 03 §4).
    pub id: String,
    /// Set only when the title differs from the filename by key (doc 07 §4.2).
    pub title: Option<String>,
    /// The section's path (`Storage > Write lock`) when one section was asked for.
    pub section: Option<String>,
    /// The first 12 hex characters of the SHA-256 of the file (or of the section's bytes). A cut note read carries the
    /// whole-note checksum too (doc 07 §7.2); only a cut section read has none, since its bytes are not all shown.
    pub checksum: Option<String>,
    /// The text under the header line. A cut read ends with its closing line.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// The note, or the section, whole.
    Whole(NoteText),
    /// The note, or the section, cut to the limit (doc 07 §7.2).
    Cut(NoteText),
    /// The name matched several notes (doc 07 §6.3): the tool runs on none of them.
    Several { name: String, ids: Vec<String> },
    /// `section` matched several headings (doc 07 §7.3): no section is returned, their paths are.
    Sections {
        /// The note's id, as in doc 07 §4.2.
        note: String,
        name: String,
        paths: Vec<String>,
    },
}

/// Reads the note `name` (an id, path or link name, doc 07 §6.3) from its file under `vault_root`. The index gives
/// the path and the title; the content and the checksums come from the file itself, so an edit made after the last
/// sweep shows. `section` is a heading (§7.3), `query` guides a cut (§7.2), `max_tokens` is `read.max_tokens`.
pub fn read(
    conn: &Connection,
    vault_root: &Path,
    name: &str,
    section: Option<&str>,
    query: Option<&str>,
    max_tokens: usize,
) -> Result<Read> {
    let key = match find_note(conn, name)? {
        NoteLookup::One(key) => key,
        NoteLookup::Several(ids) => {
            return Ok(Read::Several {
                name: name.to_string(),
                ids,
            });
        }
        NoteLookup::None => return Err(Error::NoteNotFound(name.to_string())),
    };
    let (path, title, title_key, name_key): (String, String, String, String) = conn
        .prepare_cached("SELECT path, title, title_key, name_key FROM notes WHERE key = ?1")?
        .query_row(params![key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let file = vault_root.join(&path);
    let bytes = match fs::read(&file) {
        Ok(b) => b,
        // The index knows a note the disk no longer has: to the caller it does not exist (doc 07 §6).
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::NoteNotFound(name.to_string())),
        Err(e) => return Err(Error::io(file, e)),
    };
    let id = note_id(&path).to_string();
    let title = (title_key != name_key).then_some(title);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let ctx = Ctx {
        id,
        title,
        text: &text,
        limit: max_tokens,
        query,
    };
    let outline = outline(&text);
    match section {
        None => Ok(ctx.note(&bytes, &outline)),
        Some(wanted) => ctx.section(&outline, wanted),
    }
}

/// True when `read_note` without `section` would cut the note whose file holds `bytes`: it is over the read limit
/// (doc 07 §7.2). The write tools ask this to decide what a long note allows (doc 07 §11), so they cannot disagree
/// with the read.
pub(crate) fn is_cut(rel: &str, bytes: &[u8], max_tokens: usize) -> bool {
    matches!(read_file(rel, bytes, max_tokens), Read::Cut(_))
}

/// A note read from the bytes of its file, with no index lookup: what a rejected write returns as the current version
/// (doc 07 §5.3). The file may be one the index does not know yet, so the title comes from the text, as the indexer
/// takes it (the first heading, else the filename). `rel` is the vault-relative path as named on disk.
pub(crate) fn read_file(rel: &str, bytes: &[u8], max_tokens: usize) -> Read {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let ctx = file_ctx(rel, &text, max_tokens);
    ctx.note(bytes, &outline(&text))
}

/// One section of a note read from the bytes of its file, as `read_note` returns it (doc 07 §7.3): what a rejected
/// `replace_section` returns (doc 07 §5.3). Fails like `read_note` when `wanted` names no heading.
pub(crate) fn read_file_section(rel: &str, bytes: &[u8], wanted: &str, max_tokens: usize) -> Result<Read> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let ctx = file_ctx(rel, &text, max_tokens);
    ctx.section(&outline(&text), wanted)
}

/// The note's id as every output shows it (doc 07 §4.2): the id, and the title when it differs from the filename.
pub(crate) fn titled_id_of(rel: &str, text: &str) -> String {
    file_ctx(rel, text, 0).titled()
}

/// The sections `wanted` names, by the matching of doc 07 §7.3.
pub(crate) enum Picked<'a> {
    /// One heading, as the chain of sections from a top-level one down to it.
    One(Vec<&'a Section>),
    /// Several headings: their paths, for the agent to call again with one.
    Several(Vec<String>),
}

/// Finds the heading `wanted` names in `outline` (doc 07 §7.3, "Matching"). `titled` is the note's id as doc 07 §4.2
/// shows it. No match is the error that lists the note's top-level sections.
pub(crate) fn pick_section<'a>(outline: &'a Outline, titled: &str, wanted: &str) -> Result<Picked<'a>> {
    let mut found = find_sections(&outline.sections, wanted);
    match found.len() {
        0 => Err(Error::NoSection {
            note: titled.to_string(),
            name: wanted.to_string(),
            sections: outline.sections.iter().map(|s| s.display.clone()).collect(),
        }),
        1 => Ok(Picked::One(found.remove(0))),
        _ => Ok(Picked::Several(found.iter().map(|c| path_of(c)).collect())),
    }
}

/// The text of a call whose `section` matched several headings (doc 07 §7.3), for a tool that names one.
pub(crate) fn sections_text(titled: &str, wanted: &str, paths: &[String]) -> String {
    render(&Read::Sections {
        note: titled.to_string(),
        name: wanted.to_string(),
        paths: paths.to_vec(),
    })
}

/// The path of a picked section as `section` takes it back (`Storage > Write lock`).
pub(crate) fn section_path(chain: &[&Section]) -> String {
    path_of(chain)
}

/// The first 12 hex characters of the checksum of a section's bytes (doc 07 §7.3).
pub(crate) fn section_checksum(text: &str, section: &Section) -> String {
    sha256_prefix(&text.as_bytes()[section.start..section.end], CHECKSUM_LEN)
}

fn file_ctx<'a>(rel: &str, text: &'a str, max_tokens: usize) -> Ctx<'a> {
    let id = note_id(rel).to_string();
    let stem = file_stem(&id).to_lowercase();
    let title = parse_note(text).title.filter(|t| t.to_lowercase() != stem);
    Ctx {
        id,
        title,
        text,
        limit: max_tokens,
        query: None,
    }
}

struct Ctx<'a> {
    id: String,
    title: Option<String>,
    text: &'a str,
    limit: usize,
    query: Option<&'a str>,
}

impl Ctx<'_> {
    fn titled(&self) -> String {
        titled_id(&self.id, self.title.as_deref())
    }

    fn note(&self, bytes: &[u8], outline: &Outline) -> Read {
        let checksum = sha256_prefix(bytes, CHECKSUM_LEN);
        let header = header_line(&self.titled(), Some(&checksum));
        if fits(header.chars().count() + self.text.chars().count(), self.limit) {
            return Read::Whole(self.result(None, Some(checksum), self.text.to_string()));
        }
        let container = Container {
            header: self.titled(),
            checksum: Some(checksum.clone()),
            text: self.text,
            fixed: 0..outline.fm_end,
            head: outline.fm_end..outline.head_end,
            block_starts: &outline.block_starts,
            children: outline
                .sections
                .iter()
                .map(|s| Child::new(self.text, s.display.clone(), s))
                .collect(),
            limit: self.limit,
        };
        // Doc 07 §7.2: the header line carries the whole-note checksum on a cut read too.
        Read::Cut(self.result(None, Some(checksum), container.cut(self.query)))
    }

    fn section(&self, outline: &Outline, wanted: &str) -> Result<Read> {
        let mut found = find_sections(&outline.sections, wanted);
        if found.len() > 1 {
            return Ok(Read::Sections {
                note: self.titled(),
                name: wanted.to_string(),
                paths: found.iter().map(|chain| path_of(chain)).collect(),
            });
        }
        let Some(chain) = found.pop() else {
            return Err(Error::NoSection {
                note: self.titled(),
                name: wanted.to_string(),
                sections: outline.sections.iter().map(|s| s.display.clone()).collect(),
            });
        };
        let path = path_of(&chain);
        let Some(section) = chain.last().copied() else {
            return Err(Error::NoteNotFound(wanted.to_string()));
        };
        let body = &self.text[section.start..section.end];
        let checksum = sha256_prefix(body.as_bytes(), CHECKSUM_LEN);
        let header = format!("{}{PATH_SEPARATOR}{path}", self.titled());
        if fits(
            header_line(&header, Some(&checksum)).chars().count() + body.chars().count(),
            self.limit,
        ) {
            return Ok(Read::Whole(self.result(Some(path), Some(checksum), body.to_string())));
        }
        let container = Container {
            header,
            checksum: None,
            text: self.text,
            fixed: section.start..section.start,
            head: section.start..section.head_end(),
            block_starts: &outline.block_starts,
            children: section
                .children
                .iter()
                .map(|c| Child::new(self.text, format!("{path}{PATH_SEPARATOR}{}", c.display), c))
                .collect(),
            limit: self.limit,
        };
        Ok(Read::Cut(self.result(Some(path), None, container.cut(self.query))))
    }

    fn result(&self, section: Option<String>, checksum: Option<String>, text: String) -> NoteText {
        NoteText {
            id: self.id.clone(),
            title: self.title.clone(),
            section,
            checksum,
            text,
        }
    }
}

/// `<id>` or `<id>  checksum: <12 hex>`, with its line break.
fn header_line(head: &str, checksum: Option<&str>) -> String {
    match checksum {
        Some(sum) => format!("{head}  checksum: {sum}\n"),
        None => format!("{head}\n"),
    }
}

/// True when `chars` characters, estimated as characters ÷ 4, stay within `limit` tokens.
fn fits(chars: usize, limit: usize) -> bool {
    estimate_tokens(chars) <= limit
}

/// A heading as `section` names it (doc 07 §7.3): lowercase, without its `#` marks.
fn normalized(heading: &str) -> String {
    heading.trim().trim_start_matches('#').trim().to_lowercase()
}

/// The headings `wanted` names (doc 07 §7.3), each as the chain of sections from a top-level one down to it. A plain
/// name matches a heading at any level; a path joined by ` > ` starts at a top-level section and goes down through
/// subsections.
fn find_sections<'a>(top: &'a [Section], wanted: &str) -> Vec<Vec<&'a Section>> {
    let parts: Vec<String> = wanted.split(PATH_SEPARATOR).map(normalized).collect();
    let mut found = Vec::new();
    if parts.len() == 1 {
        walk(top, &mut Vec::new(), &mut |chain| {
            if chain.last().is_some_and(|s| names(s, &parts[0])) {
                found.push(chain.to_vec());
            }
        });
    } else {
        let mut chains: Vec<Vec<&Section>> = top.iter().filter(|s| names(s, &parts[0])).map(|s| vec![s]).collect();
        for part in &parts[1..] {
            chains = chains
                .into_iter()
                .flat_map(|chain| {
                    let last = chain.last().copied();
                    last.into_iter()
                        .flat_map(|s| s.children.iter())
                        .filter(|c| names(c, part))
                        .map(|c| {
                            let mut next = chain.clone();
                            next.push(c);
                            next
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
        }
        found = chains;
    }
    found
}

/// True when `part` (already normalized) names `section`: by its heading text, which repeated siblings share, or by
/// its numbered name (doc 07 §7.3).
fn names(section: &Section, part: &str) -> bool {
    normalized(&section.name) == part || normalized(&section.display) == part
}

fn walk<'a>(sections: &'a [Section], chain: &mut Vec<&'a Section>, visit: &mut impl FnMut(&[&'a Section])) {
    for s in sections {
        chain.push(s);
        visit(chain);
        walk(&s.children, chain, visit);
        chain.pop();
    }
}

fn path_of(chain: &[&Section]) -> String {
    chain
        .iter()
        .map(|s| s.display.as_str())
        .collect::<Vec<_>>()
        .join(PATH_SEPARATOR)
}

/// A part of a cut read that is shown or left out as a whole: a section of the note, or a subsection of a section.
struct Child {
    /// The name the closing line shows: a top-level section's heading, or a subsection's path.
    name: String,
    start: usize,
    end: usize,
    checksum: String,
    chars: usize,
    ends_with_newline: bool,
}

impl Child {
    fn new(text: &str, name: String, section: &Section) -> Child {
        let body = &text[section.start..section.end];
        Child {
            name,
            start: section.start,
            end: section.end,
            checksum: sha256_prefix(body.as_bytes(), CHECKSUM_LEN),
            chars: body.chars().count(),
            ends_with_newline: body.ends_with('\n'),
        }
    }
}

/// What is shown so far.
#[derive(Clone)]
struct Plan {
    head_shown: usize,
    shown: Vec<bool>,
}

/// What a cut works on (doc 07 §7.2): the always-kept `fixed` bytes, the `head` in blocks, and the `children` in file
/// order.
struct Container<'a> {
    /// The header line without its checksum.
    header: String,
    /// The checksum the header line carries: the whole note's on a cut note, none on a cut section (doc 07 §7.2,
    /// §7.3). It counts toward the limit, as the whole output does.
    checksum: Option<String>,
    text: &'a str,
    /// Kept whole: the frontmatter of a note, nothing for a section.
    fixed: std::ops::Range<usize>,
    head: std::ops::Range<usize>,
    block_starts: &'a [usize],
    children: Vec<Child>,
    limit: usize,
}

impl Container<'_> {
    /// The head as slices, one per block: each runs from its block's start to the next block's start, the first from
    /// the start of the head and the last to the end of the head, so the slices tile the head.
    fn head_slices(&self) -> Vec<std::ops::Range<usize>> {
        let starts: Vec<usize> = self
            .block_starts
            .iter()
            .copied()
            .filter(|s| *s >= self.head.start && *s < self.head.end)
            .collect();
        if starts.is_empty() {
            return if self.head.is_empty() {
                Vec::new()
            } else {
                vec![self.head.clone()]
            };
        }
        (0..starts.len())
            .map(|i| {
                let from = if i == 0 { self.head.start } else { starts[i] };
                let to = starts.get(i + 1).copied().unwrap_or(self.head.end);
                from..to
            })
            .collect()
    }

    /// The text under the header line of a cut read, closing line included.
    fn cut(&self, query: Option<&str>) -> String {
        let slices = self.head_slices();
        let slice_chars: Vec<usize> = slices.iter().map(|r| self.text[r.clone()].chars().count()).collect();
        let order = self.rank(query);
        let fixed_chars = self.text[self.fixed.clone()].chars().count();
        let header_chars = header_line(&self.header, self.checksum.as_deref()).chars().count();

        let size = |plan: &Plan| -> usize {
            let mut chars = header_chars + fixed_chars;
            chars += slice_chars[..plan.head_shown].iter().sum::<usize>();
            for (c, shown) in self.children.iter().zip(&plan.shown) {
                if *shown {
                    chars += c.chars;
                }
            }
            if self.needs_break(&slices, plan) {
                chars += 1;
            }
            chars + self.closing(&order, plan, slices.len()).chars().count() + 1
        };

        let mut plan = Plan {
            head_shown: 0,
            shown: vec![false; self.children.len()],
        };
        // The head, block by block: the first block that does not fit ends it.
        for i in 0..slices.len() {
            let candidate = Plan {
                head_shown: i + 1,
                ..plan.clone()
            };
            if fits(size(&candidate), self.limit) {
                plan = candidate;
            } else {
                break;
            }
        }
        // The sections, best match first: one that does not fit is skipped and the next one is tried.
        for &i in &order {
            let mut candidate = plan.clone();
            candidate.shown[i] = true;
            if fits(size(&candidate), self.limit) {
                plan = candidate;
            }
        }

        let mut out = String::new();
        out.push_str(&self.text[self.fixed.clone()]);
        for r in &slices[..plan.head_shown] {
            out.push_str(&self.text[r.clone()]);
        }
        for (c, shown) in self.children.iter().zip(&plan.shown) {
            if *shown {
                out.push_str(&self.text[c.start..c.end]);
            }
        }
        if self.needs_break(&slices, &plan) {
            out.push('\n');
        }
        out.push_str(&self.closing(&order, &plan, slices.len()));
        out.push('\n');
        out
    }

    /// The closing line goes on a line of its own: a break is added when the text before it does not end with one.
    fn needs_break(&self, slices: &[std::ops::Range<usize>], plan: &Plan) -> bool {
        let last_child = self
            .children
            .iter()
            .zip(&plan.shown)
            .rev()
            .find(|(_, shown)| **shown)
            .map(|(c, _)| c.ends_with_newline);
        let last = last_child.or_else(|| {
            plan.head_shown
                .checked_sub(1)
                .map(|i| self.text[slices[i].clone()].ends_with('\n'))
        });
        match last {
            Some(ends) => !ends,
            None => !self.fixed.is_empty() && !self.text[self.fixed.clone()].ends_with('\n'),
        }
    }

    /// The closing line (doc 07 §7.2): one line in brackets, built from the parts that apply.
    fn closing(&self, order: &[usize], plan: &Plan, head_blocks: usize) -> String {
        let mut parts: Vec<String> = vec![format!("cut at {} tokens.", self.limit)];
        let shown: Vec<String> = self
            .children
            .iter()
            .zip(&plan.shown)
            .filter(|(_, shown)| **shown)
            .map(|(c, _)| format!("\"{}\" {}", c.name, c.checksum))
            .collect();
        if !shown.is_empty() {
            parts.push(format!("Shown, with checksums: {}.", shown.join(", ")));
        }
        let hidden: Vec<&Child> = order
            .iter()
            .filter(|i| !plan.shown[**i])
            .map(|i| &self.children[*i])
            .collect();
        if let Some(first) = hidden.first() {
            let names: Vec<String> = hidden.iter().map(|c| format!("\"{}\"", c.name)).collect();
            parts.push(format!(
                "Not shown: {}. Read one with section: \"{}\"",
                names.join(", "),
                first.name
            ));
        }
        if self.children.is_empty() {
            parts.push("The rest has no headings, so it cannot be read in parts.".to_string());
        } else if plan.head_shown < head_blocks {
            parts.push("Part of the head is not shown; it cannot be read in parts.".to_string());
        }
        // Every part but the last ends with a full stop; the last one ends the line as the doc 07 §7.2 example shows.
        let last = parts.len() - 1;
        let mut line = String::from("[");
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            line.push_str(part);
            if i < last && !part.ends_with('.') {
                line.push('.');
            }
        }
        line.push(']');
        line
    }

    /// The children in rank order (doc 07 §7.2, "Ranking"): those with every query word, then those with any, then
    /// the rest in file order; inside the first two groups the better BM25 rank first.
    fn rank(&self, query: Option<&str>) -> Vec<usize> {
        let in_file_order: Vec<usize> = (0..self.children.len()).collect();
        let Some(query) = query else { return in_file_order };
        let parts = query_parts(query);
        let (Some(all), Some(any)) = (expression_of(&parts, " AND "), expression_of(&parts, " OR ")) else {
            return in_file_order;
        };
        let texts: Vec<&str> = self.children.iter().map(|c| &self.text[c.start..c.end]).collect();
        // A failure here only costs the ranking, not the read: the cut falls back to file order.
        let Ok(scored) = score(&texts, &all, &any) else {
            return in_file_order;
        };
        let (every, some) = scored;
        let mut order: Vec<usize> = every.iter().map(|(i, _)| *i).collect();
        let in_every: std::collections::HashSet<usize> = order.iter().copied().collect();
        order.extend(some.iter().map(|(i, _)| *i).filter(|i| !in_every.contains(i)));
        let placed: std::collections::HashSet<usize> = order.iter().copied().collect();
        order.extend(in_file_order.into_iter().filter(|i| !placed.contains(i)));
        order
    }
}

type Scored = Vec<(usize, f64)>;

/// BM25 of each text that matches `all`, and of each that matches `any`, best first, on a temporary in-memory
/// table with the tokenizer of the search index. Nothing is stored.
fn score(texts: &[&str], all: &str, any: &str) -> rusqlite::Result<(Scored, Scored)> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("CREATE VIRTUAL TABLE sections USING fts5(body, tokenize='porter unicode61');")?;
    {
        let mut insert = conn.prepare("INSERT INTO sections (rowid, body) VALUES (?1, ?2)")?;
        for (i, text) in texts.iter().enumerate() {
            insert.execute(params![i as i64 + 1, text])?;
        }
    }
    let run = |expr: &str| -> rusqlite::Result<Scored> {
        let mut stmt = conn.prepare("SELECT rowid, bm25(sections) FROM sections WHERE sections MATCH ?1")?;
        let rows = stmt.query_map(params![expr], |r| {
            Ok((r.get::<_, i64>(0)? as usize - 1, r.get::<_, f64>(1)?))
        })?;
        let mut scored: Scored = rows.collect::<rusqlite::Result<_>>()?;
        scored.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        Ok(scored)
    };
    Ok((run(all)?, run(any)?))
}

/// The text of doc 07 §7, §7.3 and §6.3.
pub fn render(read: &Read) -> String {
    match read {
        Read::Whole(note) | Read::Cut(note) => {
            let head = match &note.section {
                Some(path) => format!("{}{PATH_SEPARATOR}{path}", titled_id(&note.id, note.title.as_deref())),
                None => titled_id(&note.id, note.title.as_deref()),
            };
            format!("{}{}", header_line(&head, note.checksum.as_deref()), note.text)
        }
        Read::Several { name, ids } => several_text(name, ids),
        Read::Sections { note, name, paths } => {
            let mut text = format!(
                "\"{name}\" matches {} sections in {note}. Call again with one:\n",
                paths.len()
            );
            for path in paths {
                text.push_str(&format!("  {path}\n"));
            }
            text
        }
    }
}

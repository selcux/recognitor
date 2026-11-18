//! `edit_note(note, op, ...)` (doc 07 §9): changes part of a note without rewriting it. Each op patches the bytes of
//! the file in place; Markdown is never regenerated from the parse tree.
//!
//! It follows the write path of doc 07 §5.1: the write lock for the whole call, the note's file read from disk (the
//! index only says where it is), the op's guard (§5.2), the patched bytes renamed over the file, and the note's index
//! rows rewritten in the same transaction. A refusal rolls the transaction back and returns the current data of what
//! clashed (§5.3). Sections come from [`crate::outline`], the parser `read_note` uses, so a heading-shaped line in
//! code is text.

use std::fs;
use std::io;
use std::path::Path;

use pulldown_cmark::{Event, Parser, Tag};
use rusqlite::Transaction;

use crate::error::{Error, Result};
use crate::hash::sha256_prefix;
use crate::index::reindex_written;
use crate::outline::{Outline, Section, outline};
use crate::parse::{markdown_options, parse_note};
use crate::paths::{ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, namespace_of_key, note_id};
use crate::read::{self, CHECKSUM_LEN, Picked};
use crate::resolve::{NoteLookup, find_note, several_text};
use crate::write::{bootstrap_notice, replace_file};
use crate::write_note::with_current;
use crate::{Index, SweepOptions};

/// One edit, with the arguments its op takes (doc 07 §2.1, §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Adds `text` at the end of `section`, or at the end of the main text without one.
    Append { section: Option<String>, text: String },
    /// Adds the observation line `text` (without its `- `) under `## Observations`.
    AddObservation { text: String },
    /// Adds the typed relation line `text` (without its `- `) under `## Relations`.
    AddRelation { text: String },
    /// Replaces `find`, which must occur exactly once, with `text`.
    FindReplace { find: String, text: String },
    /// Replaces the whole `section` with `text`, when its checksum is `expected_checksum`.
    ReplaceSection {
        section: String,
        expected_checksum: String,
        text: String,
    },
}

impl Op {
    /// The op with the line breaks of its new text written as `eol`. `find` stays as given: it matches either.
    fn with_line_breaks(&self, eol: &str) -> Op {
        match self {
            Op::Append { section, text } => Op::Append {
                section: section.clone(),
                text: with_eol(text, eol),
            },
            Op::AddObservation { text } => Op::AddObservation {
                text: with_eol(text, eol),
            },
            Op::AddRelation { text } => Op::AddRelation {
                text: with_eol(text, eol),
            },
            Op::FindReplace { find, text } => Op::FindReplace {
                find: find.clone(),
                text: with_eol(text, eol),
            },
            Op::ReplaceSection {
                section,
                expected_checksum,
                text,
            } => Op::ReplaceSection {
                section: section.clone(),
                expected_checksum: expected_checksum.clone(),
                text: with_eol(text, eol),
            },
        }
    }
}

/// What an `edit_note` call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditNote {
    /// The file is patched and indexed.
    Written {
        id: String,
        /// The first 12 hex characters of the SHA-256 of the file (doc 07 §7.1).
        checksum: String,
        /// The note is in the bootstrap: the result carries the notice (doc 07 §1.1).
        bootstrap: bool,
    },
    /// `add_observation` or `add_relation` with a line the section already has: nothing was written (doc 07 §9).
    AlreadyThere { id: String, checksum: String },
    /// A normal result with nothing written: the note name or the section matched several (doc 07 §6.3, §7.3).
    Several(String),
    /// Nothing was written. The text is the reason, plus the current data of what clashed (doc 07 §5.3). A tool
    /// returns it as an error.
    Refused(String),
}

/// The text of an `edit_note` result (doc 07 §9, "Result"): `<id>  checksum: <12 hex>`, then the bootstrap notice
/// when it applies. Already there: `Already there, nothing was written.` above the result line.
pub fn render(e: &EditNote) -> String {
    match e {
        EditNote::Written {
            id,
            checksum,
            bootstrap,
        } => {
            let mut s = format!("{id}  checksum: {checksum}\n");
            if *bootstrap {
                s.push_str(&bootstrap_notice(id));
                s.push('\n');
            }
            s
        }
        EditNote::AlreadyThere { id, checksum } => {
            format!("Already there, nothing was written.\n{id}  checksum: {checksum}\n")
        }
        EditNote::Several(text) | EditNote::Refused(text) => text.clone(),
    }
}

impl Index {
    /// `edit_note`: the stale-gated sweep (doc 02 §4), then the edit under the write lock (doc 07 §5.1). `note` names
    /// the note as in doc 07 §6.3. `max_tokens` is `read.max_tokens`, which cuts the note a refusal returns (§5.3).
    pub fn edit_note(&mut self, note: &str, op: &Op, max_tokens: usize) -> Result<EditNote> {
        self.sync_if_stale(SweepOptions::default())?;
        let root = self.vault_root().to_path_buf();
        let tx = self.write_transaction()?;
        let outcome = edit(&tx, &root, note, op, max_tokens)?;
        // A refusal, a normal result and "already there" drop the transaction: it rolls back, and the index is as it
        // was.
        if matches!(outcome, EditNote::Written { .. }) {
            tx.commit()?;
        }
        Ok(outcome)
    }
}

/// Doc 07 §9, "Archive".
const ARCHIVE_APPEND_ONLY: &str = "Archive notes are append only: use append to add to one.";

fn edit(tx: &Transaction<'_>, root: &Path, note: &str, op: &Op, max_tokens: usize) -> Result<EditNote> {
    let key = match find_note(tx, note)? {
        NoteLookup::One(key) => key,
        NoteLookup::Several(ids) => return Ok(EditNote::Several(several_text(note, &ids))),
        NoteLookup::None => return Err(Error::NoteNotFound(note.to_string())),
    };
    let path: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [&key], |r| r.get(0))?;
    let ns = namespace_of_key(&key).unwrap_or_default();
    // Doc 07 §1.1: the archive takes `append` only.
    if ns == ARCHIVE_NAMESPACE && !matches!(op, Op::Append { .. }) {
        return Ok(EditNote::Refused(ARCHIVE_APPEND_ONLY.to_string()));
    }
    // Doc 07 §5.1 step 2: the file from disk, never the index.
    let abs = root.join(&path);
    let bytes = match fs::read(&abs) {
        Ok(b) => b,
        // The index knows a note the disk no longer has: to the caller it does not exist (doc 07 §6).
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::NoteNotFound(note.to_string())),
        Err(e) => return Err(Error::io(&abs, e)),
    };
    let id = note_id(&path).to_string();
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(EditNote::Refused(format!(
            "{id} is not valid UTF-8, so nothing was written."
        )));
    };
    // Doc 07 §9, "Line breaks": new text is written with the note's own line breaks.
    let op = &op.with_line_breaks(eol_of(text));
    let ctx = Ctx {
        text,
        bytes: &bytes,
        path: &path,
        max_tokens,
        outline: outline(text),
        titled: read::titled_id_of(&path, text),
    };
    let patched = match op {
        Op::Append {
            section: Some(section),
            text: new,
        } => append_to_section(&ctx, section, new)?,
        Op::Append {
            section: None,
            text: new,
        } => Step::New(append_to_main_text(&ctx, new)),
        Op::AddObservation { text: line } => add_line(&ctx, line, Kind::Observation),
        Op::AddRelation { text: line } => add_line(&ctx, line, Kind::Relation),
        Op::FindReplace { find, text: new } => find_replace(&ctx, find, new),
        Op::ReplaceSection {
            section,
            expected_checksum,
            text: new,
        } => replace_section(&ctx, section, expected_checksum, new)?,
    };
    let new_text = match patched {
        Step::New(t) => t,
        Step::Already => {
            return Ok(EditNote::AlreadyThere {
                id,
                checksum: sha256_prefix(&bytes, CHECKSUM_LEN),
            });
        }
        Step::Done(result) => return Ok(result),
    };
    let new_bytes = new_text.into_bytes();
    replace_file(&abs, &new_bytes).map_err(|e| Error::io(&abs, e))?;
    let checksum = sha256_prefix(&new_bytes, CHECKSUM_LEN);
    reindex_written(tx, root, &path, new_bytes)?;
    Ok(EditNote::Written {
        id,
        checksum,
        bootstrap: ns == BOOTSTRAP_NAMESPACE,
    })
}

/// The note as read from disk, and what the ops need to know about it.
struct Ctx<'a> {
    text: &'a str,
    bytes: &'a [u8],
    path: &'a str,
    max_tokens: usize,
    outline: Outline,
    /// The id as doc 07 §4.2 shows it, with the title when it differs from the filename.
    titled: String,
}

/// What an op decided.
enum Step {
    /// The whole file as it is to be written.
    New(String),
    /// Nothing to write: the line is there already.
    Already,
    /// The call ends here, with this result.
    Done(EditNote),
}

// ---------------------------------------------------------------------------------------------------------------
// find_replace

/// Doc 07 §9: X must occur exactly once (§5.2). A match that overlaps itself counts each start, so `aa` in `aaa`
/// occurs twice and is refused. With empty `new` and a `find` that is a whole line (or several), the line goes with
/// its line break.
fn find_replace(ctx: &Ctx<'_>, find: &str, new: &str) -> Step {
    let found = occurrences(ctx.text, find);
    let reason = match found.len() {
        1 => None,
        0 => Some("The text to replace was not found, so nothing was written.".to_string()),
        n => Some(format!(
            "The text to replace occurs {n} times, so nothing was written; it must occur exactly once."
        )),
    };
    if let Some(reason) = reason {
        return Step::Done(EditNote::Refused(with_current(
            &reason,
            ctx.path,
            ctx.bytes,
            ctx.max_tokens,
        )));
    }
    let (start, mut end) = found[0];
    // Doc 07 §9, "Empty `text`": only an empty `text` deletes; whitespace is a normal replacement.
    if new.is_empty() && is_whole_line(ctx.text, start, end) {
        let rest = &ctx.text[end..];
        end += if rest.starts_with("\r\n") {
            2
        } else {
            usize::from(rest.starts_with('\n'))
        };
    }
    let mut out = String::with_capacity(ctx.text.len() + new.len());
    out.push_str(&ctx.text[..start]);
    out.push_str(new);
    out.push_str(&ctx.text[end..]);
    Step::New(out)
}

/// Where `find` occurs in `text`, as `(start, end)`, overlapping occurrences included. A line break in `find`, `\n` or
/// `\r\n`, matches either in the text (doc 07 §9, "Line breaks"), so the match can be longer or shorter than `find`.
fn occurrences(text: &str, find: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    if find.is_empty() {
        return found;
    }
    let unified = find.replace("\r\n", "\n");
    let parts: Vec<&str> = unified.split('\n').collect();
    let mut from = 0;
    while from <= text.len() {
        let Some(i) = text[from..].find(parts[0]) else {
            break;
        };
        let at = from + i;
        // A `find` that starts with a line break cannot start in the middle of a CRLF.
        let mid_crlf = parts[0].is_empty() && text[..at].ends_with('\r') && text[at..].starts_with('\n');
        if !mid_crlf && let Some(end) = match_parts(text, at, &parts) {
            found.push((at, end));
        }
        // The next try starts one character on, so overlapping matches show.
        from = at + text[at..].chars().next().map_or(1, char::len_utf8);
    }
    found
}

/// The end of the match of `parts` (the lines of `find`) at `at`, or `None`. The parts are joined by a line break.
fn match_parts(text: &str, at: usize, parts: &[&str]) -> Option<usize> {
    let mut pos = at + parts[0].len();
    for part in &parts[1..] {
        let rest = &text[pos..];
        pos += if rest.starts_with("\r\n") {
            2
        } else if rest.starts_with('\n') {
            1
        } else {
            return None;
        };
        if !text[pos..].starts_with(part) {
            return None;
        }
        pos += part.len();
    }
    Some(pos)
}

/// True when `start..end` begins at the start of a line and ends at the end of one (before its line break or the end
/// of the text).
fn is_whole_line(text: &str, start: usize, end: usize) -> bool {
    let starts_line = start == 0 || text[..start].ends_with('\n');
    let ends_line = end == text.len() || text[end..].starts_with('\n') || text[end..].starts_with("\r\n");
    starts_line && ends_line && start < end
}

// ---------------------------------------------------------------------------------------------------------------
// replace_section

/// Doc 07 §9, §5.2: the section's checksum must be `expected`. The section is named as `read_note` names it (§7.3).
fn replace_section(ctx: &Ctx<'_>, wanted: &str, expected: &str, new: &str) -> Result<Step> {
    let chain = match read::pick_section(&ctx.outline, &ctx.titled, wanted)? {
        Picked::One(chain) => chain,
        Picked::Several(paths) => {
            return Ok(Step::Done(EditNote::Several(read::sections_text(
                &ctx.titled,
                wanted,
                &paths,
            ))));
        }
    };
    let Some(section) = chain.last().copied() else {
        return Err(Error::NoteNotFound(wanted.to_string()));
    };
    // Doc 07 §5.3: new text that does not start with a heading line would merge its body into the section above.
    if !new.trim().is_empty() && !starts_with_heading(new) {
        let heading = ctx.text[section.start..].lines().next().unwrap_or_default();
        return Ok(Step::Done(EditNote::Refused(format!(
            "The new section must start with its heading line, so nothing was written. Current heading: {heading}"
        ))));
    }
    if !expected.eq_ignore_ascii_case(&read::section_checksum(ctx.text, section)) {
        // Doc 07 §5.3: the section as it is now, with its checksum, cut like a read.
        let current = read::read_file_section(ctx.path, ctx.bytes, &read::section_path(&chain), ctx.max_tokens)?;
        return Ok(Step::Done(EditNote::Refused(format!(
            "The section changed since that checksum, so nothing was written. Current version:\n{}",
            read::render(&current)
        ))));
    }
    let body = if new.trim().is_empty() {
        // Doc 07 §9: empty content removes the section, heading included.
        String::new()
    } else {
        let t = new.trim_start_matches(['\n', '\r']);
        if t.ends_with("\n\n") || t.ends_with("\r\n\r\n") {
            t.to_string()
        } else {
            // The text ends its last line, and the blank lines that set the old section apart from what follows stay.
            let (content_end, _) = content_end(ctx.text, section.start, section.end);
            let eol = eol_of(ctx.text);
            format!(
                "{}{eol}{}",
                t.trim_end_matches(['\n', '\r']),
                &ctx.text[content_end..section.end]
            )
        }
    };
    Ok(Step::New(format!(
        "{}{body}{}",
        &ctx.text[..section.start],
        &ctx.text[section.end..]
    )))
}

// ---------------------------------------------------------------------------------------------------------------
// append, add_observation, add_relation

/// True when the first block of `text`, after any blank lines, is a heading (ATX or setext), as the parser reads it.
fn starts_with_heading(text: &str) -> bool {
    let text = text.trim_start_matches(['\n', '\r']);
    Parser::new_ext(text, markdown_options())
        .next()
        .is_some_and(|event| matches!(event, Event::Start(Tag::Heading { .. })))
}

/// Doc 07 §9, "Where new text goes": the end of the named section, subsections included.
fn append_to_section(ctx: &Ctx<'_>, wanted: &str, new: &str) -> Result<Step> {
    let chain = match read::pick_section(&ctx.outline, &ctx.titled, wanted)? {
        Picked::One(chain) => chain,
        Picked::Several(paths) => {
            return Ok(Step::Done(EditNote::Several(read::sections_text(
                &ctx.titled,
                wanted,
                &paths,
            ))));
        }
    };
    let Some(section) = chain.last().copied() else {
        return Err(Error::NoteNotFound(wanted.to_string()));
    };
    Ok(Step::New(insert(ctx.text, section.start, section.end, new)))
}

/// Doc 07 §9, "Where new text goes": the end of the main text, which is just before `## Observations`; without it,
/// before `## Relations`; with neither, at the end of the note.
fn append_to_main_text(ctx: &Ctx<'_>, new: &str) -> String {
    let boundary = observations(&ctx.outline.sections)
        .or_else(|| relations(&ctx.outline.sections))
        .map_or(ctx.text.len(), |s| s.start);
    insert(ctx.text, 0, boundary, new)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Observation,
    Relation,
}

/// `add_observation` and `add_relation` (doc 07 §9): the line goes at the end of its section, which is created when
/// the note has none.
fn add_line(ctx: &Ctx<'_>, line: &str, kind: Kind) -> Step {
    let line = line.trim();
    if !is_line_of(line, kind) {
        let refusal = match kind {
            Kind::Observation => NOT_AN_OBSERVATION,
            Kind::Relation => NOT_A_RELATION,
        };
        return Step::Done(EditNote::Refused(refusal.to_string()));
    }
    let item = format!("- {line}");
    let section = match kind {
        Kind::Observation => observations(&ctx.outline.sections),
        Kind::Relation => relations(&ctx.outline.sections),
    };
    match section {
        Some(section) => {
            // The section's own text, before any subsection: the line stays directly under its heading.
            let own = &ctx.text[section.start..section.head_end()];
            if own.lines().skip(1).any(|l| l.trim() == item) {
                return Step::Already;
            }
            Step::New(insert(ctx.text, section.start, section.head_end(), &item))
        }
        None => {
            let (heading, boundary) = match kind {
                Kind::Observation => (
                    "## Observations",
                    relations(&ctx.outline.sections).map_or(ctx.text.len(), |s| s.start),
                ),
                Kind::Relation => ("## Relations", ctx.text.len()),
            };
            let eol = eol_of(ctx.text);
            Step::New(insert(ctx.text, 0, boundary, &format!("{heading}{eol}{item}")))
        }
    }
}

const NOT_AN_OBSERVATION: &str = "That is not an observation line, so nothing was written. Give one line without its \
leading \"- \", as [category] text, for example: [fact] Tokens expire after 15 minutes.";
const NOT_A_RELATION: &str = "That is not a typed relation line, so nothing was written. Give one line without its \
leading \"- \", as predicate [[Target]], for example: part_of [[Security]].";

/// True when `- <line>` under the matching heading is an observation (doc 03 §2) or a typed relation (doc 03 §3,
/// Tier 1): the parser the indexer uses reads it as one. Anything else would sit in the note as a plain list item and
/// never be indexed. A line break would make it two lines.
fn is_line_of(line: &str, kind: Kind) -> bool {
    if line.is_empty() || line.contains(['\n', '\r']) {
        return false;
    }
    match kind {
        Kind::Observation => {
            // The line itself starts with its bracket: `- - [fact] x` would be a list inside a list.
            line.starts_with('[')
                && parse_note(&format!("# T\n\n## Observations\n- {line}\n"))
                    .observations
                    .len()
                    == 1
        }
        Kind::Relation => {
            // The line starts with its predicate: `- - part_of [[X]]` would be a list inside a list.
            let parsed = parse_note(&format!("# T\n\n## Relations\n- {line}\n"));
            line.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                && !parsed.links.is_empty()
                && parsed.links.iter().all(|l| l.relation.is_some())
        }
    }
}

/// The first `## Observations` section (doc 07 §9), its heading compared in lowercase as `section` names are.
fn observations(sections: &[Section]) -> Option<&Section> {
    first_heading(sections, |s| {
        s.level == 2 && s.name.trim().eq_ignore_ascii_case("Observations")
    })
}

/// The first `## Relations` section: an H2 whose text is exactly `Relations`, as the parser reads it (doc 03 §3).
fn relations(sections: &[Section]) -> Option<&Section> {
    first_heading(sections, |s| s.level == 2 && s.name == "Relations")
}

/// The first section in file order, at any depth, that `pick` accepts.
fn first_heading(sections: &[Section], pick: impl Fn(&Section) -> bool + Copy) -> Option<&Section> {
    for s in sections {
        if pick(s) {
            return Some(s);
        }
        if let Some(found) = first_heading(&s.children, pick) {
            return Some(found);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------------------------
// Inserting text

/// The line break the note uses: that of its first line, LF when it has none.
fn eol_of(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if text[..i].ends_with('\r') => "\r\n",
        _ => "\n",
    }
}

/// `text` with every line break written as `eol`, so new text carries the note's own line breaks (doc 07 §9).
fn with_eol(text: &str, eol: &str) -> String {
    let unified = text.replace("\r\n", "\n");
    if eol == "\n" {
        unified
    } else {
        unified.replace('\n', eol)
    }
}

/// The end of the last line with content in `text[floor..boundary]`, its line break included (the end of the text when
/// it has none), and that line. The blank lines after it are what separates it from what follows.
fn content_end(text: &str, floor: usize, boundary: usize) -> (usize, &str) {
    let mut end = floor;
    let mut last = "";
    let mut at = floor;
    for line in text[floor..boundary].split_inclusive('\n') {
        at += line.len();
        if !line.trim().is_empty() {
            end = at;
            last = line;
        }
    }
    (end, last)
}

/// True for a Markdown list item: a bullet or a number, and a space.
fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        return true;
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "))
}

/// Inserts `new` after the last line with content in `text[floor..boundary]`, and returns the whole text. The
/// text goes on its own lines, set apart from what is above by one blank line, except when both are list items:
/// then it joins the list. The blank lines that were after the content stay after the new text, and a heading that
/// followed with no blank line keeps one above it.
fn insert(text: &str, floor: usize, boundary: usize, new: &str) -> String {
    let eol = eol_of(text);
    let (end, last) = content_end(text, floor, boundary);
    let body = new.trim_matches(['\n', '\r']);
    let first = body.lines().next().unwrap_or("");
    let mut out = String::with_capacity(text.len() + body.len() + 4);
    out.push_str(&text[..end]);
    if end > 0 && !text[..end].ends_with('\n') {
        out.push_str(eol);
    }
    if end > 0 && !(is_list_item(last) && is_list_item(first)) {
        out.push_str(eol);
    }
    out.push_str(body);
    out.push_str(eol);
    if end == boundary && boundary < text.len() {
        out.push_str(eol);
    }
    out.push_str(&text[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_of(text: &str) -> Ctx<'_> {
        Ctx {
            text,
            bytes: text.as_bytes(),
            path: "a/T.md",
            max_tokens: 4000,
            outline: outline(text),
            titled: "a/T".to_string(),
        }
    }

    fn new_text(step: Step) -> String {
        match step {
            Step::New(t) => t,
            Step::Already => panic!("already there"),
            Step::Done(r) => panic!("done: {r:?}"),
        }
    }

    #[test]
    fn occurrences_count_overlaps() {
        assert_eq!(occurrences("aaa", "aa"), [(0, 2), (1, 3)]);
        assert_eq!(occurrences("a b a", "a"), [(0, 1), (4, 5)]);
        assert_eq!(occurrences("x", "y"), Vec::<(usize, usize)>::new());
        assert!(occurrences("x", "").is_empty());
        assert_eq!(occurrences("\u{e9}\u{e9}", "\u{e9}"), [(0, 2), (2, 4)]);
    }

    #[test]
    fn a_line_break_in_find_matches_lf_or_crlf() {
        // Typed with LF, found in a CRLF note, and the other way round.
        assert_eq!(occurrences("a\r\nb\r\nc", "a\nb"), [(0, 4)]);
        assert_eq!(occurrences("a\nb\nc", "a\r\nb"), [(0, 3)]);
        assert_eq!(occurrences("a\nb\r\nc", "a\nb\nc"), [(0, 6)]);
        // A line break at either end, and one that is not there.
        assert_eq!(occurrences("x\r\ny", "\n"), [(1, 3)]);
        assert_eq!(occurrences("a\r\n\r\nb", "a\n\nb"), [(0, 6)]);
        assert!(occurrences("a b", "a\nb").is_empty());
        assert!(occurrences("a\n\nb", "a\nb").is_empty());
    }

    #[test]
    fn new_text_takes_the_notes_line_breaks() {
        assert_eq!(eol_of("a\r\nb\n"), "\r\n");
        assert_eq!(eol_of("a\nb\r\n"), "\n");
        assert_eq!(eol_of("no break"), "\n");
        assert_eq!(with_eol("a\nb\r\nc", "\r\n"), "a\r\nb\r\nc");
        assert_eq!(with_eol("a\r\nb\nc", "\n"), "a\nb\nc");
    }

    #[test]
    fn only_text_that_starts_with_a_heading_may_replace_a_section() {
        for ok in [
            "## A\nx",
            "\n\n## A\nx",
            "# Other\n",
            "Title\n=====\nx",
            "   ### Indented\nx",
            "###### Six",
        ] {
            assert!(starts_with_heading(ok), "{ok:?}");
        }
        for bad in [
            "x\n## A",
            "- item\n## A",
            "```\n## A\n```",
            "    ## code",
            "#nospace",
            "text only",
        ] {
            assert!(!starts_with_heading(bad), "{bad:?}");
        }
    }

    #[test]
    fn replace_section_refuses_text_without_a_heading_and_names_the_current_heading() {
        let text = "# T\n\n## A\n\nold a\n\n## B\n\nb\n";
        let ctx = ctx_of(text);
        let sum = read::section_checksum(text, &ctx.outline.sections[0]);
        let Step::Done(EditNote::Refused(refused)) = replace_section(&ctx, "A", &sum, "just the body").unwrap() else {
            panic!("text without a heading must be refused");
        };
        assert_eq!(
            refused,
            "The new section must start with its heading line, so nothing was written. Current heading: ## A"
        );
        // Before the checksum guard, so a stale checksum meets the same refusal.
        let Step::Done(EditNote::Refused(refused)) =
            replace_section(&ctx, "A", "000000000000", "just the body").unwrap()
        else {
            panic!("text without a heading must be refused");
        };
        assert!(refused.starts_with("The new section must start with its heading line"));
        // Empty or whitespace-only text still removes the section.
        assert!(matches!(
            replace_section(&ctx, "A", &sum, "  \n").unwrap(),
            Step::New(_)
        ));
    }

    #[test]
    fn a_whole_line_is_removed_with_its_line_break_when_the_replacement_is_empty() {
        let text = "# T\n\n- [fact] one\n- [fact] two\n- [fact] three\n";
        let out = new_text(find_replace(&ctx_of(text), "- [fact] two", ""));
        assert_eq!(out, "# T\n\n- [fact] one\n- [fact] three\n");
        // The last line, with and without a line break at the end of the file.
        assert_eq!(
            new_text(find_replace(&ctx_of(text), "- [fact] three", "")),
            "# T\n\n- [fact] one\n- [fact] two\n"
        );
        assert_eq!(new_text(find_replace(&ctx_of("a\nb"), "b", "")), "a\n");
        // CRLF files lose the whole break.
        assert_eq!(
            new_text(find_replace(&ctx_of("a\r\nb\r\nc\r\n"), "b", "")),
            "a\r\nc\r\n"
        );
        // Several whole lines.
        assert_eq!(new_text(find_replace(&ctx_of("a\nb\nc\nd\n"), "b\nc", "")), "a\nd\n");
    }

    #[test]
    fn part_of_a_line_leaves_the_line_and_a_non_empty_replacement_keeps_it() {
        assert_eq!(
            new_text(find_replace(&ctx_of("one two three\n"), " two", "")),
            "one three\n"
        );
        assert_eq!(
            new_text(find_replace(&ctx_of("- a\n- b\n"), "- a", "- a2")),
            "- a2\n- b\n"
        );
        // Whitespace is a normal replacement; only an empty `text` deletes (doc 07 §9, "Empty `text`").
        assert_eq!(new_text(find_replace(&ctx_of("a\nb\nc\n"), "b", "\n")), "a\n\n\nc\n");
        assert_eq!(new_text(find_replace(&ctx_of("a b c\n"), " b", "  ")), "a   c\n");
        assert_eq!(new_text(find_replace(&ctx_of("foo  bar\n"), "  ", " ")), "foo bar\n");
    }

    #[test]
    fn a_text_that_is_absent_or_repeated_is_refused_with_the_note() {
        let text = "# T\n\nsame\nsame\n";
        let Step::Done(EditNote::Refused(twice)) = find_replace(&ctx_of(text), "same", "x") else {
            panic!("a repeated text must be refused");
        };
        assert!(
            twice.starts_with(
                "The text to replace occurs 2 times, so nothing was written; it must occur exactly once. Current \
                 version:\na/T  checksum: "
            ),
            "{twice}"
        );
        assert!(twice.ends_with(text));
        let Step::Done(EditNote::Refused(none)) = find_replace(&ctx_of(text), "absent", "x") else {
            panic!("an absent text must be refused");
        };
        assert!(
            none.starts_with("The text to replace was not found, so nothing was written. Current version:\n"),
            "{none}"
        );
    }

    #[test]
    fn replace_section_keeps_the_blank_lines_around_it_and_empty_text_removes_it() {
        let text = "# T\n\nIntro.\n\n## A\n\nold a\n\n## B\n\nb\n";
        let ctx = ctx_of(text);
        let sum = read::section_checksum(text, &ctx.outline.sections[0]);
        let out = new_text(replace_section(&ctx, "A", &sum, "## A\n\nnew a").unwrap());
        assert_eq!(out, "# T\n\nIntro.\n\n## A\n\nnew a\n\n## B\n\nb\n");
        // The agent's own blank line is kept as given.
        let out = new_text(replace_section(&ctx, "a", &sum, "## A\nnew a\n\n").unwrap());
        assert_eq!(out, "# T\n\nIntro.\n\n## A\nnew a\n\n## B\n\nb\n");
        // Removed, heading included.
        let out = new_text(replace_section(&ctx, "## A", &sum.to_uppercase(), "").unwrap());
        assert_eq!(out, "# T\n\nIntro.\n\n## B\n\nb\n");
        let out = new_text(replace_section(&ctx, "A", &sum, " \n").unwrap());
        assert_eq!(out, "# T\n\nIntro.\n\n## B\n\nb\n");
    }

    #[test]
    fn replace_section_of_a_subsection_and_of_the_last_section() {
        let text = "# T\n\n## A\n\n### A1\n\none\n\n### A2\n\ntwo\n";
        let ctx = ctx_of(text);
        let a1 = &ctx.outline.sections[0].children[0];
        let sum = read::section_checksum(text, a1);
        let out = new_text(replace_section(&ctx, "A > A1", &sum, "### A1\n\nONE").unwrap());
        assert_eq!(out, "# T\n\n## A\n\n### A1\n\nONE\n\n### A2\n\ntwo\n");
        let a2 = &ctx.outline.sections[0].children[1];
        let sum = read::section_checksum(text, a2);
        let out = new_text(replace_section(&ctx, "A2", &sum, "### A2\n\nTWO\n").unwrap());
        assert_eq!(out, "# T\n\n## A\n\n### A1\n\none\n\n### A2\n\nTWO\n");
    }

    #[test]
    fn a_stale_section_checksum_returns_the_section_as_it_is_now() {
        let text = "# T\n\n## A\n\nold a\n\n## B\n\nb\n";
        let ctx = ctx_of(text);
        let Step::Done(EditNote::Refused(refused)) = replace_section(&ctx, "A", "000000000000", "## A\n").unwrap()
        else {
            panic!("a stale checksum must be refused");
        };
        let sum = read::section_checksum(text, &ctx.outline.sections[0]);
        assert_eq!(
            refused,
            format!(
                "The section changed since that checksum, so nothing was written. Current version:\n\
                 a/T > A  checksum: {sum}\n## A\n\nold a\n\n"
            )
        );
    }

    #[test]
    fn replace_section_with_several_matches_or_none() {
        let text = "# T\n\n## Storage\n\n### Notes\n\nx\n\n## Search\n\n### Notes\n\ny\n";
        let ctx = ctx_of(text);
        let Step::Done(EditNote::Several(several)) = replace_section(&ctx, "Notes", "000000000000", "x").unwrap()
        else {
            panic!("several matches are a normal result");
        };
        assert_eq!(
            several,
            "\"Notes\" matches 2 sections in a/T. Call again with one:\n  Storage > Notes\n  Search > Notes\n"
        );
        let Err(missing) = replace_section(&ctx, "Nope", "000000000000", "x") else {
            panic!("no match is an error");
        };
        assert_eq!(
            missing.to_string(),
            "No section \"Nope\" in a/T. Sections: \"Storage\", \"Search\"."
        );
    }

    #[test]
    fn a_heading_in_code_is_not_a_section() {
        let text = "# T\n\n```\n## Fake\n```\n\n## Real\n\nx\n";
        let ctx = ctx_of(text);
        assert!(replace_section(&ctx, "Fake", "000000000000", "x").is_err());
    }

    #[test]
    fn append_without_a_section_goes_before_observations_then_relations_then_the_end() {
        let with_obs = "# T\n\nSummary.\n\n## Observations\n- [fact] a\n\n## Relations\n- part_of [[X]]\n";
        assert_eq!(
            new_text(Step::New(append_to_main_text(&ctx_of(with_obs), "More text."))),
            "# T\n\nSummary.\n\nMore text.\n\n## Observations\n- [fact] a\n\n## Relations\n- part_of [[X]]\n"
        );
        let with_rel = "# T\n\nSummary.\n\n## Relations\n- part_of [[X]]\n";
        assert_eq!(
            append_to_main_text(&ctx_of(with_rel), "More text."),
            "# T\n\nSummary.\n\nMore text.\n\n## Relations\n- part_of [[X]]\n"
        );
        assert_eq!(
            append_to_main_text(&ctx_of("# T\n\nSummary.\n"), "More text."),
            "# T\n\nSummary.\n\nMore text.\n"
        );
        // No newline at the end of the file, and a glued heading.
        assert_eq!(
            append_to_main_text(&ctx_of("# T\n\nSum"), "More"),
            "# T\n\nSum\n\nMore\n"
        );
        assert_eq!(
            append_to_main_text(&ctx_of("# T\nSum\n## Observations\n- [fact] a\n"), "More"),
            "# T\nSum\n\nMore\n\n## Observations\n- [fact] a\n"
        );
    }

    #[test]
    fn append_can_add_a_new_section_and_joins_a_list() {
        let text = "# T\n\nSummary.\n\n## Observations\n- [fact] a\n";
        assert_eq!(
            append_to_main_text(&ctx_of(text), "## Plan\n\nDo it."),
            "# T\n\nSummary.\n\n## Plan\n\nDo it.\n\n## Observations\n- [fact] a\n"
        );
        let list = "# T\n\n## Steps\n\n- one\n- two\n\n## Next\n\nx\n";
        let ctx = ctx_of(list);
        assert_eq!(
            new_text(append_to_section(&ctx, "Steps", "- three").unwrap()),
            "# T\n\n## Steps\n\n- one\n- two\n- three\n\n## Next\n\nx\n"
        );
        assert_eq!(
            new_text(append_to_section(&ctx, "Steps", "A paragraph.").unwrap()),
            "# T\n\n## Steps\n\n- one\n- two\n\nA paragraph.\n\n## Next\n\nx\n"
        );
    }

    #[test]
    fn append_to_a_section_goes_after_its_subsections() {
        let text = "# T\n\n## A\n\nintro\n\n### A1\n\none\n\n## B\n\nb\n";
        let out = new_text(append_to_section(&ctx_of(text), "A", "tail").unwrap());
        assert_eq!(out, "# T\n\n## A\n\nintro\n\n### A1\n\none\n\ntail\n\n## B\n\nb\n");
        // The last section of a file with no final line break.
        let out = new_text(append_to_section(&ctx_of("# T\n\n## A\n\nx"), "A", "y").unwrap());
        assert_eq!(out, "# T\n\n## A\n\nx\n\ny\n");
        // An empty section.
        let out = new_text(append_to_section(&ctx_of("# T\n\n## A\n\n## B\n"), "A", "y").unwrap());
        assert_eq!(out, "# T\n\n## A\n\ny\n\n## B\n");
    }

    #[test]
    fn a_crlf_note_gets_crlf_breaks() {
        let text = "# T\r\n\r\nSum.\r\n\r\n## Observations\r\n- [fact] a\r\n";
        assert_eq!(
            append_to_main_text(&ctx_of(text), "More"),
            "# T\r\n\r\nSum.\r\n\r\nMore\r\n\r\n## Observations\r\n- [fact] a\r\n"
        );
        let out = new_text(add_line(&ctx_of(text), "[fact] b", Kind::Observation));
        assert_eq!(
            out,
            "# T\r\n\r\nSum.\r\n\r\n## Observations\r\n- [fact] a\r\n- [fact] b\r\n"
        );
    }

    #[test]
    fn an_observation_goes_under_its_section_or_creates_it_before_relations_or_at_the_end() {
        let text = "# T\n\nSum.\n\n## Observations\n- [fact] a\n\n## Relations\n- part_of [[X]]\n";
        assert_eq!(
            new_text(add_line(&ctx_of(text), "[decision] b #tag", Kind::Observation)),
            "# T\n\nSum.\n\n## Observations\n- [fact] a\n- [decision] b #tag\n\n## Relations\n- part_of [[X]]\n"
        );
        let no_obs = "# T\n\nSum.\n\n## Relations\n- part_of [[X]]\n";
        assert_eq!(
            new_text(add_line(&ctx_of(no_obs), "[fact] a", Kind::Observation)),
            "# T\n\nSum.\n\n## Observations\n- [fact] a\n\n## Relations\n- part_of [[X]]\n"
        );
        assert_eq!(
            new_text(add_line(&ctx_of("# T\n\nSum.\n"), "[fact] a", Kind::Observation)),
            "# T\n\nSum.\n\n## Observations\n- [fact] a\n"
        );
        // The heading is found whatever its case, and an observation stays above a subsection.
        let sub = "# T\n\n## observations\n- [fact] a\n\n### Old\n- [fact] z\n";
        assert_eq!(
            new_text(add_line(&ctx_of(sub), "[fact] b", Kind::Observation)),
            "# T\n\n## observations\n- [fact] a\n- [fact] b\n\n### Old\n- [fact] z\n"
        );
    }

    #[test]
    fn a_relation_goes_under_its_section_or_creates_it_at_the_end() {
        let text = "# T\n\nSum.\n\n## Relations\n- part_of [[X]]\n";
        assert_eq!(
            new_text(add_line(&ctx_of(text), "relates_to [[Y]] (why)", Kind::Relation)),
            "# T\n\nSum.\n\n## Relations\n- part_of [[X]]\n- relates_to [[Y]] (why)\n"
        );
        assert_eq!(
            new_text(add_line(
                &ctx_of("# T\n\n## Observations\n- [fact] a\n"),
                "part_of [[X]]",
                Kind::Relation
            )),
            "# T\n\n## Observations\n- [fact] a\n\n## Relations\n- part_of [[X]]\n"
        );
        // `## relations` is not the parser's section, so a new one is made.
        assert_eq!(
            new_text(add_line(
                &ctx_of("# T\n\n## relations\n- part_of [[X]]\n"),
                "part_of [[Y]]",
                Kind::Relation
            )),
            "# T\n\n## relations\n- part_of [[X]]\n\n## Relations\n- part_of [[Y]]\n"
        );
    }

    #[test]
    fn a_line_that_is_already_there_writes_nothing() {
        let text = "# T\n\n## Observations\n- [fact] a\n\n## Relations\n- part_of [[X]]\n";
        assert!(matches!(
            add_line(&ctx_of(text), "[fact] a", Kind::Observation),
            Step::Already
        ));
        assert!(matches!(
            add_line(&ctx_of(text), "  [fact] a  ", Kind::Observation),
            Step::Already
        ));
        assert!(matches!(
            add_line(&ctx_of(text), "part_of [[X]]", Kind::Relation),
            Step::Already
        ));
        // The same text in another section does not count.
        assert!(matches!(
            add_line(&ctx_of(text), "part_of [[X]]", Kind::Observation),
            Step::Done(_)
        ));
    }

    #[test]
    fn a_line_that_would_not_be_indexed_is_refused() {
        let ctx = ctx_of("# T\n\nSum.\n");
        for bad in [
            "plain text",
            "[ ] task",
            "[x] done",
            "[fact]no space",
            "[fact]",
            "[fact] #tag",
            "[24:33.098] timestamp",
            "- [fact] already a bullet",
            "[Title](file.md) link",
            "[fact] two\n[fact] lines",
            "",
        ] {
            assert!(
                matches!(
                    add_line(&ctx, bad, Kind::Observation),
                    Step::Done(EditNote::Refused(ref t)) if t == NOT_AN_OBSERVATION
                ),
                "{bad:?}"
            );
        }
        for bad in [
            "see also [[X]]",
            "[[X]]",
            "part_of",
            "part_of X",
            "part_of [[X]]\nrelates_to [[Y]]",
            "- part_of [[X]]",
        ] {
            assert!(
                matches!(
                    add_line(&ctx, bad, Kind::Relation),
                    Step::Done(EditNote::Refused(ref t)) if t == NOT_A_RELATION
                ),
                "{bad:?}"
            );
        }
        for good in [
            "[fact] A fact.",
            "[Decision] @effective:2026-06-10 Use SQLite. #db",
            "[a-b_1] x",
        ] {
            assert!(
                matches!(add_line(&ctx, good, Kind::Observation), Step::New(_)),
                "{good:?}"
            );
        }
        for good in [
            "part_of [[Security]]",
            "supersedes [[Old]] (replaced)",
            "relates_to [[ns::X]]",
        ] {
            assert!(matches!(add_line(&ctx, good, Kind::Relation), Step::New(_)), "{good:?}");
        }
    }

    #[test]
    fn headings_in_code_do_not_count_as_the_observations_section() {
        let text = "# T\n\n```\n## Observations\n```\n\nSum.\n";
        let out = new_text(add_line(&ctx_of(text), "[fact] a", Kind::Observation));
        assert_eq!(
            out,
            "# T\n\n```\n## Observations\n```\n\nSum.\n\n## Observations\n- [fact] a\n"
        );
    }

    #[test]
    fn the_result_lines() {
        let done = EditNote::Written {
            id: "a/T".into(),
            checksum: "3fa9c1e2b7d0".into(),
            bootstrap: false,
        };
        assert_eq!(render(&done), "a/T  checksum: 3fa9c1e2b7d0\n");
        let boot = EditNote::Written {
            id: "bootstrap/P".into(),
            checksum: "3fa9c1e2b7d0".into(),
            bootstrap: true,
        };
        assert_eq!(
            render(&boot),
            "bootstrap/P  checksum: 3fa9c1e2b7d0\nnotice: bootstrap/P loads as instructions at session start\n"
        );
        let already = EditNote::AlreadyThere {
            id: "a/T".into(),
            checksum: "3fa9c1e2b7d0".into(),
        };
        assert_eq!(
            render(&already),
            "Already there, nothing was written.\na/T  checksum: 3fa9c1e2b7d0\n"
        );
    }
}

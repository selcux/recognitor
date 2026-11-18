//! `write_note(path, content, expected_checksum?)` (doc 07 §11): creates a note, or overwrites one when it is given the
//! checksum of the version it replaces.
//!
//! It follows the write path of doc 07 §5.1: the write lock for the whole call, the target looked up **on the disk**
//! (never in the index), the guard of §5.2, the file written with [`create_file`] (which cannot replace) or
//! [`replace_file`], and the note's index rows rewritten in the same transaction. A refusal rolls the transaction back
//! and returns the current data of what clashed (§5.3).

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::Transaction;

use crate::error::{Error, Result};
use crate::hash::sha256_prefix;
use crate::index::reindex_written;
use crate::paths::{ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, note_id, strip_md_ignore_case};
use crate::read::{self, CHECKSUM_LEN};
use crate::write::{bootstrap_notice, create_file, replace_file};
use crate::{Index, SweepOptions};

/// What a `write_note` call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteNote {
    /// The file is written and indexed.
    Written {
        /// The id as the folders are spelled on disk (doc 07 §6: an existing folder keeps its spelling).
        id: String,
        /// The first 12 hex characters of the SHA-256 of the file (doc 07 §7.1).
        checksum: String,
        /// The note is in the bootstrap: the result carries the notice (doc 07 §1.1).
        bootstrap: bool,
    },
    /// Nothing was written. The text is the reason, plus the current data of what clashed (doc 07 §5.3). A tool
    /// returns it as an error.
    Refused(String),
}

/// The text of a `write_note` result (doc 07 §11, "Output"): `<id>  checksum: <12 hex>`, then the bootstrap notice
/// when it applies, or the refusal.
pub fn render(w: &WriteNote) -> String {
    match w {
        WriteNote::Written {
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
        WriteNote::Refused(text) => text.clone(),
    }
}

impl Index {
    /// `write_note`: the stale-gated sweep (doc 02 §4), then the create or overwrite under the write lock (doc 07
    /// §5.1). `max_tokens` is `read.max_tokens`, which cuts the note a refusal returns (§5.3).
    pub fn write_note(
        &mut self,
        path: &str,
        content: &str,
        expected_checksum: Option<&str>,
        max_tokens: usize,
    ) -> Result<WriteNote> {
        // Doc 07 §11: content that is empty or only whitespace is refused, as a missing argument (§2.1). An empty
        // overwrite would remove a note without the trash.
        if content.trim().is_empty() {
            return Ok(WriteNote::Refused("write_note needs content".to_string()));
        }
        // Doc 07 §11: an empty `expected_checksum` counts as absent, so the call is a create.
        let expected_checksum = expected_checksum.filter(|c| !c.is_empty());
        let target = match parse_path(path) {
            Ok(t) => t,
            Err(refusal) => return Ok(WriteNote::Refused(refusal)),
        };
        self.sync_if_stale(SweepOptions::default())?;
        let root = self.vault_root().to_path_buf();
        let tx = self.write_transaction()?;
        let outcome = write_note(&tx, &root, &target, path, content, expected_checksum, max_tokens)?;
        // A refusal drops the transaction: it rolls back, and the index is as it was.
        if matches!(outcome, WriteNote::Written { .. }) {
            tx.commit()?;
        }
        Ok(outcome)
    }
}

const ARCHIVE_APPEND_ONLY: &str =
    "Archive notes are append only: write_note never overwrites one. To add to it, use edit_note with append.";

/// The refusal for an overwrite of a note `read_note` would cut (doc 07 §5.3, §11): the agent has seen only part of
/// it, and a whole-note write would delete the rest. It returns no note, because no version of it would let the agent
/// retry the same call.
const TOO_LONG: &str = "The note is too long to read whole, so write_note cannot replace it and nothing was written. \
                        Change it with edit_note, a section at a time.";

/// The path an agent gave, split and checked (doc 07 §6, §11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// The folders as typed, top-level folder first. Never empty.
    pub(crate) folders: Vec<String>,
    /// The filename without `.md`, as typed.
    pub(crate) stem: String,
}

/// Splits `path` into folders and a filename and refuses what no note can have as its path (doc 07 §6, §11): no
/// top-level folder, a non-ASCII character. The other refusals are not in the docs: they keep a path inside the vault
/// and make it a name the indexer would read back (see the report on this slice). The `Err` is the refusal's text.
pub(crate) fn parse_path(path: &str) -> std::result::Result<Target, String> {
    if !path.is_ascii() {
        return Err("The path has a non-ASCII character. Note and folder names are ASCII only.".to_string());
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 && !path.contains('\\') {
        return Err("The path has no top-level folder. Write it from the vault root, as folder/name.".to_string());
    }
    let bad = |seg: &str| {
        seg.is_empty() || seg.starts_with('.') || seg.chars().any(|c| c.is_ascii_control() || "\\:*?\"<>|".contains(c))
    };
    let Some((last, folders)) = parts.split_last() else {
        return Err("The path is empty.".to_string());
    };
    let stem = strip_md_ignore_case(last).unwrap_or(last);
    if folders.iter().any(|f| bad(f) || f.ends_with([' ', '.'])) || bad(last) || stem.is_empty() {
        return Err(
            "The path is not a note path. Use folder/name from the vault root, with no empty, hidden or \
             \"..\" parts and none of \\ : * ? \" < > |."
                .to_string(),
        );
    }
    Ok(Target {
        folders: folders.iter().map(|f| (*f).to_string()).collect(),
        stem: stem.to_string(),
    })
}

/// One entry of a folder on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    is_dir: bool,
    is_file: bool,
}

/// The folders of `listing` whose name has the key of `name`: names are compared in lowercase, and only ASCII names
/// count, as in the walk (a non-ASCII name is left out of the index and never matches a key).
fn folder_matches(listing: &[Entry], name: &str) -> Vec<String> {
    listing
        .iter()
        .filter(|e| e.is_dir && e.name.is_ascii() && e.name.eq_ignore_ascii_case(name))
        .map(|e| e.name.clone())
        .collect()
}

/// The note files of `listing` (`*.md`, any case of the extension) whose name without it has the key of `stem`.
fn file_matches(listing: &[Entry], stem: &str) -> Vec<String> {
    listing
        .iter()
        .filter(|e| e.is_file && e.name.is_ascii())
        .filter(|e| strip_md_ignore_case(&e.name).is_some_and(|s| s.eq_ignore_ascii_case(stem)))
        .map(|e| e.name.clone())
        .collect()
}

/// The entries of the folder `dir`; none when it does not exist. Symbolic links are neither files nor folders, as in
/// the walk, which does not follow them.
fn list(dir: &Path) -> Result<Vec<Entry>> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(dir, e)),
    };
    let mut entries = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let kind = entry.file_type().map_err(|e| Error::io(entry.path(), e))?;
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: kind.is_dir(),
            is_file: kind.is_file(),
        });
    }
    Ok(entries)
}

/// Where the target is on the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Place {
    /// Two folders on the target path share a key. Their paths end with `/`.
    TwinFolders(Vec<String>),
    /// The folder as it is spelled on disk, where existing folders keep their spelling and new ones are spelled as
    /// typed, and the note files in it with the target's key (none, one, or several: twins). Paths from the vault
    /// root.
    At { folder: String, files: Vec<String> },
}

/// Doc 07 §5.1 step 2: reads each folder on the target path from the disk and compares every name in it by key. The
/// index is never asked: a human may have saved a file inside the sweep window.
pub(crate) fn locate(root: &Path, target: &Target) -> Result<Place> {
    let mut folder = String::new();
    let mut exists = true;
    for typed in &target.folders {
        let mut spelled = typed.clone();
        if exists {
            let listing = list(&root.join(&folder))?;
            let mut found = folder_matches(&listing, typed);
            match found.len() {
                0 => exists = false,
                1 => spelled = found.remove(0),
                _ => {
                    let at = |n: &String| join(&folder, &format!("{n}/"));
                    return Ok(Place::TwinFolders(found.iter().map(at).collect()));
                }
            }
        }
        folder = join(&folder, &spelled);
    }
    let files = if exists {
        file_matches(&list(&root.join(&folder))?, &target.stem)
            .iter()
            .map(|n| join(&folder, n))
            .collect()
    } else {
        Vec::new()
    };
    Ok(Place::At { folder, files })
}

pub(crate) fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// The refusal for names that differ only in case: every path, no content (doc 07 §5.3).
fn twins_refusal(paths: &[String]) -> WriteNote {
    WriteNote::Refused(twins_text(paths, "written"))
}

/// The text of [`twins_refusal`]. `done` is what did not happen: `written` for a create, `moved` for a move (doc 07
/// §5.3 gives each its own text).
pub(crate) fn twins_text(paths: &[String], done: &str) -> String {
    let mut text = format!("Names that differ only in case cannot exist together, so nothing was {done}. On disk:\n");
    for p in paths {
        text.push_str(&format!("  {p}\n"));
    }
    text
}

/// A refusal that returns the note as it is now, with its checksum, cut at the read limit (doc 07 §5.3).
fn current_refusal(root: &Path, rel: &str, reason: &str, max_tokens: usize) -> Result<WriteNote> {
    let abs = root.join(rel);
    let bytes = fs::read(&abs).map_err(|e| Error::io(&abs, e))?;
    Ok(WriteNote::Refused(with_current(reason, rel, &bytes, max_tokens)))
}

/// The refusal of a create on an existing note (doc 07 §5.3): the note as it is now. A note over the read limit gets the
/// text that says it cannot be replaced whole, because the checksum the first text asks for could never be used.
fn exists_refusal(root: &Path, rel: &str, max_tokens: usize) -> Result<WriteNote> {
    let abs = root.join(rel);
    let bytes = fs::read(&abs).map_err(|e| Error::io(&abs, e))?;
    let reason = if read::is_cut(rel, &bytes, max_tokens) {
        EXISTS_TOO_LONG
    } else {
        EXISTS
    };
    Ok(WriteNote::Refused(with_current(reason, rel, &bytes, max_tokens)))
}

/// The text of a refusal that returns the whole note: the reason, then the note as `read_note` shows it (doc 07
/// §5.3, "Cut like a read"). `bytes` are the file as it is on disk now.
pub(crate) fn with_current(reason: &str, rel: &str, bytes: &[u8], max_tokens: usize) -> String {
    format!(
        "{reason} Current version:
{}",
        read::render(&read::read_file(rel, bytes, max_tokens))
    )
}

fn write_note(
    tx: &Transaction<'_>,
    root: &Path,
    target: &Target,
    path: &str,
    content: &str,
    expected: Option<&str>,
    max_tokens: usize,
) -> Result<WriteNote> {
    let (folder, mut files) = match locate(root, target)? {
        Place::TwinFolders(paths) => return Ok(twins_refusal(&paths)),
        Place::At { folder, files } => (folder, files),
    };
    if files.len() > 1 {
        return Ok(twins_refusal(&files));
    }
    let bytes = content.as_bytes();
    match (expected, files.pop()) {
        (None, None) => {
            let rel = join(&folder, &format!("{}.md", target.stem));
            let abs = root.join(&rel);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            match create_file(&abs, bytes) {
                Ok(()) => finish(tx, root, &rel, content),
                // A file appeared since the check: the guard of a create (doc 07 §5.2), answered as if the check had
                // found it.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match locate(root, target)? {
                    Place::TwinFolders(paths) => Ok(twins_refusal(&paths)),
                    Place::At { files, .. } if files.len() > 1 => Ok(twins_refusal(&files)),
                    Place::At { files, .. } => match files.first() {
                        Some(found) => exists_refusal(root, found, max_tokens),
                        None => Err(Error::io(abs, e)),
                    },
                },
                Err(e) => Err(Error::io(abs, e)),
            }
        }
        (None, Some(found)) => exists_refusal(root, &found, max_tokens),
        // Doc 07 §6.3, "None": a note that is not there is "not found".
        (Some(_), None) => Err(Error::NoteNotFound(path.to_string())),
        (Some(expected), Some(found)) => {
            // Doc 07 §11, order of checks for an overwrite: not found (above), archive, read limit, checksum.
            let in_archive = found
                .split('/')
                .next()
                .is_some_and(|ns| ns.eq_ignore_ascii_case(ARCHIVE_NAMESPACE));
            if in_archive {
                return Ok(WriteNote::Refused(ARCHIVE_APPEND_ONLY.to_string()));
            }
            let abs = root.join(&found);
            let current = fs::read(&abs).map_err(|e| Error::io(&abs, e))?;
            // The read code makes the decision, so the two cannot disagree.
            if read::is_cut(&found, &current, max_tokens) {
                return Ok(WriteNote::Refused(TOO_LONG.to_string()));
            }
            if !expected.eq_ignore_ascii_case(&sha256_prefix(&current, CHECKSUM_LEN)) {
                return current_refusal(root, &found, CHANGED, max_tokens);
            }
            replace_file(&abs, bytes).map_err(|e| Error::io(&abs, e))?;
            finish(tx, root, &found, content)
        }
    }
}

const EXISTS: &str = "The note already exists, so nothing was written. To replace it, call again with its checksum as \
                      expected_checksum.";
const EXISTS_TOO_LONG: &str = "The note already exists and is too long to replace whole, so nothing was written. Change \
                               it with edit_note.";
const CHANGED: &str = "The note changed since that checksum, so nothing was written.";

/// Step 5 of doc 07 §5.1: the note's index rows, then the result.
fn finish(tx: &Transaction<'_>, root: &Path, rel: &str, content: &str) -> Result<WriteNote> {
    reindex_written(tx, root, rel, content.as_bytes().to_vec())?;
    let id = note_id(rel).to_string();
    let namespace = id.split('/').next().unwrap_or_default().to_ascii_lowercase();
    Ok(WriteNote::Written {
        checksum: sha256_prefix(content.as_bytes(), CHECKSUM_LEN),
        bootstrap: namespace == BOOTSTRAP_NAMESPACE,
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            is_dir: true,
            is_file: false,
        }
    }

    fn file(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            is_dir: false,
            is_file: true,
        }
    }

    #[test]
    fn a_path_splits_into_folders_and_a_stem_and_md_is_optional() {
        let a = parse_path("baseline/nutrition/Sleep.md").unwrap();
        let b = parse_path("baseline/nutrition/Sleep").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.folders, ["baseline", "nutrition"]);
        assert_eq!(a.stem, "Sleep");
        assert_eq!(parse_path("Baseline/x.MD").unwrap().stem, "x");
        // Dots inside a name are part of it.
        assert_eq!(parse_path("a/v1.2 notes").unwrap().stem, "v1.2 notes");
    }

    #[test]
    fn a_path_without_a_top_level_folder_or_with_non_ascii_is_refused() {
        assert!(parse_path("x.md").unwrap_err().contains("no top-level folder"));
        assert!(parse_path("x").unwrap_err().contains("no top-level folder"));
        assert!(parse_path("caf\u{e9}/x.md").unwrap_err().contains("non-ASCII"));
        assert!(parse_path("a/caf\u{e9}.md").unwrap_err().contains("non-ASCII"));
    }

    #[test]
    fn a_path_that_leaves_the_vault_or_hides_or_cannot_be_a_name_is_refused() {
        for p in [
            "/x/y.md",
            "a//y.md",
            "../y.md",
            "a/../y.md",
            "a/./y.md",
            ".recognitor/trash/y.md",
            "a/.hidden/y.md",
            "a/.md",
            "a/.y.md",
            "a\\y.md",
            "a/b:y.md",
            "a/y?.md",
            "a/y\n.md",
            "a /y.md",
            "a./y.md",
            "a/",
        ] {
            assert!(parse_path(p).unwrap_err().contains("not a note path"), "{p}");
        }
    }

    #[test]
    fn folders_match_by_key_and_twin_folders_all_show() {
        let listing = [dir("Baseline"), dir("baseline"), dir("other"), file("baseline.md")];
        assert_eq!(folder_matches(&listing, "BASELINE"), ["Baseline", "baseline"]);
        assert_eq!(folder_matches(&listing, "other"), ["other"]);
        assert!(folder_matches(&listing, "baselin").is_empty());
    }

    #[test]
    fn files_match_by_key_without_md_and_a_folder_of_the_same_name_is_no_twin() {
        let listing = [
            file("Nutrition.md"),
            file("nutrition.MD"),
            dir("nutrition"),
            file("nutrition.txt"),
            file("Other.md"),
        ];
        assert_eq!(file_matches(&listing, "NUTRITION"), ["Nutrition.md", "nutrition.MD"]);
        assert!(file_matches(&listing, "nutri").is_empty());
    }

    #[test]
    fn a_non_ascii_name_never_matches_a_key() {
        // U+212A KELVIN SIGN lowercases to `k`, but it is not the name `k`.
        let listing = [file("\u{212a}.md"), dir("\u{212a}")];
        assert!(file_matches(&listing, "k").is_empty());
        assert!(folder_matches(&listing, "k").is_empty());
    }

    #[test]
    fn the_result_line_is_the_id_and_the_short_checksum() {
        let plain = WriteNote::Written {
            id: "a/b".into(),
            checksum: "3fa9c1e2b7d0".into(),
            bootstrap: false,
        };
        assert_eq!(render(&plain), "a/b  checksum: 3fa9c1e2b7d0\n");
        let boot = WriteNote::Written {
            id: "bootstrap/Profile".into(),
            checksum: "3fa9c1e2b7d0".into(),
            bootstrap: true,
        };
        assert_eq!(
            render(&boot),
            "bootstrap/Profile  checksum: 3fa9c1e2b7d0\n\
             notice: bootstrap/Profile loads as instructions at session start\n"
        );
    }
}

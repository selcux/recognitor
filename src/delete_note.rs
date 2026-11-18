//! `delete_note(note, expected_checksum)` (doc 07 §10): moves a note to the vault trash and says which links now point
//! nowhere.
//!
//! It follows the write path of doc 07 §5.1: the write lock for the whole call, the note's file read **from disk**
//! (the index only says where it is), the guard of §5.2 (the whole-note checksum), the file renamed into
//! `.recognitor/trash/<timestamp>/<original path>`, and the note's index rows removed in the same transaction. A
//! refusal rolls the transaction back and returns the current note, cut like a read (§5.3).

use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use rusqlite::Transaction;

use crate::error::{Error, Result};
use crate::hash::sha256_prefix;
use crate::index::{BrokenEdge, LinkNow, unindex_deleted};
use crate::paths::{ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, namespace_of_key, note_id};
use crate::read::{self, CHECKSUM_LEN, Read};
use crate::resolve::{NoteLookup, find_note, several_text};
use crate::write::{bootstrap_deleted_notice, move_to_trash};
use crate::write_note::with_current;
use crate::{Index, SweepOptions};

/// A link that pointed to the deleted note, and where it leads now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLink {
    /// The linking note's id, with its title when that differs from the filename (doc 07 §4.2).
    pub source: String,
    pub predicate: String,
    /// The target as the linking note wrote it.
    pub target: String,
    pub now: Now,
}

/// Where a link to the deleted note leads now (doc 07 §10, "Broken means changed").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Now {
    /// To no note: a ghost.
    Nowhere,
    /// To no note, because the target now matches several (doc 03 §4).
    Ambiguous,
    /// To another note, by the same link: its id as written.
    Note(String),
}

impl From<BrokenEdge> for BrokenLink {
    fn from(e: BrokenEdge) -> Self {
        BrokenLink {
            source: e.source,
            predicate: e.predicate,
            target: e.target_ref,
            now: match e.now {
                LinkNow::Nowhere => Now::Nowhere,
                LinkNow::Ambiguous => Now::Ambiguous,
                LinkNow::Note(id) => Now::Note(id),
            },
        }
    }
}

/// What a `delete_note` call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteNote {
    /// The file is in the trash and its rows are gone.
    Deleted {
        /// The id as written.
        id: String,
        /// Where the file went, from the vault root: `.recognitor/trash/<timestamp>/<original path>`.
        trash: String,
        /// The links that now point nowhere, linking notes in id order and each note's links in written order.
        broken: Vec<BrokenLink>,
        /// The note was in the bootstrap: the result carries the notice (doc 07 §1.1).
        bootstrap: bool,
    },
    /// A normal result with nothing deleted: the name matched several notes (doc 07 §6.3).
    Several(String),
    /// Nothing was deleted. The text is the reason, plus the current data of what clashed (doc 07 §5.3). A tool
    /// returns it as an error.
    Refused(String),
}

/// The text of a `delete_note` result:
///
/// ```text
/// deleted: ops/Caching
/// trash: .recognitor/trash/2026-11-18T14-30-05Z/ops/Caching.md
/// broken links: 3 links in 3 notes
/// - backend/Auth Flow: relates_to Caching
/// - design/Plan: links_to ops::Caching  (ambiguous)
/// - ops/Runbook: links_to Caching  (now research/Caching)
/// ```
///
/// With nothing broken the third line is `broken links: none`. A bootstrap note adds the notice line at the end, as
/// `relink` does (doc 07 §6.1). The counts read as in `relink`'s `relinked:` line.
pub fn render(d: &DeleteNote) -> String {
    match d {
        DeleteNote::Several(text) | DeleteNote::Refused(text) => text.clone(),
        DeleteNote::Deleted {
            id,
            trash,
            broken,
            bootstrap,
        } => {
            let mut s = format!("deleted: {id}\ntrash: {trash}\n");
            if broken.is_empty() {
                s.push_str("broken links: none\n");
            } else {
                let mut sources: Vec<&str> = broken.iter().map(|b| b.source.as_str()).collect();
                sources.sort_unstable();
                sources.dedup();
                s.push_str(&format!(
                    "broken links: {} in {}\n",
                    counted(broken.len(), "link"),
                    counted(sources.len(), "note")
                ));
                for b in broken {
                    let mark = match &b.now {
                        Now::Nowhere => String::new(),
                        Now::Ambiguous => "  (ambiguous)".to_string(),
                        Now::Note(id) => format!("  (now {id})"),
                    };
                    s.push_str(&format!("- {}: {} {}{mark}\n", b.source, b.predicate, b.target));
                }
            }
            if *bootstrap {
                s.push_str(&bootstrap_deleted_notice(id));
                s.push('\n');
            }
            s
        }
    }
}

fn counted(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

impl Index {
    /// `delete_note`: the stale-gated sweep (doc 02 §4), then the move to the trash under the write lock (doc 07
    /// §5.1). `note` names the note as in doc 07 §6.3. `max_tokens` is `read.max_tokens`, which cuts the note a
    /// refusal returns (§5.3).
    pub fn delete_note(&mut self, note: &str, expected_checksum: &str, max_tokens: usize) -> Result<DeleteNote> {
        self.delete_note_at(note, expected_checksum, max_tokens, SystemTime::now())
    }

    /// [`Index::delete_note`] with the time of the delete given, for tests.
    pub fn delete_note_at(
        &mut self,
        note: &str,
        expected_checksum: &str,
        max_tokens: usize,
        now: SystemTime,
    ) -> Result<DeleteNote> {
        self.sync_if_stale(SweepOptions::default())?;
        let root = self.vault_root().to_path_buf();
        let tx = self.write_transaction()?;
        let outcome = delete(&tx, &root, note, expected_checksum, max_tokens, now)?;
        // A refusal and a normal result drop the transaction: it rolls back, and the index is as it was.
        if matches!(outcome, DeleteNote::Deleted { .. }) {
            tx.commit()?;
        }
        Ok(outcome)
    }
}

/// The refusal for a note in the archive (doc 07 §1.1: the archive takes creates and `append` only), worded like
/// `write_note`'s and `edit_note`'s, and naming the fix.
const ARCHIVE_APPEND_ONLY: &str =
    "Archive notes are append only: delete_note never removes one. Only the user can delete the file.";

/// The refusal for a note `read_note` would cut (doc 07 §5.3, §10): a cut read shows no whole-note checksum, so the agent
/// cannot hold a valid one, and a stale-checksum rejection would return the cut note again in a loop.
const TOO_LONG: &str = "The note is too long to read whole, so delete_note cannot check its checksum and nothing was \
                        deleted. Only the user can delete the file.";

/// The reason of a stale checksum. Doc 07 §5.3 words it for an overwrite ("nothing was written"); a delete writes
/// nothing, so this says what did not happen.
const CHANGED: &str = "The note changed since that checksum, so nothing was deleted.";

fn delete(
    tx: &Transaction<'_>,
    root: &Path,
    note: &str,
    expected: &str,
    max_tokens: usize,
    now: SystemTime,
) -> Result<DeleteNote> {
    let key = match find_note(tx, note)? {
        NoteLookup::One(key) => key,
        NoteLookup::Several(ids) => return Ok(DeleteNote::Several(several_text(note, &ids))),
        NoteLookup::None => return Err(Error::NoteNotFound(note.to_string())),
    };
    let path: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [&key], |r| r.get(0))?;
    let ns = namespace_of_key(&key).unwrap_or_default();
    // Doc 07 §1.1: nothing changes an archive note but a create and `append`.
    if ns == ARCHIVE_NAMESPACE {
        return Ok(DeleteNote::Refused(ARCHIVE_APPEND_ONLY.to_string()));
    }
    // Doc 07 §5.1 step 2: the file from disk, never the index.
    let abs = root.join(&path);
    let bytes = match fs::read(&abs) {
        Ok(b) => b,
        // The index knows a note the disk no longer has: to the caller it does not exist (doc 07 §6).
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::NoteNotFound(note.to_string())),
        Err(e) => return Err(Error::io(&abs, e)),
    };
    // Doc 07 §10: a note `read_note` would cut has no whole-note checksum to give, so it is refused before the guard.
    // The read code makes the decision, so the two cannot disagree.
    if matches!(read::read_file(&path, &bytes, max_tokens), Read::Cut(_)) {
        return Ok(DeleteNote::Refused(TOO_LONG.to_string()));
    }
    // Doc 07 §5.2: the whole-note checksum, matched ignoring case (§11).
    if !expected.eq_ignore_ascii_case(&sha256_prefix(&bytes, CHECKSUM_LEN)) {
        return Ok(DeleteNote::Refused(with_current(CHANGED, &path, &bytes, max_tokens)));
    }
    // Doc 07 §5.1 step 4, then step 5.
    let trash = move_to_trash(root, &path, now).map_err(|e| Error::io(&abs, e))?;
    let broken = unindex_deleted(tx, &key)?.into_iter().map(BrokenLink::from).collect();
    Ok(DeleteNote::Deleted {
        id: note_id(&path).to_string(),
        trash,
        broken,
        bootstrap: ns == BOOTSTRAP_NAMESPACE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broken(source: &str, predicate: &str, target: &str, now: Now) -> BrokenLink {
        BrokenLink {
            source: source.to_string(),
            predicate: predicate.to_string(),
            target: target.to_string(),
            now,
        }
    }

    fn deleted(broken: Vec<BrokenLink>, bootstrap: bool) -> DeleteNote {
        DeleteNote::Deleted {
            id: "ops/Caching".to_string(),
            trash: ".recognitor/trash/2026-11-18T14-30-05Z/ops/Caching.md".to_string(),
            broken,
            bootstrap,
        }
    }

    #[test]
    fn nothing_broken_says_so() {
        assert_eq!(
            render(&deleted(Vec::new(), false)),
            "deleted: ops/Caching\ntrash: .recognitor/trash/2026-11-18T14-30-05Z/ops/Caching.md\nbroken links: none\n"
        );
    }

    #[test]
    fn broken_links_are_counted_like_relink_and_listed_one_per_line() {
        let text = render(&deleted(
            vec![
                broken("backend/Auth Flow", "relates_to", "Caching", Now::Nowhere),
                broken("backend/Auth Flow", "links_to", "ops/Caching", Now::Nowhere),
                broken(
                    "design/Plan, titled \"The Plan\"",
                    "links_to",
                    "Caching",
                    Now::Ambiguous,
                ),
            ],
            false,
        ));
        assert_eq!(
            text,
            "deleted: ops/Caching\n\
             trash: .recognitor/trash/2026-11-18T14-30-05Z/ops/Caching.md\n\
             broken links: 3 links in 2 notes\n\
             - backend/Auth Flow: relates_to Caching\n\
             - backend/Auth Flow: links_to ops/Caching\n\
             - design/Plan, titled \"The Plan\": links_to Caching  (ambiguous)\n"
        );
    }

    #[test]
    fn a_redirected_link_ends_with_the_note_it_reaches_now() {
        let text = render(&deleted(
            vec![broken(
                "a/B",
                "links_to",
                "Caching",
                Now::Note("research/Caching".to_string()),
            )],
            false,
        ));
        assert!(
            text.ends_with("- a/B: links_to Caching  (now research/Caching)\n"),
            "{text}"
        );
    }

    #[test]
    fn one_broken_link_is_singular_and_a_bootstrap_note_adds_its_notice_last() {
        let text = render(&deleted(vec![broken("a/B", "links_to", "Caching", Now::Nowhere)], true));
        assert_eq!(
            text,
            "deleted: ops/Caching\n\
             trash: .recognitor/trash/2026-11-18T14-30-05Z/ops/Caching.md\n\
             broken links: 1 link in 1 note\n\
             - a/B: links_to Caching\n\
             notice: ops/Caching no longer loads as instructions at session start\n"
        );
    }
}

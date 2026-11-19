//! `move_note(note, new_path, expected_checksum)` (doc 07 §6.1, §11): moves or renames a note and says which links the
//! move changed.
//!
//! It follows the write path of doc 07 §5.1: the write lock for the whole call, the target looked up **on the disk**
//! with the checks of `write_note`'s create (key, case twins, portable and ASCII paths, existing folder spelling), the
//! guard of §5.2 (the whole-note checksum, which a cut read of a long note shows too), the file moved with a rename that
//! cannot replace, and the note's index rows moved with it in the same transaction. A refusal rolls the transaction back
//! and returns the current data of what clashed (§5.3).

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::Transaction;

use crate::delete_note::{BrokenLink, broken_section, counted};
use crate::error::{Error, Result};
use crate::hash::sha256_prefix;
use crate::index::reindex_moved;
use crate::lint::{NoteFinding, load_schemas_quiet, new_findings, note_findings, render_block};
use crate::paths::{ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, namespace_of_key, note_id, note_key};
use crate::read::CHECKSUM_LEN;
use crate::resolve::{NoteLookup, find_note, several_text};
use crate::write::{bootstrap_notice, move_file};
use crate::write_note::{Place, join, locate, parse_path, twins_text, with_current};
use crate::{Index, SweepOptions};

/// What a `move_note` call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveNote {
    /// The file is at its new path and its rows follow it.
    Moved {
        /// The id before the move, as written.
        old_id: String,
        /// The id now, as the folders are spelled on disk (doc 07 §6: an existing folder keeps its spelling).
        id: String,
        /// The links the move changed (doc 07 §6.1): links into the note and the note's own, linking notes in path order.
        broken: Vec<BrokenLink>,
        /// The notes `relink` can fix, each once, in id order: those with a link that pointed to the moved note.
        relink: Vec<String>,
        /// What the move does to the note's place in the bootstrap (doc 07 §1.1).
        bootstrap: BootstrapChange,
        /// The findings the move introduced in the moved note (doc 07 §5.4).
        lint: Vec<NoteFinding>,
    },
    /// A normal result with nothing moved: the name matched several notes (doc 07 §6.3), or the new path is the note's
    /// own path (doc 07 §11, "Same path").
    Normal(String),
    /// Nothing was moved. The text is the reason, plus the current data of what clashed (doc 07 §5.3). A tool returns
    /// it as an error.
    Refused(String),
}

/// The bootstrap notice of a move (doc 07 §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapChange {
    /// Neither place is in the bootstrap.
    None,
    /// The note is in the bootstrap after the move, from outside it or inside it: it loads as instructions.
    Loads,
    /// The note was in the bootstrap and no longer is.
    NoLonger,
}

/// The text of a `move_note` result:
///
/// ```text
/// moved: ideas/Caching -> design/b/Caching
/// broken links: 2 links in 2 notes
/// - ideas/index: indexes Caching  (ambiguous)
/// - ops/Runbook, titled "Runbook v2": links_to ideas::Caching
/// relink(old: "ideas/Caching", new: "design/b/Caching", notes: ["ideas/index", "ops/Runbook"])
/// ```
///
/// The links are listed as in `delete_note` (doc 07 §10), with the moved note's own links (the ones `edit_note` fixes)
/// among them. The `relink` line is one call that fixes every link into the note; the agent drops the notes the user does
/// not pick. It is left out when no link into the note broke. The bootstrap notice comes last (doc 07 §1.1).
pub fn render(m: &MoveNote) -> String {
    render_with(
        m,
        |old_id, id, notes| {
            let notes: Vec<String> = notes.iter().map(|n| format!("\"{n}\"")).collect();
            format!(
                "relink(old: \"{old_id}\", new: \"{id}\", notes: [{}])\n",
                notes.join(", ")
            )
        },
        None,
    )
}

/// The text of a `move_note` result as the CLI prints it (doc 07 §12: "the same Markdown text as the MCP tools"). It
/// differs from [`render`] in two lines, because a person at a prompt cannot call an MCP tool:
///
/// - the `relink(...)` call is the `recognitor relink` command line that does the same (`finish`, built by the
///   caller, which knows how the vault was named), and
/// - when the moved note's own links broke, one line says the user fixes them by hand, since the CLI has no `edit`
///   (doc 07 §12).
pub fn render_cli(m: &MoveNote, finish: impl Fn(&str, &str, &[String]) -> String) -> String {
    let by_hand = match m {
        MoveNote::Moved { id, broken, .. } => {
            let own = broken.iter().filter(|b| source_id(&b.source) == id).count();
            (own > 0).then(|| {
                format!(
                    "edit by hand: {} in {id} (relink does not touch the moved note's own links)\n",
                    counted(own, "link")
                )
            })
        }
        _ => None,
    };
    render_with(
        m,
        |old_id, id, notes| format!("{}\n", finish(old_id, id, notes)),
        by_hand.as_deref(),
    )
}

/// The id of a listed link's source: `id`, or `id, titled "..."` (doc 07 §4.2).
fn source_id(source: &str) -> &str {
    source.split(", titled ").next().unwrap_or(source)
}

fn render_with(
    m: &MoveNote,
    relink_line: impl Fn(&str, &str, &[String]) -> String,
    after_links: Option<&str>,
) -> String {
    match m {
        MoveNote::Normal(text) | MoveNote::Refused(text) => text.clone(),
        MoveNote::Moved {
            old_id,
            id,
            broken,
            relink,
            bootstrap,
            lint,
        } => {
            let mut s = format!("moved: {old_id} -> {id}\n");
            s.push_str(&broken_section(broken));
            if !relink.is_empty() {
                s.push_str(&relink_line(old_id, id, relink));
            }
            if let Some(line) = after_links {
                s.push_str(line);
            }
            s.push_str(&render_block(lint));
            match bootstrap {
                BootstrapChange::None => {}
                BootstrapChange::Loads => {
                    s.push_str(&bootstrap_notice(id));
                    s.push('\n');
                }
                BootstrapChange::NoLonger => {
                    s.push_str(&format!(
                        "notice: {old_id} no longer loads as instructions at session start\n"
                    ));
                }
            }
            s
        }
    }
}

impl Index {
    /// `move_note`: the stale-gated sweep (doc 02 §4), then the move under the write lock (doc 07 §5.1). `note` names the
    /// note as in doc 07 §6.3; `new_path` is from the vault root, `.md` optional. `max_tokens` is `read.max_tokens`,
    /// which cuts the note a refusal returns (§5.3).
    pub fn move_note(
        &mut self,
        note: &str,
        new_path: &str,
        expected_checksum: &str,
        max_tokens: usize,
    ) -> Result<MoveNote> {
        self.sync_if_stale(SweepOptions::default())?;
        let root = self.vault_root().to_path_buf();
        let tx = self.write_transaction()?;
        let outcome = move_it(&tx, &root, note, new_path, expected_checksum, max_tokens)?;
        // A refusal and a normal result drop the transaction: it rolls back, and the index is as it was.
        if matches!(outcome, MoveNote::Moved { .. }) {
            tx.commit()?;
        }
        Ok(outcome)
    }
}

/// The refusal for a note in the archive (doc 07 §1.1: the archive takes creates and `append` only). A move out of it
/// changes an archive note, and so does a rename inside it. Worded like `delete_note`'s, and naming the fix.
const ARCHIVE_APPEND_ONLY: &str =
    "Archive notes are append only: move_note never moves one. Only the user can move the file.";

/// The reason of a stale checksum (doc 07 §5.3 words it for an overwrite and a delete; a move changes where the note
/// is, so this says what did not happen).
const CHANGED: &str = "The note changed since that checksum, so nothing was moved.";

/// The reason of a move onto the key of another note (doc 07 §6: the error returns the existing note).
const EXISTS: &str = "A note already exists at that path, so nothing was moved.";

fn move_it(
    tx: &Transaction<'_>,
    root: &Path,
    note: &str,
    new_path: &str,
    expected: &str,
    max_tokens: usize,
) -> Result<MoveNote> {
    let key = match find_note(tx, note)? {
        NoteLookup::One(key) => key,
        NoteLookup::Several(ids) => return Ok(MoveNote::Normal(several_text(note, &ids))),
        NoteLookup::None => return Err(Error::NoteNotFound(note.to_string())),
    };
    let path: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [&key], |r| r.get(0))?;
    let old_ns = namespace_of_key(&key).unwrap_or_default().to_string();
    // Doc 07 §1.1: a move checks the store the note leaves. Nothing changes an archive note but `append`.
    if old_ns == ARCHIVE_NAMESPACE {
        return Ok(MoveNote::Refused(ARCHIVE_APPEND_ONLY.to_string()));
    }
    let target = match parse_path(new_path) {
        Ok(t) => t,
        Err(refusal) => return Ok(MoveNote::Refused(refusal)),
    };
    // Doc 07 §5.1 step 2: every folder on the target path and the names in it, from the disk.
    let (folder, mut files) = match locate(root, &target)? {
        Place::TwinFolders(paths) => return Ok(MoveNote::Refused(twins_text(&paths, "moved"))),
        Place::At { folder, files } => (folder, files),
    };
    if files.len() > 1 {
        return Ok(MoveNote::Refused(twins_text(&files, "moved")));
    }
    let new_rel = join(&folder, &format!("{}.md", target.stem));
    let new_key = note_key(&new_rel);
    let case_only = new_key == key;
    if let Some(found) = files.pop() {
        if note_key(&found) != key {
            return exists_refusal(root, &found, max_tokens);
        }
        // The only file with the target's key is the note itself.
        if found == path && new_rel == path {
            // Doc 07 §11, "Same path": a normal result, not an error.
            return Ok(MoveNote::Normal(format!(
                "The note is already at {}, so nothing was moved.",
                note_id(&path)
            )));
        }
    }
    // Doc 07 §5.1 step 2: the file from disk, never the index.
    let abs = root.join(&path);
    let bytes = match fs::read(&abs) {
        Ok(b) => b,
        // The index knows a note the disk no longer has: to the caller it does not exist (doc 07 §6).
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::NoteNotFound(note.to_string())),
        Err(e) => return Err(Error::io(&abs, e)),
    };
    // Doc 07 §5.2: the whole-note checksum, matched ignoring case (§11). A long note is guarded like any other (§7.2).
    if !expected.eq_ignore_ascii_case(&sha256_prefix(&bytes, CHECKSUM_LEN)) {
        return Ok(MoveNote::Refused(with_current(CHANGED, &path, &bytes, max_tokens)));
    }
    // Doc 07 §5.1 step 4: a rename that fails when the target exists.
    let new_abs = root.join(&new_rel);
    if let Some(parent) = new_abs.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    match move_file(&abs, &new_abs, case_only) {
        Ok(()) => {}
        // Something appeared at the target since the check: the guard of a move, answered as if the check had found it.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return match locate(root, &target)? {
                Place::TwinFolders(paths) => Ok(MoveNote::Refused(twins_text(&paths, "moved"))),
                Place::At { files, .. } if files.len() > 1 => Ok(MoveNote::Refused(twins_text(&files, "moved"))),
                Place::At { files, .. } => match files.first() {
                    Some(found) => exists_refusal(root, found, max_tokens),
                    None => Err(Error::io(new_abs, e)),
                },
            };
        }
        Err(e) => return Err(Error::io(new_abs, e)),
    }
    // Doc 07 §5.4: the note's findings at its old place, in the index as it is now, against its findings at the new
    // one once its rows moved. A path decides the namespace and the base of relative links, so a move can add some.
    let schemas = load_schemas_quiet(root);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let before = note_findings(tx, &schemas, &path, &text)?;
    // Doc 07 §5.1 step 5.
    let links = reindex_moved(tx, root, &key, &new_rel, bytes)?;
    let lint = new_findings(&before, note_findings(tx, &schemas, &new_rel, &text)?);
    let mut relink: Vec<String> = links
        .iter()
        .filter(|l| l.into_note)
        .map(|l| l.source_id.clone())
        .collect();
    relink.sort_by_key(|id| id.to_lowercase());
    relink.dedup();
    let new_in_bootstrap = namespace_of_key(&new_key) == Some(BOOTSTRAP_NAMESPACE);
    Ok(MoveNote::Moved {
        old_id: note_id(&path).to_string(),
        id: note_id(&new_rel).to_string(),
        broken: links.into_iter().map(|l| BrokenLink::from(l.edge)).collect(),
        relink,
        bootstrap: match (old_ns == BOOTSTRAP_NAMESPACE, new_in_bootstrap) {
            (_, true) => BootstrapChange::Loads,
            (true, false) => BootstrapChange::NoLonger,
            (false, false) => BootstrapChange::None,
        },
        lint,
    })
}

/// A move onto another note (doc 07 §6, §5.3): the reason, then that note as `read_note` shows it, cut at the read
/// limit.
fn exists_refusal(root: &Path, rel: &str, max_tokens: usize) -> Result<MoveNote> {
    let abs = root.join(rel);
    let bytes = fs::read(&abs).map_err(|e| Error::io(&abs, e))?;
    Ok(MoveNote::Refused(with_current(EXISTS, rel, &bytes, max_tokens)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(broken: Vec<BrokenLink>, relink: &[&str], bootstrap: BootstrapChange) -> MoveNote {
        MoveNote::Moved {
            old_id: "ideas/Caching".to_string(),
            id: "design/b/Caching".to_string(),
            broken,
            relink: relink.iter().map(|s| (*s).to_string()).collect(),
            bootstrap,
            lint: Vec::new(),
        }
    }

    #[test]
    fn nothing_broken_is_one_line_and_no_relink_call() {
        assert_eq!(
            render(&moved(Vec::new(), &[], BootstrapChange::None)),
            "moved: ideas/Caching -> design/b/Caching\nbroken links: none\n"
        );
    }

    #[test]
    fn the_relink_call_names_old_new_and_every_linking_note() {
        let broken = vec![BrokenLink {
            source: "ops/Runbook".to_string(),
            predicate: "links_to".to_string(),
            target: "ideas::Caching".to_string(),
            now: crate::delete_note::Now::Nowhere,
        }];
        assert_eq!(
            render(&moved(broken, &["ops/Runbook", "ops/Other"], BootstrapChange::Loads)),
            "moved: ideas/Caching -> design/b/Caching\n\
             broken links: 1 link in 1 note\n\
             - ops/Runbook: links_to ideas::Caching\n\
             relink(old: \"ideas/Caching\", new: \"design/b/Caching\", notes: [\"ops/Runbook\", \"ops/Other\"])\n\
             notice: design/b/Caching loads as instructions at session start\n"
        );
    }

    #[test]
    fn a_note_that_leaves_the_bootstrap_says_it_no_longer_loads() {
        assert_eq!(
            render(&moved(Vec::new(), &[], BootstrapChange::NoLonger)),
            "moved: ideas/Caching -> design/b/Caching\n\
             broken links: none\n\
             notice: ideas/Caching no longer loads as instructions at session start\n"
        );
    }

    fn finish(old: &str, new: &str, notes: &[String]) -> String {
        format!("recognitor relink {old} {new} {}", notes.join(" "))
    }

    #[test]
    fn the_cli_text_swaps_the_relink_call_for_the_command() {
        let broken = vec![BrokenLink {
            source: "ops/Runbook".to_string(),
            predicate: "links_to".to_string(),
            target: "ideas::Caching".to_string(),
            now: crate::delete_note::Now::Nowhere,
        }];
        assert_eq!(
            render_cli(&moved(broken, &["ops/Runbook"], BootstrapChange::Loads), finish),
            "moved: ideas/Caching -> design/b/Caching\n\
             broken links: 1 link in 1 note\n\
             - ops/Runbook: links_to ideas::Caching\n\
             recognitor relink ideas/Caching design/b/Caching ops/Runbook\n\
             notice: design/b/Caching loads as instructions at session start\n"
        );
    }

    #[test]
    fn the_moved_notes_own_links_get_an_edit_by_hand_line_even_with_no_linking_note() {
        let own = |source: &str| BrokenLink {
            source: source.to_string(),
            predicate: "links_to".to_string(),
            target: "./Setup".to_string(),
            now: crate::delete_note::Now::Nowhere,
        };
        let m = moved(
            vec![own("design/b/Caching"), own("design/b/Caching, titled \"Caching v2\"")],
            &[],
            BootstrapChange::NoLonger,
        );
        assert_eq!(
            render_cli(&m, finish),
            "moved: ideas/Caching -> design/b/Caching\n\
             broken links: 2 links in 2 notes\n\
             - design/b/Caching: links_to ./Setup\n\
             - design/b/Caching, titled \"Caching v2\": links_to ./Setup\n\
             edit by hand: 2 links in design/b/Caching (relink does not touch the moved note's own links)\n\
             notice: ideas/Caching no longer loads as instructions at session start\n"
        );
    }

    #[test]
    fn the_cli_text_of_a_normal_result_is_the_mcp_text() {
        let m = MoveNote::Normal("The note is already at a/B, so nothing was moved.".to_string());
        assert_eq!(render_cli(&m, finish), render(&m));
    }
}

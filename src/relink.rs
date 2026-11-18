//! `relink(old, new, notes)`: rewrites the links to a moved note in the notes the user chose (doc 07 §6.1).
//!
//! The first write tool. It follows the write path of doc 07 §5.1: the write lock for the whole call, then for each
//! note the file read from disk, its links patched in place, the file renamed over the note, and its index rows
//! rewritten. It never rejects: a note it cannot handle becomes a warning and the others still run.

use std::collections::HashSet;
use std::fs;
use std::ops::Range;
use std::path::Path;

use rusqlite::Transaction;

use crate::error::{Error, Result};
use crate::index::reindex_written;
use crate::parse::{Link, LinkSyntax, parse_note, silence};
use crate::paths::{
    ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, file_stem, namespace_of_key, note_id, parent_dir, strip_md_ignore_case,
};
use crate::resolve::{Before, NoteLookup, find_note, link_key, name_is_unique, resolve, resolves_before, several_text};
use crate::write::{bootstrap_notice, replace_file};
use crate::{Index, SweepOptions};

/// What a `relink` call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relink {
    /// `new` matched several notes: nothing was written, and the agent calls again with one id (doc 07 §6.3).
    Several {
        name: String,
        ids: Vec<String>,
    },
    Done(Relinked),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relinked {
    /// Links rewritten.
    pub links: usize,
    /// Notes written.
    pub notes: usize,
    /// One line each, without the `warning: ` prefix, in the order of `notes`.
    pub warnings: Vec<String>,
    /// Ids of the bootstrap notes written: each gets the bootstrap notice (doc 07 §1.1, doc 04 §2.3).
    pub bootstrap: Vec<String>,
}

impl Index {
    /// `relink`: the stale-gated sweep (doc 02 §4), then the rewrite under the write lock (doc 07 §5.1, §6.1). `old`
    /// is the note's id before the move; `new` and each of `notes` name a note as in doc 07 §6.3.
    pub fn relink(&mut self, old: &str, new: &str, notes: &[String]) -> Result<Relink> {
        self.sync_if_stale(SweepOptions::default())?;
        let root = self.vault_root().to_path_buf();
        let tx = self.write_transaction()?;
        let outcome = relink(&tx, &root, old, new, notes)?;
        tx.commit()?;
        Ok(outcome)
    }
}

/// The text of a `relink` result (doc 07 §6.1).
pub fn render(r: &Relink) -> String {
    match r {
        Relink::Several { name, ids } => several_text(name, ids),
        Relink::Done(d) => {
            let mut s = format!(
                "relinked: {} in {}\n",
                counted(d.links, "link"),
                counted(d.notes, "note")
            );
            for w in &d.warnings {
                s.push_str(&format!("warning: {w}\n"));
            }
            for id in &d.bootstrap {
                s.push_str(&bootstrap_notice(id));
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

/// The moved note: where it was, and where it is now.
struct Moved<'a> {
    before: Before<'a>,
    key: &'a str,
    /// The id as written, for path links.
    id: &'a str,
    /// The namespace's folder as written, for `ns::` links.
    ns_written: &'a str,
    ns: &'a str,
}

fn relink(tx: &Transaction<'_>, root: &Path, old: &str, new: &str, notes: &[String]) -> Result<Relink> {
    let new_key = match find_note(tx, new)? {
        NoteLookup::One(key) => key,
        NoteLookup::Several(ids) => {
            return Ok(Relink::Several {
                name: new.to_string(),
                ids,
            });
        }
        NoteLookup::None => return Err(Error::NoteNotFound(new.to_string())),
    };
    // `old` is an id no note has any more (doc 07 §6.3), so it is only made into a key, never looked up.
    let old_key = link_key("", LinkSyntax::Wiki, old)
        .filter(|k| k.contains('/') && !k.contains("::"))
        .ok_or_else(|| Error::OldNotAnId(old.to_string()))?;
    let (new_path, new_title): (String, String) =
        tx.query_row("SELECT path, title FROM notes WHERE key = ?1", [&new_key], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    // A move keeps the title; the filename can change with it, so the old one comes from `old`.
    let title_key = new_title.to_lowercase();
    let moved = Moved {
        before: Before {
            key: &old_key,
            namespace: namespace_of_key(&old_key).unwrap_or_default(),
            title_key: &title_key,
            name_key: file_stem(&old_key),
            now: &new_key,
        },
        key: &new_key,
        id: note_id(&new_path),
        ns_written: new_path.split('/').next().unwrap_or_default(),
        ns: namespace_of_key(&new_key).unwrap_or_default(),
    };
    let mut out = Relinked::default();
    let mut done: HashSet<String> = HashSet::new();
    for name in notes {
        let key = match find_note(tx, name)? {
            NoteLookup::One(key) => key,
            NoteLookup::Several(ids) => {
                out.warnings.push(format!(
                    "skipped, \"{name}\" matches {} notes: {}",
                    ids.len(),
                    ids.join(", ")
                ));
                continue;
            }
            NoteLookup::None => {
                out.warnings.push(format!("skipped, \"{name}\" not found"));
                continue;
            }
        };
        if done.insert(key.clone()) {
            relink_note(tx, root, &key, &moved, &mut out)?;
        }
    }
    Ok(Relink::Done(out))
}

/// Rewrites the links to the moved note in one linking note.
fn relink_note(tx: &Transaction<'_>, root: &Path, key: &str, moved: &Moved<'_>, out: &mut Relinked) -> Result<()> {
    let path: String = tx.query_row("SELECT path FROM notes WHERE key = ?1", [key], |r| r.get(0))?;
    let id = note_id(&path).to_string();
    let ns = namespace_of_key(key).unwrap_or_default();
    if ns == ARCHIVE_NAMESPACE {
        out.warnings.push(format!("skipped, archive notes have no links: {id}"));
        return Ok(());
    }
    // Doc 07 §5.1 step 2: the file from disk, never the index. A human may have edited it a moment ago.
    let abs = root.join(&path);
    let bytes = match fs::read(&abs) {
        Ok(b) => b,
        Err(e) => {
            out.warnings.push(format!("skipped, could not read {id}: {e}"));
            return Ok(());
        }
    };
    let Ok(mut text) = String::from_utf8(bytes) else {
        out.warnings.push(format!("skipped, not valid UTF-8: {id}"));
        return Ok(());
    };
    let mut parsed = parse_note(&text);
    silence(&mut parsed, ns);
    let mut found = 0;
    let mut no_form = false;
    let mut patches: Vec<(Range<usize>, String)> = Vec::new();
    for link in &parsed.links {
        let Some(ref_key) = link_key(key, link.syntax, &link.target) else {
            continue;
        };
        // Doc 07 §6.1: the link pointed to the note at `old`, and does not reach it now.
        if !resolves_before(tx, ns, &ref_key, &moved.before)?
            || resolve(tx, ns, &ref_key)?.target_key() == Some(moved.key)
        {
            continue;
        }
        found += 1;
        match locate(&text, link) {
            Some(range) => match new_target(tx, key, &id, ns, link, moved)? {
                Some(target) => patches.push((range, target)),
                // No form names the note alone (doc 07 §6.1, "No form, no rewrite"): the link stays as written. One
                // warning per linking note.
                None if !no_form => {
                    no_form = true;
                    out.warnings
                        .push(format!("skipped, no unambiguous link to {}: {id}", moved.id));
                }
                None => {}
            },
            None => out
                .warnings
                .push(format!("skipped, link could not be rewritten in {id}: {}", link.target)),
        }
    }
    if found == 0 {
        // The guard of doc 07 §5.2: the link is gone, so there is nothing to rewrite.
        out.warnings.push(format!("skipped, link already gone: {id}"));
        return Ok(());
    }
    // Nothing to write when every link of the note was left as written (no form, or not locatable).
    if patches.is_empty() {
        return Ok(());
    }
    // From the end of the file back, so the ranges still to apply keep their offsets.
    patches.sort_by_key(|p| std::cmp::Reverse(p.0.start));
    for (range, target) in &patches {
        text.replace_range(range.clone(), target);
    }
    let bytes = text.into_bytes();
    if let Err(e) = replace_file(&abs, &bytes) {
        out.warnings.push(format!("skipped, could not write {id}: {e}"));
        return Ok(());
    }
    reindex_written(tx, root, &path, bytes)?;
    out.links += patches.len();
    out.notes += 1;
    if ns == BOOTSTRAP_NAMESPACE {
        out.bootstrap.push(id);
    }
    Ok(())
}

/// The bytes of the link's target inside the link, so that only the target changes: the alias, the `#heading`, the
/// `!` of an embed, a Markdown link's text, fragment and title stay as written. `None` when the target is not
/// written inside the link (a reference-style Markdown link keeps it in its definition).
fn locate(text: &str, link: &Link) -> Option<Range<usize>> {
    let span = text.get(link.range.clone())?;
    let base = link.range.start;
    match link.syntax {
        LinkSyntax::Wiki => {
            let open = span.find("[[")? + 2;
            let close = span.rfind("]]")?;
            let inner = span.get(open..close)?;
            let dest = &inner[..inner.find(['|', '#']).unwrap_or(inner.len())];
            let lead = dest.len() - dest.trim_start().len();
            let target = dest.trim();
            (target == link.target).then(|| {
                let start = base + open + lead;
                start..start + target.len()
            })
        }
        LinkSyntax::Markdown => {
            let open = span.rfind("](")? + 2;
            let inner = span.get(open..)?;
            let lead = inner.len() - inner.trim_start().len();
            let rest = &inner[lead..];
            let lead = lead + usize::from(rest.starts_with('<'));
            inner[lead..].starts_with(link.target.as_str()).then(|| {
                let start = base + open + lead;
                start..start + link.target.len()
            })
        }
    }
}

/// The new target text, or `None` when no form from the link rules names the moved note alone. A relative link keeps its form and gets the relative path to the new file; any other wikilink,
/// a name or a path, is written by the link rules of doc 03 §4 ("Writing a link"), whatever it looked like before.
/// The alias, the `#heading`, the `!` and a written `.md` stay (doc 07 §6.1).
fn new_target(
    tx: &Transaction<'_>,
    source_key: &str,
    source_id: &str,
    source_ns: &str,
    link: &Link,
    moved: &Moved<'_>,
) -> Result<Option<String>> {
    let written = link.target.trim();
    let md = if strip_md_ignore_case(written).is_some() {
        ".md"
    } else {
        ""
    };
    Ok(Some(match link.syntax {
        LinkSyntax::Markdown => {
            let target = if written.starts_with('/') {
                format!("/{}", moved.id)
            } else {
                relative_path(parent_dir(source_id), moved.id)
            };
            percent_encode(&format!("{target}{md}"))
        }
        LinkSyntax::Wiki if written.starts_with("./") || written.starts_with("../") => {
            relative_wiki(source_id, moved.id, md)
        }
        LinkSyntax::Wiki => {
            let bare = strip_md_ignore_case(written).unwrap_or(written);
            let name = kept_name(bare.split_once("::").map_or(bare, |(_, n)| n), moved);
            // The name when it matches the moved note alone in its namespace, else the path from the namespace folder.
            let inner = moved.id.split_once('/').map_or(moved.id, |(_, rest)| rest);
            let unique = name_is_unique(tx, moved.ns, &name.to_lowercase(), moved.key)?;
            let reach = if unique { name } else { inner };
            // `ns::` exactly when the note lives in another namespace than the linking note.
            let target = if moved.ns == source_ns {
                reach.to_string()
            } else {
                format!("{}::{reach}", moved.ns_written)
            };
            let reaches = match link_key(source_key, LinkSyntax::Wiki, &target) {
                Some(k) => resolve(tx, source_ns, &k)?.target_key() == Some(moved.key),
                None => false,
            };
            // A note directly in its namespace folder whose name repeats there has no form that names it alone.
            if !reaches {
                return Ok(None);
            }
            format!("{target}{md}")
        }
    }))
}

/// A relative wikilink target from the linking note to the moved one: `../design/Caching` or `./Caching`.
fn relative_wiki(source_id: &str, moved_id: &str, md: &str) -> String {
    let rel = relative_path(parent_dir(source_id), moved_id);
    if rel.starts_with("../") {
        format!("{rel}{md}")
    } else {
        format!("./{rel}{md}")
    }
}

/// The name that goes into the new link. It follows the file: the written name stays as written while it still names
/// the moved note (its title or its new filename, as keys). A path is reduced to its last part under the same test, and
/// any other name becomes the new filename as written on disk.
fn kept_name<'a>(written: &'a str, moved: &Moved<'a>) -> &'a str {
    let new_stem = file_stem(moved.id);
    let names_it = |n: &str| {
        let k = n.to_lowercase();
        k == moved.before.title_key || k == new_stem.to_lowercase()
    };
    if names_it(written) {
        return written;
    }
    match written.rsplit_once('/') {
        Some((_, last)) if names_it(last) => last,
        _ => new_stem,
    }
}

/// The path from the folder `from_dir` to the note `to`, both from the vault root: `../design/Caching` from `ops`.
/// Folder names are ASCII and have one spelling (doc 03 §4), so they compare ignoring ASCII case.
fn relative_path(from_dir: &str, to: &str) -> String {
    let from: Vec<&str> = from_dir.split('/').filter(|s| !s.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').collect();
    let shared = from
        .iter()
        .zip(&to_parts[..to_parts.len().saturating_sub(1)])
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count();
    let mut parts: Vec<&str> = vec![".."; from.len() - shared];
    parts.extend(&to_parts[shared..]);
    parts.join("/")
}

/// Percent-encodes a Markdown link path: everything but letters, digits, `/` and the characters a URL path keeps
/// as they are. A space becomes `%20`, a parenthesis `%28` or `%29`, so the link still ends where it should.
fn percent_encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/!$&'*+,;=:@".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert_eq!(relative_path("ops", "design/Caching"), "../design/Caching");
        assert_eq!(relative_path("design", "design/Caching"), "Caching");
        assert_eq!(relative_path("design/specs", "design/Caching"), "../Caching");
        assert_eq!(relative_path("Design", "design/a/Caching"), "a/Caching");
        assert_eq!(relative_path("a/b/c", "x/y"), "../../../x/y");
        // A folder named like the note's file is still left and re-entered.
        assert_eq!(relative_path("ops/Caching", "ops/Caching"), "../Caching");
    }

    #[test]
    fn percent_encoding_keeps_slashes_and_encodes_spaces_and_parentheses() {
        assert_eq!(percent_encode("../design/Cache Layer.md"), "../design/Cache%20Layer.md");
        assert_eq!(percent_encode("a/b (old).md"), "a/b%20%28old%29.md");
    }
}

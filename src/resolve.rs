//! Link keys and link resolution (doc 03 §3 "Markdown Links", doc 03 §4).
//!
//! A link's **key** is made once, when the note is saved, and stored in `edges.ref_key`. Resolution reads only the
//! key, so a link can be resolved again without reading the file.

use rusqlite::{Connection, params};

use crate::error::Result;
use crate::parse::LinkSyntax;
use crate::paths::{join_normalized, note_id, parent_dir, percent_decode, strip_md_ignore_case};

/// The key of a link written in the note `source_key` (doc 03 §4): lowercased with Rust Unicode lowercasing, without
/// alias, heading or `.md`, with all percent-encoding decoded, and a relative path made full from the vault root.
///
/// The parser has already removed the alias, the heading and (for Markdown links) the fragment and query.
///
/// `None` when the link is a path that lands in the vault root or outside the vault: it can never reach a note, so it
/// is ignored, with no edge and no ghost (doc 03 §3-4). That covers `[[../README]]` from a top-level folder,
/// `[x](/README.md)` and `[[../../x]]`. Name links (`[[README]]`) are not paths and keep their key.
pub fn link_key(source_key: &str, syntax: LinkSyntax, target: &str) -> Option<String> {
    let decoded = percent_decode(target.trim()).to_lowercase();
    let bare = strip_md_ignore_case(&decoded).unwrap_or(&decoded);
    let base = parent_dir(source_key);
    let path = match syntax {
        LinkSyntax::Markdown => match bare.strip_prefix('/') {
            Some(from_root) => join_normalized("", from_root),
            None => join_normalized(base, bare),
        },
        LinkSyntax::Wiki if bare.starts_with("./") || bare.starts_with("../") => join_normalized(base, bare),
        LinkSyntax::Wiki => return Some(bare.to_string()),
    };
    // Every note has a folder, so a path without one is the vault root, and `None` is outside the vault.
    path.filter(|p| p.contains('/'))
}

/// The step that resolved a link. Diagnostics only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Step {
    /// Step 1: the key is a note key (a path from the vault root).
    ExactPath,
    /// Step 2: `ns::Name`.
    PrefixedName,
    /// Step 3: a name in the linking note's namespace.
    SameNamespace,
    /// Step 4: a name that exists in exactly one note.
    UniqueName,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::ExactPath => "step 1, exact path",
            Step::PrefixedName => "step 2, prefixed name",
            Step::SameNamespace => "step 3, same-namespace name",
            Step::UniqueName => "step 4, unique name",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved {
        /// The target's `notes.key`.
        key: String,
        step: Step,
    },
    /// No match: `target_key = NULL`.
    Ghost,
    /// More than one match at the first step that finds any: a ghost plus a warning. Holds the ids as written.
    Ambiguous(Vec<String>),
}

impl Resolution {
    pub fn target_key(&self) -> Option<&str> {
        match self {
            Resolution::Resolved { key, .. } => Some(key),
            _ => None,
        }
    }
}

/// Resolves `ref_key` for a link written in a note of namespace `source_ns`, by the steps of doc 03 §4. Every lookup
/// uses an index: `notes.key` (unique), `idx_notes_name` and `idx_notes_title`.
pub fn resolve(conn: &Connection, source_ns: &str, ref_key: &str) -> Result<Resolution> {
    // Step 1: exact path. Relative paths were made full when the key was made, and they never leave step 1: a key
    // with a folder in it matches no name.
    let exact: Option<String> = {
        let mut stmt = conn.prepare_cached("SELECT key FROM notes WHERE key = ?1")?;
        let mut rows = stmt.query(params![ref_key])?;
        rows.next()?.map(|r| r.get(0)).transpose()?
    };
    if let Some(key) = exact {
        return Ok(Resolution::Resolved {
            key,
            step: Step::ExactPath,
        });
    }
    // Step 2: prefixed name. Steps 3 and 4 are defined for unprefixed links only.
    if let Some((ns, name)) = ref_key.split_once("::") {
        let matches = names_in(conn, ns, name)?;
        return Ok(pick(matches, Step::PrefixedName).unwrap_or(Resolution::Ghost));
    }
    // Step 3: same-namespace name.
    if let Some(r) = pick(names_in(conn, source_ns, ref_key)?, Step::SameNamespace) {
        return Ok(r);
    }
    // Step 4: unique name anywhere.
    Ok(pick(names_anywhere(conn, ref_key)?, Step::UniqueName).unwrap_or(Resolution::Ghost))
}

/// `(key, path)` of every note in `ns` whose filename or title matches `name`. The unary `+` keeps SQLite from
/// choosing the namespace index over the name indexes.
fn names_in(conn: &Connection, ns: &str, name: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT key, path FROM notes WHERE name_key = ?2 AND +namespace = ?1 \
         UNION SELECT key, path FROM notes WHERE title_key = ?2 AND +namespace = ?1",
    )?;
    let rows = stmt.query_map(params![ns, name], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn names_anywhere(conn: &Connection, name: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT key, path FROM notes WHERE name_key = ?1 UNION SELECT key, path FROM notes WHERE title_key = ?1",
    )?;
    let rows = stmt.query_map(params![name], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `None` when there is no match, so the caller tries the next step.
fn pick(mut matches: Vec<(String, String)>, step: Step) -> Option<Resolution> {
    match matches.len() {
        0 => None,
        1 => matches.pop().map(|(key, _)| Resolution::Resolved { key, step }),
        _ => {
            let mut ids: Vec<String> = matches.iter().map(|(_, path)| note_id(path).to_string()).collect();
            ids.sort();
            Some(Resolution::Ambiguous(ids))
        }
    }
}

/// The names under which links can point at a note (doc 03 §4): its key, its title, its filename, and the last two
/// with the namespace prefix. Targeted re-resolution looks up `edges.ref_key` by each of them.
pub fn names_of(key: &str, namespace: &str, title: &str) -> Vec<String> {
    let title_key = title.to_lowercase();
    let name_key = crate::paths::file_stem(key).to_string();
    vec![
        key.to_string(),
        format!("{namespace}::{title_key}"),
        format!("{namespace}::{name_key}"),
        title_key,
        name_key,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::paths::{file_stem, namespace_of_key};

    fn conn(notes: &[(&str, &str)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(db::SCHEMA).unwrap();
        for (path, title) in notes {
            let key = path.strip_suffix(".md").unwrap().to_lowercase();
            conn.execute(
                "INSERT INTO notes (key, namespace, title, title_key, name_key, path, content, checksum, mtime, size) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '', 0, 0)",
                params![
                    key,
                    namespace_of_key(&key).unwrap(),
                    title,
                    title.to_lowercase(),
                    file_stem(&key),
                    path
                ],
            )
            .unwrap();
        }
        conn
    }

    fn catalog() -> Connection {
        conn(&[
            ("common/working-style/ADHD profile.md", "ADHD profile"),
            ("backend/Auth Flow.md", "Auth Flow"),
            ("backend/specs/security-architecture.md", "Security Architecture"),
            ("backend/guides/Setup.md", "Setup"),
            ("cognition/index.md", "Cognition"),
            ("compass/Index.md", "Compass"),
            ("compass/concepts/Feedback loops.md", "Feedback loops"),
            ("research/Caching.md", "Caching"),
            ("personal/Caching.md", "Caching"),
            ("ops/Deploy Checklist.md", "Deploy Checklist"),
            ("lang/kirmizi.md", "K\u{131}rm\u{131}z\u{131}"),
        ])
    }

    /// Makes the key for a wikilink and resolves it, as a note at `from` would.
    fn wiki(c: &Connection, from: &str, target: &str) -> Resolution {
        let from_key = from.to_lowercase();
        let key = link_key(&from_key, LinkSyntax::Wiki, target).unwrap();
        resolve(c, namespace_of_key(&from_key).unwrap(), &key).unwrap()
    }

    fn md(c: &Connection, from: &str, target: &str) -> Resolution {
        let from_key = from.to_lowercase();
        match link_key(&from_key, LinkSyntax::Markdown, target) {
            Some(key) => resolve(c, namespace_of_key(&from_key).unwrap(), &key).unwrap(),
            None => Resolution::Ghost,
        }
    }

    fn resolved(key: &str, step: Step) -> Resolution {
        Resolution::Resolved {
            key: key.to_string(),
            step,
        }
    }

    #[test]
    fn link_keys() {
        let k = |from: &str, s, t: &str| link_key(from, s, t);
        let w = LinkSyntax::Wiki;
        let m = LinkSyntax::Markdown;
        assert_eq!(k("a/x", w, "Caching").as_deref(), Some("caching"));
        assert_eq!(k("a/x", w, "Setup.md").as_deref(), Some("setup"));
        assert_eq!(k("a/x", w, "Dir/Setup.MD").as_deref(), Some("dir/setup"));
        assert_eq!(k("a/x", w, "NS::Name").as_deref(), Some("ns::name"));
        // A relative path becomes the full path from the vault root (doc 03 §4).
        assert_eq!(
            k("common/notes/x", w, "../guides/Setup.md").as_deref(),
            Some("common/guides/setup")
        );
        assert_eq!(k("common/notes/x", w, "./Same").as_deref(), Some("common/notes/same"));
        // Percent-encoding is decoded for every link, `%28` and `%29` included.
        assert_eq!(k("a/x", m, "Note%20%28draft%29.md").as_deref(), Some("a/note (draft)"));
        assert_eq!(k("a/x", w, "Deploy%20Checklist").as_deref(), Some("deploy checklist"));
        // Markdown links: relative to the note, `/` from the vault root, no `.md` needed.
        assert_eq!(k("a/b/x", m, "../ops/Deploy.md").as_deref(), Some("a/ops/deploy"));
        assert_eq!(k("a/b/x", m, "/Ops/Deploy.md").as_deref(), Some("ops/deploy"));
        assert_eq!(k("common/notes/x", m, "Setup").as_deref(), Some("common/notes/setup"));
        assert_eq!(k("common/notes/x", m, "Setup.md"), k("common/notes/x", m, "Setup"));
        // A path link that lands in the vault root or outside the vault is ignored (doc 03 §3-4); a name link is not.
        assert_eq!(k("a/x", m, "../../outside.md"), None);
        assert_eq!(k("a/x", w, "../../Outside"), None);
        assert_eq!(k("a/x", w, "../README"), None);
        assert_eq!(k("a/x", m, "../README.md"), None);
        assert_eq!(k("a/x", m, "/README.md"), None);
        assert_eq!(k("a/b/x", w, "../../README"), None);
        assert_eq!(k("a/x", w, "README").as_deref(), Some("readme"));
        assert_eq!(k("a/x", w, "README.md").as_deref(), Some("readme"));
        // Unicode lowercasing: titles and link text can hold any letter.
        assert_eq!(
            k("a/x", w, "K\u{131}rm\u{131}z\u{131}").as_deref(),
            Some("k\u{131}rm\u{131}z\u{131}")
        );
        assert_eq!(k("a/x", w, "KIRMIZI").as_deref(), Some("kirmizi"));
    }

    #[test]
    fn step_1_exact_path_with_or_without_md_ignoring_case() {
        let c = catalog();
        let want = resolved("common/working-style/adhd profile", Step::ExactPath);
        assert_eq!(wiki(&c, "backend/Auth Flow", "common/working-style/ADHD profile"), want);
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "common/working-style/ADHD profile.md"),
            want
        );
        assert_eq!(wiki(&c, "backend/Auth Flow", "COMMON/working-style/adhd PROFILE"), want);
    }

    #[test]
    fn step_1_relative_paths_are_made_full() {
        let c = catalog();
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "./Auth Flow.md"),
            resolved("backend/auth flow", Step::ExactPath)
        );
        assert_eq!(
            wiki(&c, "backend/specs/security-architecture", "../guides/Setup.md"),
            resolved("backend/guides/setup", Step::ExactPath)
        );
        // A relative path never falls back to name lookup.
        assert_eq!(wiki(&c, "backend/Auth Flow", "./Setup.md"), Resolution::Ghost);
    }

    #[test]
    fn step_2_prefixed_name() {
        let c = catalog();
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "cognition::index"),
            resolved("cognition/index", Step::PrefixedName)
        );
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "cognition::Cognition"),
            resolved("cognition/index", Step::PrefixedName)
        );
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "COGNITION::INDEX"),
            resolved("cognition/index", Step::PrefixedName)
        );
        assert_eq!(wiki(&c, "backend/Auth Flow", "nowhere::index"), Resolution::Ghost);
    }

    #[test]
    fn step_3_same_namespace_before_step_4() {
        let c = catalog();
        assert_eq!(
            wiki(&c, "research/Other", "Caching"),
            resolved("research/caching", Step::SameNamespace)
        );
        assert_eq!(
            wiki(&c, "personal/Other", "caching"),
            resolved("personal/caching", Step::SameNamespace)
        );
    }

    #[test]
    fn step_4_unique_name_by_title_or_filename() {
        let c = catalog();
        let want = resolved("backend/specs/security-architecture", Step::UniqueName);
        assert_eq!(wiki(&c, "common/x", "Security Architecture"), want);
        assert_eq!(wiki(&c, "common/x", "security-architecture"), want);
        assert_eq!(
            wiki(&c, "backend/x", "ADHD profile"),
            resolved("common/working-style/adhd profile", Step::UniqueName)
        );
    }

    #[test]
    fn title_and_filename_match_in_any_language() {
        let c = catalog();
        // The title is Turkish with dotless i; the file name is ASCII (doc 03 §4).
        let want = resolved("lang/kirmizi", Step::UniqueName);
        assert_eq!(wiki(&c, "common/x", "K\u{131}rm\u{131}z\u{131}"), want);
        assert_eq!(wiki(&c, "common/x", "KIRMIZI"), want);
    }

    #[test]
    fn ambiguous_is_a_ghost_with_both_ids_as_written() {
        let c = catalog();
        assert_eq!(
            wiki(&c, "backend/x", "Caching"),
            Resolution::Ambiguous(vec!["personal/Caching".into(), "research/Caching".into()])
        );
        // `index` exists in two namespaces and none is the source's.
        let ambiguous = wiki(&c, "backend/x", "index");
        assert!(matches!(ambiguous, Resolution::Ambiguous(ids) if ids == ["cognition/index", "compass/Index"]));
        assert_eq!(Resolution::Ambiguous(vec![]).target_key(), None);
    }

    #[test]
    fn unknown_is_a_ghost() {
        assert_eq!(wiki(&catalog(), "backend/x", "Gall's law"), Resolution::Ghost);
    }

    #[test]
    fn markdown_links_follow_the_key() {
        let c = catalog();
        let want = resolved("ops/deploy checklist", Step::ExactPath);
        assert_eq!(md(&c, "backend/Release", "../ops/Deploy%20Checklist.md"), want);
        assert_eq!(md(&c, "backend/Release", "/ops/Deploy%20Checklist.md"), want);
        assert_eq!(md(&c, "backend/Release", "/OPS/deploy checklist"), want);
        // No `.md` needed: `Setup` and `Setup.md` have the same key.
        assert_eq!(
            md(&c, "backend/guides/x", "Setup"),
            resolved("backend/guides/setup", Step::ExactPath)
        );
        assert_eq!(
            md(&c, "backend/guides/x", "Setup.md"),
            resolved("backend/guides/setup", Step::ExactPath)
        );
        // A path out of the vault makes no edge.
        assert_eq!(md(&c, "backend/Release", "../../x.md"), Resolution::Ghost);
    }

    #[test]
    fn names_of_a_note() {
        assert_eq!(
            names_of(
                "backend/specs/security-architecture",
                "backend",
                "Security Architecture"
            ),
            vec![
                "backend/specs/security-architecture",
                "backend::security architecture",
                "backend::security-architecture",
                "security architecture",
                "security-architecture"
            ]
        );
    }
}

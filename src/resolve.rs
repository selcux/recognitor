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
/// A full path from the vault root (a relative wikilink, a Markdown link) is stored with a leading `/`: `/design/b/x`.
/// The key is all that resolution reads, and an unprefixed path in a wikilink (`b/x`) starts at the namespace folder
/// instead, so the two need telling apart. No other key starts with `/` (doc 03 §4 leaves that mark to the engine).
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
    path.filter(|p| p.contains('/')).map(|p| format!("/{p}"))
}

/// The step that resolved a link. Diagnostics only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Step {
    /// Step 1: the key is a path: `b/x` written in a `design` note is `design/b/x`, `design::b/x` is `design/b/x` from
    /// anywhere, and a relative or Markdown path is already a full path from the vault root.
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

/// The note key a link key names as a path, if it is one (doc 03 §4, step 1). A path starts at a namespace folder, so
/// `design::b/x` is `design/b/x`, and an unprefixed `b/x` written in a `design` note is `design/b/x`. It is never read
/// from the vault root. A relative or Markdown path was made a full path from the vault root with a leading `/`
/// (see [`link_key`]), so it is taken as it is. A key without a `/` is a name and has no path.
fn path_candidate(source_ns: &str, ref_key: &str) -> Option<String> {
    if let Some(full) = ref_key.strip_prefix('/') {
        return Some(full.to_string());
    }
    match ref_key.split_once("::") {
        Some((ns, rest)) if rest.contains('/') => Some(format!("{ns}/{rest}")),
        Some(_) => None,
        None => ref_key.contains('/').then(|| format!("{source_ns}/{ref_key}")),
    }
}

/// The stored key equal to `key`, if a note has it.
fn note_key(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT key FROM notes WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

/// Resolves `ref_key` for a link written in a note of namespace `source_ns`, by the steps of doc 03 §4. Every lookup
/// uses an index: `notes.key` (unique), `idx_notes_name` and `idx_notes_title`.
pub fn resolve(conn: &Connection, source_ns: &str, ref_key: &str) -> Result<Resolution> {
    // Step 1: exact path. A path that matches no file goes on to the name steps (a title can hold a `/`, as in
    // `TCP/IP`), except a full path from the vault root, which is only ever a path.
    if let Some(candidate) = path_candidate(source_ns, ref_key) {
        if let Some(key) = note_key(conn, &candidate)? {
            return Ok(Resolution::Resolved {
                key,
                step: Step::ExactPath,
            });
        }
        if ref_key.starts_with('/') {
            return Ok(Resolution::Ghost);
        }
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

/// What a note named in a tool call matches (doc 07 §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteLookup {
    /// One note: its key. The tool runs.
    One(String),
    /// Several notes: their ids as written, sorted. The tool runs on none of them.
    Several(Vec<String>),
    /// No note.
    None,
}

/// Finds the note a tool argument names (doc 07 §6.3): an id, a path or a link name, matched by key. It resolves like
/// a link with steps 1, 2 and 4 of doc 03 §4 only: a tool call has no linking note, so step 3 has no namespace to
/// prefer, and a relative path (`./x`, `../x`) has no folder to start from, so it matches nothing. An unprefixed path
/// is from the vault root (it is an id), and a prefixed path `ns::b/x` is `ns/b/x`.
///
/// Unlike a link, several matches are not a ghost: the caller gets their ids back.
pub fn find_note(conn: &Connection, name: &str) -> Result<NoteLookup> {
    // The empty source key puts a relative path in the vault root or outside it, where `link_key` drops it.
    let Some(key) = link_key("", LinkSyntax::Wiki, name) else {
        return Ok(NoteLookup::None);
    };
    // Step 1: no namespace-relative try, since there is no linking note. An unprefixed path is an id, from the vault
    // root; a prefixed one starts at its namespace folder.
    let path = match key.split_once("::") {
        Some((ns, rest)) if rest.contains('/') => Some(format!("{ns}/{rest}")),
        Some(_) => None,
        None => key.contains('/').then(|| key.clone()),
    };
    if let Some(found) = path.map(|p| note_key(conn, &p)).transpose()?.flatten() {
        return Ok(NoteLookup::One(found));
    }
    let mut matches = match key.split_once("::") {
        Some((ns, name)) => names_in(conn, ns, name)?,
        None => names_anywhere(conn, &key)?,
    };
    Ok(match matches.len() {
        0 => NoteLookup::None,
        1 => matches.pop().map_or(NoteLookup::None, |(key, _)| NoteLookup::One(key)),
        _ => {
            let mut ids: Vec<String> = matches.iter().map(|(_, path)| note_id(path).to_string()).collect();
            ids.sort();
            NoteLookup::Several(ids)
        }
    })
}

/// The result text when a name matched several notes (doc 07 §6.3): the ids, sorted, for the agent to pick one.
/// Shared by every tool that names a note.
pub fn several_text(name: &str, ids: &[String]) -> String {
    let mut text = format!("\"{name}\" matches {} notes. Call again with one id:\n", ids.len());
    for id in ids {
        text.push_str(&format!("  {id}\n"));
    }
    text
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

/// True when `name` (a key) matches exactly one note in namespace `ns`, and it is the note `key`: a link by that name,
/// inside `ns`, reaches it alone (doc 03 §4, "Writing a link").
pub fn name_is_unique(conn: &Connection, ns: &str, name: &str, key: &str) -> Result<bool> {
    let matches = names_in(conn, ns, name)?;
    Ok(matches.len() == 1 && matches[0].0 == key)
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

/// A note as it was before a move (doc 07 §6.1, "Which links `relink` rewrites"): its old key and namespace, and the
/// names it had there. `now` is the key it has after the move.
#[derive(Debug, Clone, Copy)]
pub struct Before<'a> {
    pub key: &'a str,
    pub namespace: &'a str,
    pub title_key: &'a str,
    pub name_key: &'a str,
    pub now: &'a str,
}

impl Before<'_> {
    fn has_name(&self, name: &str) -> bool {
        name == self.title_key || name == self.name_key
    }

    /// A stored note other than this one, in either place.
    fn is_other(&self, key: &str) -> bool {
        key != self.key && key != self.now
    }
}

/// True when `ref_key`, written in a note of `source_ns`, resolves to the note as if it still were at its old place:
/// the steps of doc 03 §4 over the vault as it is, with the note at `before.key` and not at `before.now`.
pub fn resolves_before(conn: &Connection, source_ns: &str, ref_key: &str, before: &Before<'_>) -> Result<bool> {
    // Step 1: the old key itself, or another note's key. The note at `now` is not there yet, as far as this check goes.
    if let Some(candidate) = path_candidate(source_ns, ref_key) {
        if candidate == before.key {
            return Ok(true);
        }
        if let Some(key) = note_key(conn, &candidate)?
            && before.is_other(&key)
        {
            return Ok(false);
        }
        if ref_key.starts_with('/') {
            return Ok(false);
        }
    }
    let others = |matches: Vec<(String, String)>| matches.iter().filter(|(k, _)| before.is_other(k)).count();
    // Step 2: prefixed name.
    if let Some((ns, name)) = ref_key.split_once("::") {
        let hit = ns == before.namespace && before.has_name(name);
        return Ok(hit && others(names_in(conn, ns, name)?) == 0);
    }
    // Step 3: same-namespace name. Any match here, the note's or another one, decides.
    let hit = source_ns == before.namespace && before.has_name(ref_key);
    let rest = others(names_in(conn, source_ns, ref_key)?);
    if hit || rest > 0 {
        return Ok(hit && rest == 0);
    }
    // Step 4: unique name anywhere.
    Ok(before.has_name(ref_key) && others(names_anywhere(conn, ref_key)?) == 0)
}

/// The names under which links can point at a note (doc 03 §4): its key (also with the leading `/` of a full path), its title, its filename, the last two with
/// the namespace prefix, and its path inside the namespace, bare and with the prefix (`b/caching`,
/// `design::b/caching`). Targeted re-resolution looks up `edges.ref_key` by each of them.
pub fn names_of(key: &str, namespace: &str, title: &str) -> Vec<String> {
    let title_key = title.to_lowercase();
    let name_key = crate::paths::file_stem(key).to_string();
    let mut names = vec![
        key.to_string(),
        format!("/{key}"),
        format!("{namespace}::{title_key}"),
        format!("{namespace}::{name_key}"),
        title_key,
        name_key,
    ];
    // A note directly in its namespace folder has no path inside it beyond its filename, which is listed above.
    if let Some(inner) = key.strip_prefix(namespace).and_then(|r| r.strip_prefix('/'))
        && inner.contains('/')
    {
        names.push(inner.to_string());
        names.push(format!("{namespace}::{inner}"));
    }
    names
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
        // It is marked with a leading `/`, to tell it from a path that starts at the namespace folder.
        assert_eq!(
            k("common/notes/x", w, "../guides/Setup.md").as_deref(),
            Some("/common/guides/setup")
        );
        assert_eq!(k("common/notes/x", w, "./Same").as_deref(), Some("/common/notes/same"));
        assert_eq!(k("a/x", w, "b/Caching.md").as_deref(), Some("b/caching"));
        assert_eq!(k("a/x", w, "ns::b/Caching").as_deref(), Some("ns::b/caching"));
        // Percent-encoding is decoded for every link, `%28` and `%29` included.
        assert_eq!(k("a/x", m, "Note%20%28draft%29.md").as_deref(), Some("/a/note (draft)"));
        assert_eq!(k("a/x", w, "Deploy%20Checklist").as_deref(), Some("deploy checklist"));
        // Markdown links: relative to the note, `/` from the vault root, no `.md` needed.
        assert_eq!(k("a/b/x", m, "../ops/Deploy.md").as_deref(), Some("/a/ops/deploy"));
        assert_eq!(k("a/b/x", m, "/Ops/Deploy.md").as_deref(), Some("/ops/deploy"));
        assert_eq!(k("common/notes/x", m, "Setup").as_deref(), Some("/common/notes/setup"));
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
        // The path starts at the namespace folder: from `common` bare, from elsewhere with the prefix.
        assert_eq!(wiki(&c, "common/Other", "working-style/ADHD profile"), want);
        assert_eq!(wiki(&c, "common/Other", "working-style/ADHD profile.md"), want);
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "common::working-style/ADHD profile"),
            want
        );
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "COMMON::working-style/adhd PROFILE.md"),
            want
        );
        // A path is never read from the vault root.
        assert_eq!(
            wiki(&c, "backend/Auth Flow", "common/working-style/ADHD profile"),
            Resolution::Ghost
        );
        assert_eq!(
            wiki(&c, "common/Other", "common/working-style/ADHD profile"),
            Resolution::Ghost
        );
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
                "/backend/specs/security-architecture",
                "backend::security architecture",
                "backend::security-architecture",
                "security architecture",
                "security-architecture",
                "specs/security-architecture",
                "backend::specs/security-architecture"
            ]
        );
        // A note directly in its namespace folder has no path of its own inside it.
        assert_eq!(
            names_of("backend/auth flow", "backend", "Auth Flow"),
            vec![
                "backend/auth flow",
                "/backend/auth flow",
                "backend::auth flow",
                "backend::auth flow",
                "auth flow",
                "auth flow"
            ]
        );
    }

    /// Notes for the path rules: `Caching` repeats in `design`, and one note sits where a vault-root path would
    /// also point.
    fn design() -> Connection {
        conn(&[
            ("design/b/Caching.md", "Caching"),
            ("design/a/Caching.md", "Caching"),
            ("design/Caching.md", "Caching"),
            ("ops/Runbook.md", "Runbook"),
        ])
    }

    #[test]
    fn step_1_path_starts_at_the_namespace_folder() {
        let c = design();
        let want = resolved("design/b/caching", Step::ExactPath);
        assert_eq!(wiki(&c, "design/Other", "b/Caching"), want);
        assert_eq!(wiki(&c, "design/Other", "b/Caching.md"), want);
        assert_eq!(wiki(&c, "design/a/Other", "B/CACHING"), want);
        // The same path from another namespace starts at its own folder: `ops/b/caching` does not exist.
        assert_eq!(wiki(&c, "ops/Runbook", "b/Caching"), Resolution::Ghost);
    }

    #[test]
    fn step_1_prefixed_path() {
        let c = design();
        let want = resolved("design/b/caching", Step::ExactPath);
        assert_eq!(wiki(&c, "ops/Runbook", "design::b/Caching"), want);
        assert_eq!(wiki(&c, "ops/Runbook", "design::b/Caching.md"), want);
        assert_eq!(wiki(&c, "design/Other", "design::b/Caching"), want);
        assert_eq!(wiki(&c, "ops/Runbook", "DESIGN::B/caching"), want);
        // A prefixed path is a path, not a name: nothing named `nowhere/Caching` is looked up as a name, and the
        // vault-root try does not apply to it.
        assert_eq!(wiki(&c, "ops/Runbook", "design::nowhere/Caching"), Resolution::Ghost);
        assert_eq!(wiki(&c, "design/Other", "ops::design/b/Caching"), Resolution::Ghost);
    }

    #[test]
    fn step_1_never_reads_a_path_from_the_vault_root() {
        let c = design();
        // `design/b/Caching` written in `design` is `design/design/b/Caching`, and from `ops`, `ops/design/b/Caching`.
        assert_eq!(wiki(&c, "design/Other", "design/b/Caching"), Resolution::Ghost);
        assert_eq!(wiki(&c, "ops/Runbook", "design/b/Caching"), Resolution::Ghost);
        assert_eq!(wiki(&c, "ops/Runbook", "design/b/Caching.md"), Resolution::Ghost);
        // It finds the file where that path is.
        let c = conn(&[
            ("design/b/Caching.md", "Caching"),
            ("design/design/b/Caching.md", "Caching"),
        ]);
        assert_eq!(
            wiki(&c, "design/Other", "design/b/Caching"),
            resolved("design/design/b/caching", Step::ExactPath)
        );
    }

    #[test]
    fn a_title_with_a_slash_still_resolves_by_name() {
        let c = conn(&[("design/Network.md", "TCP/IP")]);
        assert_eq!(
            wiki(&c, "design/Other", "TCP/IP"),
            resolved("design/network", Step::SameNamespace)
        );
    }

    #[test]
    fn tool_calls_take_an_unprefixed_path_from_the_vault_root() {
        let c = design();
        let one = |k: &str| NoteLookup::One(k.to_string());
        assert_eq!(find_note(&c, "design/b/Caching").unwrap(), one("design/b/caching"));
        assert_eq!(find_note(&c, "design/b/caching.md").unwrap(), one("design/b/caching"));
        assert_eq!(find_note(&c, "design::b/Caching").unwrap(), one("design/b/caching"));
        assert_eq!(find_note(&c, "DESIGN::B/CACHING.md").unwrap(), one("design/b/caching"));
        // No linking note, so no namespace-relative try, and no relative paths.
        assert_eq!(find_note(&c, "b/Caching").unwrap(), NoteLookup::None);
        assert_eq!(find_note(&c, "./b/Caching").unwrap(), NoteLookup::None);
        assert_eq!(find_note(&c, "ops::b/Caching").unwrap(), NoteLookup::None);
        // A name still matches by step 2 or 4.
        assert_eq!(
            find_note(&c, "design::Caching").unwrap(),
            NoteLookup::Several(vec![
                "design/Caching".into(),
                "design/a/Caching".into(),
                "design/b/Caching".into()
            ])
        );
        assert_eq!(find_note(&c, "Runbook").unwrap(), one("ops/runbook"));
    }

    /// The note `design/b/Caching` as if it still sat at `ideas/Caching`.
    fn before_of<'a>() -> Before<'a> {
        Before {
            key: "ideas/caching",
            namespace: "ideas",
            title_key: "caching",
            name_key: "caching",
            now: "design/b/caching",
        }
    }

    fn was(c: &Connection, from: &str, target: &str) -> bool {
        let from_key = from.to_lowercase();
        let key = link_key(&from_key, LinkSyntax::Wiki, target).unwrap();
        resolves_before(c, namespace_of_key(&from_key).unwrap(), &key, &before_of()).unwrap()
    }

    #[test]
    fn resolves_before_handles_namespace_relative_and_prefixed_paths_to_old() {
        let c = conn(&[
            ("design/b/Caching.md", "Caching"),
            ("ideas/Other.md", "Other"),
            ("ops/Runbook.md", "Runbook"),
            ("ops/Caching.md", "Caching"),
        ]);
        // `ideas/Caching` is gone from the vault, but these links pointed to it.
        assert!(was(&c, "ideas/Other", "Caching"));
        assert!(was(&c, "ops/Runbook", "ideas::Caching"));
        // A path from the vault root was never read, so it did not point to the note.
        assert!(!was(&c, "ops/Runbook", "ideas/Caching"));
        assert!(!was(&c, "ideas/Other", "ideas/Caching"));
        // The old note was directly in `ideas`, so `sub/Caching` is not a path to it.
        assert!(!was(&c, "ideas/Other", "sub/Caching"));
        // A relative path (a full path from the vault root, marked with `/`) to the old place did.
        assert!(was(&c, "ops/Runbook", "../ideas/Caching"));
        // `ops/Caching` is another note of the same name: in `ops` the name goes to it.
        assert!(!was(&c, "ops/Runbook", "Caching"));
        // Links that point to other notes did not point to the moved one.
        assert!(!was(&c, "ops/Runbook", "design::b/Caching"));
        assert!(!was(&c, "ops/Runbook", "design/b/Caching"));
    }

    #[test]
    fn resolves_before_old_in_a_folder_is_a_namespace_relative_path() {
        let c = conn(&[("design/Caching.md", "Caching"), ("ideas/Other.md", "Other")]);
        let before = Before {
            key: "ideas/sub/caching",
            namespace: "ideas",
            title_key: "caching",
            name_key: "caching",
            now: "design/caching",
        };
        let was = |from: &str, target: &str| {
            let from_key = from.to_lowercase();
            let key = link_key(&from_key, LinkSyntax::Wiki, target).unwrap();
            resolves_before(&c, namespace_of_key(&from_key).unwrap(), &key, &before).unwrap()
        };
        assert!(was("ideas/Other", "sub/Caching"));
        assert!(was("design/Other", "ideas::sub/Caching"));
        assert!(!was("design/Other", "ideas/sub/Caching"));
        assert!(!was("ideas/Other", "other/Caching"));
    }
}

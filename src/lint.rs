// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor lint [--strict]` (doc 02 §8, doc 03 §2-4, doc 07 §3, §6, §12).
//!
//! Lint is a read-only report on the vault. It writes no note file. The docs give the checks, the type schemas and the
//! exit codes but no output format; the format here is provisional (see the slice report).
//!
//! **Where the warnings come from.** A no-change `sync` reads no files, and the index stores no warnings, so lint
//! cannot ask the index for them. It forces a sweep first (so the index is current and reports the names it left out
//! and the files it could not read), then parses the stored copy of every note with the one parser, and resolves every
//! link with the one resolver. Nothing is read from disk twice, and no rule is written a second time.
//!
//! **Severity.** Errors are the checks the docs call errors: a non-ASCII name or a case twin (check 7), and a
//! `schemas.toml` that cannot be used. Everything else is a warning; `--strict` turns every warning into an error.
//! The bootstrap hint is never an error, even with `--strict`.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use toml_edit::{DocumentMut, Item};

use crate::error::Result;
use crate::index::{Index, SweepOptions, edge_rows};
use crate::parse::{LinkSyntax, ParsedNote, WarningKind, parse_note, silence};
use crate::paths::{
    ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, file_stem, join_normalized, namespace_of_key, note_id, note_key,
    parent_dir, percent_decode, strip_md_ignore_case,
};
use crate::resolve::{Resolution, Step, ambiguous_message, resolve};

/// Where the type schemas live, from the vault root (doc 02 §8).
pub const SCHEMAS_PATH: &str = ".recognitor/schemas.toml";

/// The keys of a `[type.<name>]` block (doc 02 §8).
const SCHEMA_KEYS: [&str; 4] = [
    "required_fields",
    "allowed",
    "required_categories",
    "required_relations",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Hint,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Hint => "hint",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// A note id as written, or `.recognitor/schemas.toml`. `None` for findings about the vault as a whole.
    pub subject: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

impl Finding {
    fn new(severity: Severity, subject: Option<&str>, line: Option<u32>, message: String) -> Self {
        Finding {
            severity,
            subject: subject.map(str::to_string),
            line,
            message,
        }
    }

    fn warning(subject: &str, line: Option<u32>, message: String) -> Self {
        Finding::new(Severity::Warning, Some(subject), line, message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintReport {
    /// Sorted: errors, then warnings, then hints; within a severity by subject and line.
    pub findings: Vec<Finding>,
    /// Notes whose stored copy was checked.
    pub notes: usize,
    pub strict: bool,
}

impl LintReport {
    pub fn count(&self, severity: Severity) -> usize {
        self.findings.iter().filter(|f| f.severity == severity).count()
    }

    /// Doc 02 §8: errors exit non-zero. With `--strict` the warnings are errors already.
    pub fn failed(&self) -> bool {
        self.count(Severity::Error) > 0
    }

    /// The report as Markdown text: one line per finding, then one summary line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for f in &self.findings {
            let _ = write!(out, "{}: ", f.severity.label());
            if let Some(subject) = &f.subject {
                let _ = write!(out, "{subject}");
                if let Some(line) = f.line {
                    let _ = write!(out, ":{line}");
                }
                out.push_str(": ");
            }
            let _ = writeln!(out, "{}", f.message);
        }
        let _ = writeln!(
            out,
            "lint{}: {}, {}, {}; {} checked",
            if self.strict { " --strict" } else { "" },
            counted(self.count(Severity::Error), "error", "errors"),
            counted(self.count(Severity::Warning), "warning", "warnings"),
            counted(self.count(Severity::Hint), "hint", "hints"),
            counted(self.notes, "note", "notes"),
        );
        out
    }
}

fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// One `[type.<name>]` block of `schemas.toml` (doc 02 §8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeSchema {
    pub required_fields: Vec<String>,
    pub allowed: BTreeMap<String, Vec<String>>,
    pub required_categories: Vec<String>,
    pub required_relations: Vec<String>,
}

/// The type schemas of a vault, by type name as written in the file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schemas {
    pub types: BTreeMap<String, TypeSchema>,
}

/// Reads `schemas.toml` text. `Err` is a file that is not TOML at all. Problems inside a readable file (an unknown
/// key, a value of the wrong kind) are returned next to the schemas that did parse: the bad key is left out, the
/// rest still applies.
pub fn parse_schemas(text: &str) -> std::result::Result<(Schemas, Vec<String>), String> {
    let doc = text.parse::<DocumentMut>().map_err(|e| {
        let line = e
            .span()
            .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
        match line {
            Some(l) => format!("line {l}: {}", e.message()),
            None => e.message().to_string(),
        }
    })?;
    let mut schemas = Schemas::default();
    let mut problems = Vec::new();
    for (key, item) in doc.iter() {
        if key != "type" {
            problems.push(format!(
                "unknown key \"{key}\"; the file holds only [type.<name>] blocks"
            ));
            continue;
        }
        let Some(types) = item.as_table_like() else {
            problems.push("\"type\" must be a table of [type.<name>] blocks".to_string());
            continue;
        };
        for (name, block) in types.iter() {
            let Some(block) = block.as_table_like() else {
                problems.push(format!("[type.{name}] must be a table"));
                continue;
            };
            let mut schema = TypeSchema::default();
            for (k, v) in block.iter() {
                match k {
                    "required_fields" => {
                        set_list(&mut schema.required_fields, v, name, k, &mut problems);
                    }
                    "required_categories" => {
                        set_list(&mut schema.required_categories, v, name, k, &mut problems);
                    }
                    "required_relations" => {
                        set_list(&mut schema.required_relations, v, name, k, &mut problems);
                    }
                    "allowed" => match v.as_table_like() {
                        Some(table) => {
                            for (field, list) in table.iter() {
                                let mut values = Vec::new();
                                if set_list(&mut values, list, name, &format!("allowed.{field}"), &mut problems) {
                                    schema.allowed.insert(field.to_string(), values);
                                }
                            }
                        }
                        None => problems.push(format!(
                            "[type.{name}] allowed must be a table of lists, like {{ status = [\"draft\"] }}"
                        )),
                    },
                    other => problems.push(format!(
                        "[type.{name}] unknown key \"{other}\"; the keys are {}",
                        SCHEMA_KEYS.join(", ")
                    )),
                }
            }
            schemas.types.insert(name.to_string(), schema);
        }
    }
    Ok((schemas, problems))
}

/// Fills `into` from a TOML array of strings. Returns false, with a problem recorded, for anything else.
fn set_list(into: &mut Vec<String>, item: &Item, ty: &str, key: &str, problems: &mut Vec<String>) -> bool {
    let list: Option<Vec<String>> = item
        .as_array()
        .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect());
    match list {
        Some(list) => {
            *into = list;
            true
        }
        None => {
            problems.push(format!("[type.{ty}] {key} must be a list of strings"));
            false
        }
    }
}

/// A frontmatter value as text, for `allowed` and for the messages. `None` for a mapping.
fn plain(v: &serde_yaml::Value) -> Option<String> {
    match v {
        serde_yaml::Value::String(s) => Some(s.trim().to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Null => Some(String::new()),
        serde_yaml::Value::Tagged(t) => plain(&t.value),
        serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_) => None,
    }
}

/// Doc 02 §8, "Empty": a null value, a blank string or an empty list.
fn is_empty(v: &serde_yaml::Value) -> bool {
    match v {
        serde_yaml::Value::Null => true,
        serde_yaml::Value::String(s) => s.trim().is_empty(),
        serde_yaml::Value::Sequence(items) => items.is_empty(),
        serde_yaml::Value::Tagged(t) => is_empty(&t.value),
        _ => false,
    }
}

/// The messages for a note that breaks the schema of its type (doc 02 §8, the four keys). `note` is the parse before
/// the namespace rules: bootstrap and archive notes store no observations or edges, but their text still has them.
pub fn schema_problems(schemas: &Schemas, note: &ParsedNote) -> Vec<String> {
    // Notes with no `type`, or a type no block lists, are not checked.
    let Some(ty) = note.note_type.as_deref() else {
        return Vec::new();
    };
    let Some(schema) = schemas.types.get(ty) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let prefix = format!("schema \"{ty}\"");
    // An empty required field gets its own finding and no `allowed` one (doc 02 §8, "Empty").
    let mut reported: HashSet<&str> = HashSet::new();
    for field in &schema.required_fields {
        match note.frontmatter.get(field.as_str()) {
            None => {
                reported.insert(field);
                out.push(format!("{prefix}: required field \"{field}\" is missing"));
            }
            Some(v) if is_empty(v) => {
                reported.insert(field);
                out.push(format!("{prefix}: required field \"{field}\" is empty"));
            }
            Some(_) => {}
        }
    }
    for (field, allowed) in &schema.allowed {
        let Some(value) = note.frontmatter.get(field.as_str()) else {
            continue;
        };
        if reported.contains(field.as_str()) {
            continue;
        }
        if is_empty(value) {
            out.push(format!("{prefix}: {field} is empty; allowed: {}", allowed.join(", ")));
            continue;
        }
        let values: Vec<Option<String>> = match value {
            serde_yaml::Value::Sequence(items) => items.iter().map(plain).collect(),
            other => vec![plain(other)],
        };
        for v in values {
            if v.as_ref().is_none_or(|s| !allowed.contains(s)) {
                let shown = match &v {
                    Some(s) if s.is_empty() => "empty".to_string(),
                    Some(s) => format!("\"{s}\""),
                    None => "not a plain value".to_string(),
                };
                out.push(format!("{prefix}: {field} is {shown}; allowed: {}", allowed.join(", ")));
            }
        }
    }
    for category in &schema.required_categories {
        let wanted = category.to_lowercase();
        if !note.observations.iter().any(|o| o.category == wanted) {
            out.push(format!("{prefix}: no [{wanted}] observation"));
        }
    }
    for predicate in &schema.required_relations {
        let has = note.links.iter().any(|l| {
            l.relation
                .as_ref()
                .is_some_and(|r| r.predicate.eq_ignore_ascii_case(predicate))
        });
        if !has {
            out.push(format!("{prefix}: no {predicate} relation under ## Relations"));
        }
    }
    out
}

/// Reads `<vault>/.recognitor/schemas.toml`. A missing file is no schemas and no finding. A file that cannot be read
/// or used is an error, because a gate that silently does not run is worse than one that fails.
fn load_schemas(vault: &Path, findings: &mut Vec<Finding>) -> Schemas {
    let error = |message: String| Finding::new(Severity::Error, Some(SCHEMAS_PATH), None, message);
    let text = match fs::read_to_string(vault.join(SCHEMAS_PATH)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Schemas::default(),
        Err(e) => {
            findings.push(error(format!("cannot be read: {e}")));
            return Schemas::default();
        }
    };
    match parse_schemas(&text) {
        Ok((schemas, problems)) => {
            findings.extend(problems.into_iter().map(error));
            schemas
        }
        Err(message) => {
            findings.push(error(format!("is not valid TOML, so no schema is checked: {message}")));
            Schemas::default()
        }
    }
}

/// Runs every check of doc 02 §8 and returns the report. Forces a sweep first, so the index is current.
pub fn lint(index: &mut Index, strict: bool) -> Result<LintReport> {
    let sweep = index.sync(SweepOptions::default())?;
    let mut findings: Vec<Finding> = Vec::new();

    // Check 7: names. Both kinds are errors (doc 07 §6), and the files are not in the index.
    for e in &sweep.name_errors {
        findings.push(Finding::new(
            Severity::Error,
            None,
            None,
            crate::report::name_error_text(e),
        ));
    }
    for w in sweep
        .warnings
        .iter()
        .filter(|w| w.warning.kind == WarningKind::Unreadable)
    {
        findings.push(Finding::warning(&w.note, None, w.warning.message.clone()));
    }

    let schemas = load_schemas(index.vault_root(), &mut findings);
    let conn = index.connection();

    let mut notes = 0;
    let mut namespaces: BTreeMap<String, String> = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT key, path, content FROM notes ORDER BY key")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let path: String = row.get(1)?;
        let content: String = row.get(2)?;
        notes += 1;
        namespaces
            .entry(namespace_of_key(&key).unwrap_or_default().to_string())
            .or_insert_with(|| path.split('/').next().unwrap_or_default().to_string());
        findings.extend(note_findings(conn, &schemas, &path, &content)?);
    }

    namespace_findings(conn, &namespaces, &mut findings)?;

    if strict {
        for f in &mut findings {
            if f.severity == Severity::Warning {
                f.severity = Severity::Error;
            }
        }
    }
    // A stable sort: findings of one note and line keep the order the checks found them in.
    findings.sort_by_cached_key(|f| (f.severity, f.subject.as_deref().map(str::to_lowercase), f.line));
    Ok(LintReport {
        findings,
        notes,
        strict,
    })
}

/// The type schemas as a write result needs them (doc 07 §5.4, "Not checked"): a `schemas.toml` that cannot be read or
/// is not TOML gives no schemas and no finding, because the finding is about the file and `lint` reports it. A bad key
/// inside a readable file is skipped, and the rest applies, as in `lint`.
pub(crate) fn load_schemas_quiet(vault: &Path) -> Schemas {
    let Ok(text) = fs::read_to_string(vault.join(SCHEMAS_PATH)) else {
        return Schemas::default();
    };
    parse_schemas(&text).map(|(schemas, _)| schemas).unwrap_or_default()
}

/// Every check that is about one note (doc 07 §5.4, "Checks"): 1 to 6, the entry point's spelling and a Markdown link's
/// case (8), a link to another namespace without `ns::`, and the type schemas.
///
/// `rel` is the note's path from the vault root and `text` its content, which need not be what the index holds: a write
/// tool asks about the disk text before a write and the new text after it. Links are resolved, and check 6 reads the
/// other notes, in the index as `conn` sees it. The findings carry the note's id as subject and are all warnings.
pub(crate) fn note_findings(conn: &Connection, schemas: &Schemas, rel: &str, text: &str) -> Result<Vec<Finding>> {
    let id = note_id(rel);
    let key = note_key(rel);
    let ns = namespace_of_key(&key).unwrap_or_default();
    let mut findings = Vec::new();

    let mut note = parse_note(text);
    // Archive notes are frozen history: the structural checks run on them, the type schemas do not.
    if ns != ARCHIVE_NAMESPACE {
        for problem in schema_problems(schemas, &note) {
            findings.push(Finding::warning(id, None, problem));
        }
    }
    let status = note.frontmatter.get("status").and_then(plain);
    silence(&mut note, ns);

    // Checks 1, 2, 3, 5 and the empty observation: the parser's own warnings, unchanged.
    for w in &note.warnings {
        findings.push(Finding::warning(id, w.line, w.message.clone()));
    }
    link_findings(conn, &key, id, ns, &note, &mut findings)?;
    markdown_case_findings(conn, rel, id, &note, &mut findings)?;

    // Check 6, first half: `status: superseded` needs an edge that confirms it (doc 03 §3).
    if status.is_some_and(|s| s.eq_ignore_ascii_case("superseded")) && !is_superseded_by_an_edge(conn, &key)? {
        findings.push(Finding::warning(
            id,
            None,
            "status is \"superseded\", but no note links to it with supersedes".to_string(),
        ));
    }
    // Check 8, first half: the entry point of a namespace is spelled `index.md`.
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if key.matches('/').count() == 1 && file_stem(&key) == "index" && name != "index.md" {
        findings.push(Finding::warning(
            id,
            None,
            format!("entry point is spelled \"{name}\"; spell it index.md"),
        ));
    }
    Ok(findings)
}

fn path_of(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT path FROM notes WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?)
}

/// True when some note has a `supersedes` edge that resolves to `key`.
fn is_superseded_by_an_edge(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM edges WHERE target_key = ?1 AND lower(predicate) = 'supersedes' LIMIT 1")?
        .query_row([key], |_| Ok(()))
        .optional()?
        .is_some())
}

/// The frontmatter `status` of a stored note, as text.
fn status_of(conn: &Connection, key: &str) -> Result<Option<String>> {
    let content: Option<String> = conn
        .prepare_cached("SELECT content FROM notes WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?;
    Ok(content.and_then(|c| parse_note(&c).frontmatter.get("status").and_then(plain)))
}

/// Checks 4 and 6 (second half) and the unprefixed cross-namespace link, from the links of one note. Every link is
/// resolved again, because the index stores an ambiguous link as a plain ghost.
fn link_findings(
    conn: &Connection,
    key: &str,
    id: &str,
    ns: &str,
    note: &ParsedNote,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    for edge in edge_rows(key, note).0 {
        let (target_ref, line) = (edge.target_ref, edge.position);
        match resolve(conn, ns, &edge.ref_key)? {
            Resolution::Ambiguous(ids) => {
                findings.push(Finding::warning(id, Some(line), ambiguous_message(&target_ref, &ids)));
            }
            Resolution::Ghost => {}
            Resolution::Resolved { key: target_key, step } => {
                // A name that reached another namespace through step 4 (doc 03 §4, "Writing a link").
                if step == Step::UniqueName && namespace_of_key(&target_key) != Some(ns) {
                    let target = path_of(conn, &target_key)?.unwrap_or_default();
                    let target_id = note_id(&target);
                    let prefix = target_id.split('/').next().unwrap_or_default();
                    findings.push(Finding::warning(
                        id,
                        Some(line),
                        format!(
                            "link \"{target_ref}\" reaches {target_id} in another namespace without a prefix; \
                             write it as [[{prefix}::{target_ref}]]"
                        ),
                    ));
                }
                // Check 6, second half: an edge whose target says otherwise. A target with no status says nothing.
                if edge.predicate.eq_ignore_ascii_case("supersedes")
                    && let Some(status) = status_of(conn, &target_key)?
                    && !status.eq_ignore_ascii_case("superseded")
                {
                    let target = path_of(conn, &target_key)?.map_or(target_key.clone(), |p| note_id(&p).to_string());
                    findings.push(Finding::warning(
                        id,
                        Some(line),
                        format!(
                            "supersedes [[{target_ref}]], but {target} has status \"{status}\", not \"superseded\""
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Check 8, second half: a Markdown link whose case differs from the file's spelling (doc 03 §3). GitHub and Linux
/// tools need the exact case. A link without an extension is compared without one.
fn markdown_case_findings(
    conn: &Connection,
    source_path: &str,
    id: &str,
    note: &ParsedNote,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    for link in note.links.iter().filter(|l| l.syntax == LinkSyntax::Markdown) {
        let decoded = percent_decode(link.target.trim());
        let (stem, extension) = match strip_md_ignore_case(&decoded) {
            Some(stem) => (stem, Some(&decoded[stem.len()..])),
            None => (decoded.as_str(), None),
        };
        let full = match stem.strip_prefix('/') {
            Some(from_root) => join_normalized("", from_root),
            None => join_normalized(parent_dir(source_path), stem),
        };
        let Some(full) = full else { continue };
        let Some(target) = path_of(conn, &full.to_lowercase())? else {
            continue;
        };
        let spelled = note_id(&target);
        let target_extension = &target[spelled.len()..];
        if full != spelled || extension.is_some_and(|e| e != target_extension) {
            findings.push(Finding::warning(
                id,
                Some(link.line),
                format!(
                    "link \"{}\" differs in case from the file {target}; GitHub and Linux tools need the exact case",
                    link.target
                ),
            ));
        }
    }
    Ok(())
}

/// Check 9 and the bootstrap hint (doc 02 §8). `namespaces` maps each namespace key to its folder as spelled on disk.
fn namespace_findings(
    conn: &Connection,
    namespaces: &BTreeMap<String, String>,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    for (ns, spelled) in namespaces {
        if path_of(conn, &format!("{ns}/index"))?.is_some() {
            continue;
        }
        let message = if ns == BOOTSTRAP_NAMESPACE {
            format!("namespace \"{spelled}\" has no index.md, so load_bootstrap returns an error")
        } else {
            format!("namespace \"{spelled}\" has no index.md; load_namespace lists its notes instead")
        };
        findings.push(Finding::new(Severity::Warning, None, None, message));
    }
    if !namespaces.contains_key(BOOTSTRAP_NAMESPACE) {
        findings.push(Finding::new(
            Severity::Hint,
            None,
            None,
            "the vault has no bootstrap/ folder with notes, so load_bootstrap returns an error".to_string(),
        ));
    }
    Ok(())
}

/// One finding as a write result shows it (doc 07 §5.4, "Shape"): the line, when it has one, and the message of §12.2
/// without the note id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFinding {
    pub line: Option<u32>,
    pub message: String,
}

/// The findings a write introduced (doc 07 §5.4, "New"): those of `after` whose message `before` did not have. Line
/// numbers are ignored, so a finding that only moved down the note stays silent, and a message with a number in it
/// counts as new when the number changes. Findings with no line come first, then by line; the order of the checks
/// decides within a line.
pub(crate) fn new_findings(before: &[Finding], after: Vec<Finding>) -> Vec<NoteFinding> {
    let known: HashSet<&str> = before.iter().map(|f| f.message.as_str()).collect();
    let mut fresh: Vec<NoteFinding> = after
        .into_iter()
        .filter(|f| !known.contains(f.message.as_str()))
        .map(|f| NoteFinding {
            line: f.line,
            message: f.message,
        })
        .collect();
    fresh.sort_by_key(|f| f.line);
    fresh
}

/// The lint block of a write result (doc 07 §5.4): empty when there is nothing new, so a clean write costs no tokens.
pub fn render_block(findings: &[NoteFinding]) -> String {
    if findings.is_empty() {
        return String::new();
    }
    let n = findings.len();
    let mut out = format!("lint: {}; the note is saved\n", counted(n, "warning", "warnings"));
    for f in findings {
        match f.line {
            Some(line) => {
                let _ = writeln!(out, "- line {line}: {}", f.message);
            }
            None => {
                let _ = writeln!(out, "- {}", f.message);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC_02_SCHEMA: &str = r#"
[type.concept]
required_fields = ["status", "grade"]
allowed = { status = ["studied", "draft"], grade = ["green", "yellow", "orange"] }
required_categories = ["fact", "use-when", "limit"]
required_relations = ["part_of"]
"#;

    fn schemas() -> Schemas {
        let (schemas, problems) = parse_schemas(DOC_02_SCHEMA).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        schemas
    }

    #[test]
    fn doc_02_schema_example_parses() {
        let s = schemas();
        let c = &s.types["concept"];
        assert_eq!(c.required_fields, ["status", "grade"]);
        assert_eq!(c.allowed["status"], ["studied", "draft"]);
        assert_eq!(c.allowed["grade"], ["green", "yellow", "orange"]);
        assert_eq!(c.required_categories, ["fact", "use-when", "limit"]);
        assert_eq!(c.required_relations, ["part_of"]);
    }

    #[test]
    fn a_note_that_meets_the_schema_has_no_problem() {
        let note = parse_note(
            "---\ntype: concept\nstatus: studied\ngrade: green\n---\n# T\n\nS.\n\n- [fact] a\n- [use-when] b\n- [limit] c\n\n## Relations\n- part_of [[X]]\n",
        );
        assert_eq!(schema_problems(&schemas(), &note), Vec::<String>::new());
    }

    #[test]
    fn each_key_fails_as_the_doc_02_table_says() {
        let note = parse_note("---\ntype: concept\ngrade: purple\n---\n# T\n\nS.\n\n- [fact] a\n- [use-when] b\n");
        assert_eq!(
            schema_problems(&schemas(), &note),
            [
                "schema \"concept\": required field \"status\" is missing",
                "schema \"concept\": grade is \"purple\"; allowed: green, yellow, orange",
                "schema \"concept\": no [limit] observation",
                "schema \"concept\": no part_of relation under ## Relations",
            ]
        );
    }

    #[test]
    fn notes_with_no_type_or_an_unlisted_type_are_not_checked() {
        assert!(schema_problems(&schemas(), &parse_note("# T\n\nS.\n")).is_empty());
        assert!(schema_problems(&schemas(), &parse_note("---\ntype: other\n---\n# T\n")).is_empty());
    }

    #[test]
    fn schema_file_problems_are_listed_and_the_rest_still_applies() {
        let (schemas, problems) = parse_schemas(
            "[type.a]\nrequired_field = [\"x\"]\nrequired_fields = \"x\"\nallowed = [\"x\"]\nrequired_relations = [\"p\"]\n\
             [type.b]\nallowed = { status = \"draft\" }\n[other]\nx = 1\n",
        )
        .unwrap();
        assert_eq!(
            problems,
            [
                "[type.a] unknown key \"required_field\"; the keys are required_fields, allowed, required_categories, required_relations",
                "[type.a] required_fields must be a list of strings",
                "[type.a] allowed must be a table of lists, like { status = [\"draft\"] }",
                "[type.b] allowed.status must be a list of strings",
                "unknown key \"other\"; the file holds only [type.<name>] blocks",
            ]
        );
        assert_eq!(schemas.types["a"].required_relations, ["p"]);
    }

    #[test]
    fn invalid_toml_names_its_line() {
        let err = parse_schemas("[type.a]\nrequired_fields = [\n").unwrap_err();
        assert!(err.starts_with("line "), "{err}");
    }

    #[test]
    fn render_summary_lines() {
        let clean = LintReport {
            findings: Vec::new(),
            notes: 1,
            strict: false,
        };
        assert_eq!(clean.render(), "lint: 0 errors, 0 warnings, 0 hints; 1 note checked\n");
        let strict = LintReport {
            findings: vec![Finding::new(Severity::Error, Some("a/b"), Some(3), "m".to_string())],
            notes: 2,
            strict: true,
        };
        assert_eq!(
            strict.render(),
            "error: a/b:3: m\nlint --strict: 1 error, 0 warnings, 0 hints; 2 notes checked\n"
        );
        assert!(strict.failed());
    }
}

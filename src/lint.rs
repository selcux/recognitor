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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use toml_edit::{DocumentMut, Item};

use crate::error::Result;
use crate::index::{Index, SweepOptions};
use crate::parse::{LinkSyntax, ParsedNote, WarningKind, parse_note, silence};
use crate::paths::{
    ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, join_normalized, namespace_of_key, note_id, parent_dir, percent_decode,
    strip_md_ignore_case,
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

/// A `supersedes` edge whose target resolved (check 6).
struct Supersedes {
    source: String,
    line: u32,
    target_ref: String,
    target_key: String,
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

    let mut statuses: HashMap<String, String> = HashMap::new();
    let mut supersedes: Vec<Supersedes> = Vec::new();
    let mut confirmed: HashSet<String> = HashSet::new();
    let mut notes = 0;
    let mut namespaces: BTreeMap<String, String> = BTreeMap::new();

    let mut stmt = conn.prepare("SELECT key, path, content FROM notes ORDER BY key")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let path: String = row.get(1)?;
        let content: String = row.get(2)?;
        notes += 1;
        let id = note_id(&path).to_string();
        let ns = namespace_of_key(&key).unwrap_or_default().to_string();
        namespaces
            .entry(ns.clone())
            .or_insert_with(|| path.split('/').next().unwrap_or_default().to_string());

        let mut note = parse_note(&content);
        // Archive notes are frozen history: the structural checks run on them, the type schemas do not.
        if ns != ARCHIVE_NAMESPACE {
            for problem in schema_problems(&schemas, &note) {
                findings.push(Finding::warning(&id, None, problem));
            }
        }
        if let Some(status) = note.frontmatter.get("status").and_then(plain) {
            statuses.insert(key.clone(), status);
        }
        silence(&mut note, &ns);

        // Checks 1, 2, 3, 5 and the empty observation: the parser's own warnings, unchanged.
        for w in &note.warnings {
            findings.push(Finding::warning(&id, w.line, w.message.clone()));
        }
        link_findings(conn, &key, &id, &ns, &mut findings, &mut supersedes, &mut confirmed)?;
        markdown_case_findings(conn, &path, &id, &note, &mut findings)?;
    }

    // Check 6: `status: superseded` against the `supersedes` edges (doc 03 §3).
    let mut stated: Vec<&String> = statuses
        .iter()
        .filter(|(_, s)| s.eq_ignore_ascii_case("superseded"))
        .map(|(k, _)| k)
        .collect();
    stated.sort();
    for key in stated {
        if !confirmed.contains(key) {
            let id = path_of(conn, key)?.map(|p| note_id(&p).to_string()).unwrap_or_default();
            findings.push(Finding::warning(
                &id,
                None,
                "status is \"superseded\", but no note links to it with supersedes".to_string(),
            ));
        }
    }
    for edge in &supersedes {
        let Some(status) = statuses.get(&edge.target_key) else {
            continue;
        };
        if !status.eq_ignore_ascii_case("superseded") {
            let target = path_of(conn, &edge.target_key)?.map_or(edge.target_key.clone(), |p| note_id(&p).to_string());
            findings.push(Finding::warning(
                &edge.source,
                Some(edge.line),
                format!(
                    "supersedes [[{}]], but {target} has status \"{status}\", not \"superseded\"",
                    edge.target_ref
                ),
            ));
        }
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

fn path_of(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT path FROM notes WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?)
}

/// Checks 4 and the unprefixed cross-namespace link, from the stored edges of one note. Every link is resolved again,
/// because the index stores an ambiguous link as a plain ghost. Also collects the `supersedes` edges for check 6.
fn link_findings(
    conn: &Connection,
    key: &str,
    id: &str,
    ns: &str,
    findings: &mut Vec<Finding>,
    supersedes: &mut Vec<Supersedes>,
    confirmed: &mut HashSet<String>,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "SELECT predicate, target_ref, ref_key, position FROM edges WHERE source_key = ?1 ORDER BY position",
    )?;
    let edges = stmt
        .query_map([key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (predicate, target_ref, ref_key, line) in edges {
        match resolve(conn, ns, &ref_key)? {
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
                if predicate.eq_ignore_ascii_case("supersedes") {
                    confirmed.insert(target_key.clone());
                    supersedes.push(Supersedes {
                        source: id.to_string(),
                        line,
                        target_ref,
                        target_key,
                    });
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

/// Check 8, first half, check 9 and the bootstrap hint (doc 02 §8). `namespaces` maps each namespace key to its
/// folder as spelled on disk.
fn namespace_findings(
    conn: &Connection,
    namespaces: &BTreeMap<String, String>,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    for (ns, spelled) in namespaces {
        match path_of(conn, &format!("{ns}/index"))? {
            None if ns == BOOTSTRAP_NAMESPACE => findings.push(Finding::new(
                Severity::Warning,
                None,
                None,
                format!("namespace \"{spelled}\" has no index.md, so load_bootstrap returns an error"),
            )),
            None => findings.push(Finding::new(
                Severity::Warning,
                None,
                None,
                format!("namespace \"{spelled}\" has no index.md; load_namespace lists its notes instead"),
            )),
            Some(path) => {
                let name = path.rsplit('/').next().unwrap_or(&path);
                if name != "index.md" {
                    findings.push(Finding::warning(
                        note_id(&path),
                        None,
                        format!("entry point is spelled \"{name}\"; spell it index.md"),
                    ));
                }
            }
        }
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

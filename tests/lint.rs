//! `recognitor lint [--strict]` tests (doc 02 §8, doc 03 §2-4, doc 07 §3, §6, §12). The docs give the checks, the
//! schemas and the exit codes but no output format, so the line shapes asserted here are the provisional ones of
//! `src/lint.rs`. Vaults are synthetic and live in temp folders that delete themselves.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    base: PathBuf,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }

    /// Every line but the summary.
    fn findings(&self) -> Vec<&str> {
        let mut lines = self.lines();
        lines.pop();
        lines
    }

    fn summary(&self) -> &str {
        self.stdout.lines().last().unwrap_or("")
    }
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-lint-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let vault = TestVault { root, base };
        for (rel, text) in files {
            vault.write(rel, text);
        }
        vault
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .arg("--vault")
            .arg(&self.root)
            .env("RECOGNITOR_DB", self.base.join("index.db"))
            .env("RECOGNITOR_CONFIG", self.base.join("config.toml"))
            .env_remove("RECOGNITOR_VAULT")
            .output()
            .unwrap();
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn lint(&self) -> Run {
        self.run(&["lint"])
    }

    fn strict(&self) -> Run {
        self.run(&["lint", "--strict"])
    }

    /// Every file under the vault with its bytes, to prove a command wrote nothing.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    let bytes = fs::read(&p).unwrap();
                    out.push((p, bytes));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

const INDEX: &str = "# Index\n\nThe entry point.\n";

/// A vault with a bootstrap and one knowledge namespace, both with entry points, and nothing to report.
fn clean() -> TestVault {
    TestVault::new(&[
        ("bootstrap/index.md", INDEX),
        ("design/index.md", INDEX),
        ("design/Caching.md", "# Caching\n\nA cache keeps answers close.\n"),
    ])
}

fn with(files: &[(&str, &str)]) -> TestVault {
    let v = clean();
    for (rel, text) in files {
        v.write(rel, text);
    }
    v
}

#[test]
fn a_clean_vault_prints_only_the_summary_and_exits_0() {
    let v = clean();
    let r = v.lint();
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(r.stdout, "lint: 0 errors, 0 warnings, 0 hints; 3 notes checked\n");
    assert_eq!(r.stderr, "");
    let r = v.strict();
    assert_eq!(r.code, Some(0));
    assert_eq!(
        r.stdout,
        "lint --strict: 0 errors, 0 warnings, 0 hints; 3 notes checked\n"
    );
}

#[test]
fn lint_writes_no_note_file() {
    let v = with(&[("design/Bad.md", "---\ntitle: [oops\n---\n# Bad\n")]);
    let before = v.snapshot();
    let _ = v.lint();
    let _ = v.strict();
    assert_eq!(v.snapshot(), before);
}

#[test]
fn a_missing_bootstrap_is_a_hint_even_with_strict() {
    let v = TestVault::new(&[("design/index.md", INDEX)]);
    for r in [v.lint(), v.strict()] {
        assert_eq!(r.code, Some(0), "{}", r.stdout);
        assert_eq!(
            r.findings(),
            ["hint: the vault has no bootstrap/ folder with notes, so load_bootstrap returns an error"]
        );
    }
}

#[test]
fn check_9_a_namespace_without_an_entry_point_is_a_warning() {
    let v = with(&[("Shelf/Alpha.md", "# Alpha\n\nA.\n")]);
    let r = v.lint();
    assert_eq!(r.code, Some(0));
    assert_eq!(
        r.findings(),
        ["warning: namespace \"Shelf\" has no index.md; load_namespace lists its notes instead"]
    );
    let r = v.strict();
    assert_eq!(r.code, Some(1));
    assert_eq!(
        r.findings(),
        ["error: namespace \"Shelf\" has no index.md; load_namespace lists its notes instead"]
    );
    assert_eq!(
        r.summary(),
        "lint --strict: 1 error, 0 warnings, 0 hints; 4 notes checked"
    );
}

#[test]
fn a_bootstrap_folder_without_an_entry_point_is_a_warning_not_the_hint() {
    let v = TestVault::new(&[
        ("bootstrap/Profile.md", "# Profile\n\nWho.\n"),
        ("design/index.md", INDEX),
    ]);
    assert_eq!(
        v.lint().findings(),
        ["warning: namespace \"bootstrap\" has no index.md, so load_bootstrap returns an error"]
    );
}

#[test]
fn check_8_an_entry_point_not_spelled_index_md_is_a_warning() {
    let v = TestVault::new(&[("bootstrap/index.md", INDEX), ("design/Index.md", INDEX)]);
    assert_eq!(
        v.lint().findings(),
        ["warning: design/Index: entry point is spelled \"Index.md\"; spell it index.md"]
    );
}

#[test]
fn check_5_a_long_summary_warns_with_its_line() {
    let long = "word ".repeat(70);
    let v = with(&[("design/Long.md", &format!("# Long\n\n{long}\n"))]);
    let r = v.lint();
    assert_eq!(r.code, Some(0));
    assert_eq!(
        r.findings(),
        ["warning: design/Long:3: summary paragraph is 349 characters; the limit is 300"]
    );
}

#[test]
fn checks_1_to_3_come_from_the_parser() {
    let v = with(&[(
        "design/Messy.md",
        "---\ntitle: [oops\n---\n\
         # Messy\n\nA summary.\n\n\
         - [decision] @someday Later.\n\
         - [fact] @2026-06-10\n\n\
         ## Relations\n\
         - see also [[Caching]]\n\
         - relates_to [[Caching]]\n",
    )]);
    let r = v.lint();
    assert_eq!(r.code, Some(0), "{}", r.stdout);
    let lines = r.findings();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(
        lines[0].starts_with("warning: design/Messy:1: frontmatter is not valid YAML"),
        "{lines:?}"
    );
    assert_eq!(
        lines[1],
        "warning: design/Messy:8: qualifier \"@someday\" does not parse; it stays text"
    );
    assert_eq!(
        lines[2],
        "warning: design/Messy:9: observation \"[fact]\" has no text; it stays a plain list item"
    );
    assert!(
        lines[3].starts_with("warning: design/Messy:12: line under \"## Relations\" does not match"),
        "{lines:?}"
    );
}

#[test]
fn a_no_change_sync_still_lets_lint_see_every_warning() {
    let v = with(&[(
        "design/Messy.md",
        "# Messy\n\nA summary.\n\n## Relations\n- [[Caching]]\n",
    )]);
    let first = v.lint();
    assert_eq!(first.findings().len(), 1, "{}", first.stdout);
    // Nothing changed: the sweep reads no file, and lint reports the same line.
    let second = v.lint();
    assert_eq!(second.stdout, first.stdout);
}

#[test]
fn check_4_an_ambiguous_link_warns_with_both_ids() {
    let v = with(&[
        ("ops/index.md", INDEX),
        ("ops/Setup.md", "# Setup\n\nOps.\n"),
        ("dev/index.md", INDEX),
        ("dev/Setup.md", "# Setup\n\nDev.\n"),
        ("design/Links.md", "# Links\n\nSee [[Setup]].\n"),
    ]);
    assert_eq!(
        v.lint().findings(),
        ["warning: design/Links:3: link \"Setup\" matches 2 notes: dev/Setup, ops/Setup; stored as a ghost"]
    );
}

#[test]
fn a_link_into_another_namespace_needs_its_prefix() {
    let v = with(&[
        ("ops/index.md", INDEX),
        (
            "ops/Runbook.md",
            "# Runbook\n\nSee [[Caching]] and [[design::Caching]].\n",
        ),
    ]);
    assert_eq!(
        v.lint().findings(),
        [
            "warning: ops/Runbook:3: link \"Caching\" reaches design/Caching in another namespace without a prefix; write it as [[design::Caching]]"
        ]
    );
}

#[test]
fn check_6_status_superseded_needs_a_supersedes_edge() {
    let v = with(&[
        ("design/Old.md", "---\nstatus: superseded\n---\n# Old\n\nOld.\n"),
        ("design/Older.md", "---\nstatus: superseded\n---\n# Older\n\nOlder.\n"),
        (
            "design/New.md",
            "# New\n\nNew.\n\n## Relations\n- supersedes [[Older]]\n",
        ),
    ]);
    assert_eq!(
        v.lint().findings(),
        ["warning: design/Old: status is \"superseded\", but no note links to it with supersedes"]
    );
}

#[test]
fn check_6_an_edge_whose_target_says_otherwise_warns_and_a_silent_target_does_not() {
    let v = with(&[
        ("design/Active.md", "---\nstatus: active\n---\n# Active\n\nA.\n"),
        ("design/Plain.md", "# Plain\n\nNo status at all.\n"),
        (
            "design/New.md",
            "# New\n\nNew.\n\n## Relations\n- supersedes [[Active]]\n- supersedes [[Plain]]\n",
        ),
    ]);
    assert_eq!(
        v.lint().findings(),
        ["warning: design/New:6: supersedes [[Active]], but design/Active has status \"active\", not \"superseded\""]
    );
}

#[test]
fn check_8_a_markdown_link_whose_case_differs_warns() {
    let v = with(&[(
        "design/Links.md",
        "# Links\n\n\
             [exact](Caching.md) [wrong](caching.md) [no ext](caching) [root](/design/CACHING.md) [folder](../Design/Caching.md)\n",
    )]);
    let r = v.lint();
    let lines = r.findings();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(
        lines[0],
        "warning: design/Links:3: link \"caching.md\" differs in case from the file design/Caching.md; GitHub and Linux tools need the exact case"
    );
    assert!(lines[1].contains("link \"caching\" differs"), "{lines:?}");
    assert!(lines[2].contains("link \"/design/CACHING.md\" differs"), "{lines:?}");
    assert!(lines[3].contains("link \"../Design/Caching.md\" differs"), "{lines:?}");
}

#[test]
fn check_7_a_non_ascii_name_is_an_error_and_plain_lint_fails() {
    let v = with(&[("design/K\u{131}rm\u{131}z\u{131}.md", "# K\n\nx.\n")]);
    let r = v.lint();
    assert_eq!(r.code, Some(1), "{}", r.stdout);
    assert_eq!(
        r.findings(),
        ["error: non-ASCII name, not indexed: design/K\u{131}rm\u{131}z\u{131}.md (1 notes left out)"]
    );
    assert_eq!(r.stderr, "");
    assert_eq!(r.summary(), "lint: 1 error, 0 warnings, 0 hints; 3 notes checked");
}

/// True when `A` and `a` are different files here (Windows and macOS usually say no).
fn case_sensitive(v: &TestVault) -> bool {
    v.write("probe/a.md", "x");
    v.write("probe/A.md", "x");
    let n = fs::read_dir(v.root.join("probe")).unwrap().count();
    let _ = fs::remove_dir_all(v.root.join("probe"));
    n == 2
}

#[test]
fn check_7_case_twins_are_an_error_and_plain_lint_fails() {
    let v = clean();
    if !case_sensitive(&v) {
        return;
    }
    v.write("design/Note.md", "# Note\n\nA.\n");
    v.write("design/note.md", "# note\n\nB.\n");
    let r = v.lint();
    assert_eq!(r.code, Some(1), "{}", r.stdout);
    assert_eq!(
        r.findings(),
        ["error: case twins, none of them indexed: design/Note.md, design/note.md (2 notes left out)"]
    );
}

#[test]
fn errors_come_first_then_warnings_then_hints() {
    let v = TestVault::new(&[
        ("Shelf/Alpha.md", "# Alpha\n\nA.\n"),
        ("Shelf/K\u{131}r.md", "# K\n\nx.\n"),
    ]);
    let r = v.lint();
    let kinds: Vec<&str> = r.findings().iter().map(|l| l.split(':').next().unwrap()).collect();
    assert_eq!(kinds, ["error", "warning", "hint"], "{}", r.stdout);
    assert_eq!(r.code, Some(1));
}

const SCHEMAS: &str = r#"
[type.concept]
required_fields = ["status", "grade"]
allowed = { status = ["studied", "draft"], grade = ["green", "yellow", "orange"] }
required_categories = ["fact", "use-when", "limit"]
required_relations = ["part_of"]
"#;

const CONCEPT_OK: &str = "---\ntype: concept\nstatus: studied\ngrade: green\n---\n# Ok\n\nOk.\n\n\
                          - [fact] a\n- [use-when] b\n- [limit] c\n\n## Relations\n- part_of [[Caching]]\n";

#[test]
fn schemas_a_conforming_note_passes_and_a_broken_one_warns() {
    let v = with(&[
        (".recognitor/schemas.toml", SCHEMAS),
        ("design/Ok.md", CONCEPT_OK),
        (
            "design/Bad.md",
            "---\ntype: concept\ngrade: purple\n---\n# Bad\n\nBad.\n\n- [fact] a\n- [use-when] b\n",
        ),
        ("design/Free.md", "---\ntype: free\n---\n# Free\n\nNot listed.\n"),
    ]);
    let r = v.lint();
    assert_eq!(r.code, Some(0), "{}", r.stdout);
    assert_eq!(
        r.findings(),
        [
            "warning: design/Bad: schema \"concept\": required field \"status\" is missing",
            "warning: design/Bad: schema \"concept\": grade is \"purple\"; allowed: green, yellow, orange",
            "warning: design/Bad: schema \"concept\": no [limit] observation",
            "warning: design/Bad: schema \"concept\": no part_of relation under ## Relations",
        ]
    );
    let r = v.strict();
    assert_eq!(r.code, Some(1));
    assert!(r.findings().iter().all(|l| l.starts_with("error: design/Bad:")));
}

#[test]
fn schemas_apply_to_the_bootstrap_too() {
    let v = with(&[
        (".recognitor/schemas.toml", SCHEMAS),
        ("bootstrap/Persona.md", "---\ntype: concept\n---\n# Persona\n\nWho.\n"),
    ]);
    let lines = v.lint().stdout;
    assert!(
        lines.contains("warning: bootstrap/Persona: schema \"concept\": required field \"status\" is missing"),
        "{lines}"
    );
}

#[test]
fn schemas_read_the_text_of_bootstrap_notes_whose_observations_are_not_stored() {
    let v = with(&[
        (
            ".recognitor/schemas.toml",
            "[type.record]
required_categories = [\"fact\"]
",
        ),
        (
            "bootstrap/Rule.md",
            "---
type: record
---
# Rule

R.

- [fact] it holds
",
        ),
    ]);
    assert_eq!(v.lint().findings(), Vec::<&str>::new());
}

#[test]
fn schemas_skip_archive_notes_but_the_structural_checks_still_run() {
    let long = "word ".repeat(70);
    let v = with(&[
        (".recognitor/schemas.toml", SCHEMAS),
        ("archive/index.md", INDEX),
        (
            "archive/2026-11-01 session.md",
            &format!(
                "---
type: concept
---
# Session

{long}
"
            ),
        ),
    ]);
    assert_eq!(
        v.lint().findings(),
        ["warning: archive/2026-11-01 session:6: summary paragraph is 349 characters; the limit is 300"]
    );
}

#[test]
fn an_empty_required_field_gives_exactly_one_line() {
    let schemas = "[type.concept]
required_fields = [\"status\"]
allowed = { status = [\"studied\", \"draft\"] }
";
    for (name, value) in [("null", ""), ("blank", " \"  \""), ("list", " []")] {
        let v = with(&[
            (".recognitor/schemas.toml", schemas),
            (
                "design/Note.md",
                &format!(
                    "---
type: concept
status:{value}
---
# Note

N.
"
                ),
            ),
        ]);
        assert_eq!(
            v.lint().findings(),
            ["warning: design/Note: schema \"concept\": required field \"status\" is empty"],
            "{name}"
        );
    }
}

#[test]
fn an_empty_field_that_is_not_required_still_fails_allowed() {
    let schemas = "[type.concept]
allowed = { status = [\"studied\", \"draft\"] }
";
    for value in ["", " \"  \"", " []"] {
        let v = with(&[
            (".recognitor/schemas.toml", schemas),
            (
                "design/Note.md",
                &format!(
                    "---
type: concept
status:{value}
---
# Note

N.
"
                ),
            ),
        ]);
        assert_eq!(
            v.lint().findings(),
            ["warning: design/Note: schema \"concept\": status is empty; allowed: studied, draft"]
        );
    }
}

#[test]
fn a_missing_schemas_file_is_fine() {
    assert_eq!(clean().lint().code, Some(0));
}

#[test]
fn an_invalid_schemas_file_is_an_error() {
    let v = with(&[(".recognitor/schemas.toml", "[type.concept]\nrequired_fields = [\n")]);
    let r = v.lint();
    assert_eq!(r.code, Some(1));
    let lines = r.findings();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("error: .recognitor/schemas.toml: is not valid TOML, so no schema is checked: line "),
        "{lines:?}"
    );
}

#[test]
fn schema_keys_and_values_that_cannot_be_used_are_errors() {
    let v = with(&[(
        ".recognitor/schemas.toml",
        "[type.concept]\nrequired_field = [\"x\"]\nallowed = [\"x\"]\n[type.other]\nallowed = { status = \"draft\" }\n",
    )]);
    let r = v.lint();
    assert_eq!(r.code, Some(1));
    assert_eq!(
        r.findings(),
        [
            "error: .recognitor/schemas.toml: [type.concept] unknown key \"required_field\"; the keys are required_fields, allowed, required_categories, required_relations",
            "error: .recognitor/schemas.toml: [type.concept] allowed must be a table of lists, like { status = [\"draft\"] }",
            "error: .recognitor/schemas.toml: [type.other] allowed.status must be a list of strings",
        ]
    );
}

#[test]
fn archive_notes_are_graph_silent_so_lint_finds_no_link_or_relation_problem_there() {
    let v = with(&[(
        "archive/2026-11-01 session.md",
        "# Session\n\nTranscript.\n\n- [fact] @someday x\n\n## Relations\n- see also [[Nowhere]]\n",
    )]);
    // Only the entry-point warning of the new namespace remains.
    assert_eq!(
        v.lint().findings(),
        ["warning: namespace \"archive\" has no index.md; load_namespace lists its notes instead"]
    );
}

#[test]
fn usage_errors_exit_2() {
    let v = clean();
    let r = v.run(&["sync", "--strict"]);
    assert_eq!(r.code, Some(2));
    assert!(
        r.stderr.starts_with("error: --strict belongs to lint\n"),
        "{}",
        r.stderr
    );
    assert_eq!(r.stdout, "");
    let r = v.run(&["lint", "extra"]);
    assert_eq!(r.code, Some(2));
    assert!(
        r.stderr.starts_with("error: unexpected argument: extra\n"),
        "{}",
        r.stderr
    );
}

#[test]
fn without_a_vault_lint_fails_like_every_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .arg("lint")
        .env(
            "RECOGNITOR_CONFIG",
            std::env::temp_dir().join("recognitor-lint-no-config.toml"),
        )
        .env_remove("RECOGNITOR_VAULT")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(out.stdout, b"");
}

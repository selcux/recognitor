//! Lint lines in write results (doc 07 §5.4). The tools are driven through the library, whose `render` functions give
//! the text the MCP tools return. Vaults are synthetic and live in temp folders that delete themselves.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::edit_note::{Op, render as render_edit};
use recognitor::hash::sha256_prefix;
use recognitor::{Index, delete_note, move_note, relink, write_note};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

const LIMIT: usize = 4000;

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-write-lint-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let v = TestVault {
            root,
            db: base.join("index.db"),
        };
        for (rel, text) in files {
            v.put(rel, text);
        }
        v
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn file(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn index(&self) -> Index {
        Index::open(&self.root, Some(&self.db)).unwrap()
    }

    fn checksum(&self, rel: &str) -> String {
        sha256_prefix(&fs::read(self.root.join(rel)).unwrap(), 12)
    }

    fn create(&self, path: &str, content: &str) -> String {
        write_note::render(&self.index().write_note(path, content, None, LIMIT).unwrap())
    }

    fn overwrite(&self, path: &str, content: &str) -> String {
        let sum = self.checksum(&format!("{path}.md"));
        write_note::render(&self.index().write_note(path, content, Some(&sum), LIMIT).unwrap())
    }

    fn edit(&self, note: &str, op: &Op) -> String {
        render_edit(&self.index().edit_note(note, op, LIMIT).unwrap())
    }

    fn move_to(&self, note: &str, rel: &str, new_path: &str) -> String {
        let sum = self.checksum(rel);
        move_note::render(&self.index().move_note(note, new_path, &sum, LIMIT).unwrap())
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

/// A summary of 349 characters, over the limit of 300.
fn long(words: usize) -> String {
    "word ".repeat(words).trim_end().to_string()
}

fn long_message(words: usize) -> String {
    format!("summary paragraph is {} characters; the limit is 300", words * 5 - 1)
}

fn result_line(id: &str, content: &str) -> String {
    format!("{id}  checksum: {}\n", sha256_prefix(content.as_bytes(), 12))
}

const CONCEPT_SCHEMA: &str = "[type.concept]\nrequired_categories = [\"limit\"]\n";

#[test]
fn a_create_shows_every_finding_without_line_first() {
    let v = TestVault::new(&[(".recognitor/schemas.toml", CONCEPT_SCHEMA)]);
    let content = format!("---\ntype: concept\n---\n# Caching\n\n{}\n", long(70));
    assert_eq!(
        v.create("design/Caching", &content),
        format!(
            "{}lint: 2 warnings; the note is saved\n\
             - schema \"concept\": no [limit] observation\n\
             - line 6: {}\n",
            result_line("design/Caching", &content),
            long_message(70)
        )
    );
}

#[test]
fn one_finding_says_warning_and_a_clean_write_has_no_block() {
    let v = TestVault::new(&[]);
    let bad = format!("# A\n\n{}\n", long(70));
    assert_eq!(
        v.create("design/A", &bad),
        format!(
            "{}lint: 1 warning; the note is saved\n- line 3: {}\n",
            result_line("design/A", &bad),
            long_message(70)
        )
    );
    let clean = "# B\n\nShort.\n";
    assert_eq!(v.create("design/B", clean), result_line("design/B", clean));
}

#[test]
fn an_overwrite_shows_only_new_findings_and_ignores_line_numbers() {
    let old = format!("# A\n\n{}\n\n- [fact] @someday later\n", long(70));
    let v = TestVault::new(&[("design/A.md", &old)]);
    // The same two findings, two lines further down: silent.
    let same = format!(
        "# A\n\n{}\n\nA new paragraph.\n\nAnother one.\n\n- [fact] @someday later\n",
        long(70)
    );
    assert_eq!(v.overwrite("design/A", &same), result_line("design/A", &same));
    // The summary got longer, so its message changed: shown again, with the new number. The old qualifier stays silent.
    let longer = format!("# A\n\n{}\n\n- [fact] @someday later\n", long(80));
    assert_eq!(
        v.overwrite("design/A", &longer),
        format!(
            "{}lint: 1 warning; the note is saved\n- line 3: {}\n",
            result_line("design/A", &longer),
            long_message(80)
        )
    );
    // A new bad qualifier is new.
    let worse = format!("{longer}- [fact] @never again\n");
    let out = v.overwrite("design/A", &worse);
    assert!(
        out.contains("lint: 1 warning; the note is saved\n- line 6: qualifier \"@never\" does not parse"),
        "{out}"
    );
}

#[test]
fn an_edit_shows_only_new_findings() {
    let old = format!("# A\n\n{}\n", long(70));
    let v = TestVault::new(&[("design/A.md", &old)]);
    // A good observation: the old long summary stays silent.
    let ok = v.edit(
        "design/A",
        &Op::AddObservation {
            text: "[fact] fine".into(),
        },
    );
    assert!(ok.starts_with("design/A  checksum: ") && !ok.contains("lint:"), "{ok}");
    // A bad one: only its own finding shows, with its line in the file.
    let out = v.edit(
        "design/A",
        &Op::AddObservation {
            text: "[fact] @someday later".into(),
        },
    );
    let line = v
        .file("design/A.md")
        .lines()
        .position(|l| l.contains("@someday"))
        .unwrap()
        + 1;
    assert!(
        out.ends_with(&format!(
            "lint: 1 warning; the note is saved\n- line {line}: qualifier \"@someday\" does not parse; it stays text\n"
        )),
        "{out}"
    );
    assert!(!out.contains("summary paragraph"), "{out}");
}

#[test]
fn a_move_into_another_namespace_shows_the_new_missing_prefix() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", "# Caching\n\nA cache.\n"),
        ("ideas/Notes.md", "# Notes\n\nSee [[Caching]].\n"),
    ]);
    assert_eq!(
        v.move_to("ideas/Notes", "ideas/Notes.md", "ops/Notes"),
        "moved: ideas/Notes -> ops/Notes\n\
         broken links: none\n\
         lint: 1 warning; the note is saved\n\
         - line 3: link \"Caching\" reaches ideas/Caching in another namespace without a prefix; \
         write it as [[ideas::Caching]]\n"
    );
    // Back into the namespace of its link: nothing new.
    let back = v.move_to("ops/Notes", "ops/Notes.md", "ideas/Notes");
    assert!(!back.contains("lint:"), "{back}");
}

#[test]
fn a_move_keeps_old_findings_silent() {
    let v = TestVault::new(&[("ideas/A.md", &format!("# A\n\n{}\n", long(70)))]);
    let out = v.move_to("ideas/A", "ideas/A.md", "ops/A");
    assert_eq!(out, "moved: ideas/A -> ops/A\nbroken links: none\n");
}

#[test]
fn no_block_without_a_write() {
    let long_note = format!("# A\n\n{}\n", long(70));
    let v = TestVault::new(&[
        ("design/A.md", &long_note),
        (
            "ops/Runbook.md",
            &format!("# Runbook\n\n{}\n\nSee [[design::A]].\n", long(70)),
        ),
    ]);
    // Already there.
    let line = "relates_to [[design::A]]";
    let first = v.edit("ops/Runbook", &Op::AddRelation { text: line.into() });
    assert!(!first.contains("lint:"), "{first}");
    let again = v.edit("ops/Runbook", &Op::AddRelation { text: line.into() });
    assert!(
        again.starts_with("Already there, nothing was written.\n") && !again.contains("lint:"),
        "{again}"
    );
    // A refusal.
    let refused = write_note::render(&v.index().write_note("design/A", &long_note, None, LIMIT).unwrap());
    assert!(
        refused.starts_with("The note already exists") && !refused.contains("lint:"),
        "{refused}"
    );
    // relink and delete_note.
    let moved = v.move_to("design/A", "design/A.md", "design/B");
    assert!(!moved.contains("lint:"), "{moved}");
    let relinked = relink::render(
        &v.index()
            .relink("design/A", "design/B", &["ops/Runbook".to_string()])
            .unwrap(),
    );
    assert!(!relinked.contains("lint:"), "{relinked}");
    let sum = v.checksum("design/B.md");
    let deleted = delete_note::render(&v.index().delete_note("design/B", &sum, LIMIT).unwrap());
    assert!(!deleted.contains("lint:"), "{deleted}");
}

#[test]
fn the_block_comes_before_the_bootstrap_notice() {
    let v = TestVault::new(&[]);
    let content = format!("# Rule\n\n{}\n", long(70));
    assert_eq!(
        v.create("bootstrap/Rule", &content),
        format!(
            "{}lint: 1 warning; the note is saved\n- line 3: {}\n\
             notice: bootstrap/Rule loads as instructions at session start\n",
            result_line("bootstrap/Rule", &content),
            long_message(70)
        )
    );
}

#[test]
fn a_move_into_the_bootstrap_puts_the_block_before_the_notice() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", "# Caching\n\nA cache.\n"),
        ("ideas/Notes.md", "# Notes\n\nSee [[Caching]].\n"),
    ]);
    let out = v.move_to("ideas/Notes", "ideas/Notes.md", "bootstrap/Notes");
    let lint = out.find("lint: 1 warning").unwrap();
    let notice = out.find("notice: bootstrap/Notes loads").unwrap();
    assert!(lint < notice && out.ends_with('\n'), "{out}");
}

#[test]
fn a_schemas_file_that_cannot_be_used_skips_the_schema_checks_silently() {
    let v = TestVault::new(&[(".recognitor/schemas.toml", "[type.concept]\nrequired_categories = [\n")]);
    let content = "---\ntype: concept\n---\n# Caching\n\nShort.\n";
    assert_eq!(
        v.create("design/Caching", content),
        result_line("design/Caching", content)
    );
}

#[test]
fn a_bad_key_in_a_readable_schemas_file_is_skipped_and_the_rest_applies() {
    let v = TestVault::new(&[(
        ".recognitor/schemas.toml",
        "[type.concept]\nbogus = 1\nrequired_categories = [\"limit\"]\n",
    )]);
    let content = "---\ntype: concept\n---\n# Caching\n\nShort.\n";
    let out = v.create("design/Caching", content);
    assert!(
        out.ends_with("- schema \"concept\": no [limit] observation\n") && !out.contains("bogus"),
        "{out}"
    );
}

#[test]
fn archive_notes_get_structural_findings_only_and_an_append_shows_only_new_ones() {
    let v = TestVault::new(&[(".recognitor/schemas.toml", CONCEPT_SCHEMA)]);
    let content = format!("---\ntype: concept\n---\n# Session\n\n{}\n", long(70));
    assert_eq!(
        v.create("archive/Session", &content),
        format!(
            "{}lint: 1 warning; the note is saved\n- line 6: {}\n",
            result_line("archive/Session", &content),
            long_message(70)
        )
    );
    let out = v.edit(
        "archive/Session",
        &Op::Append {
            section: None,
            text: "More transcript.".into(),
        },
    );
    assert!(
        out.starts_with("archive/Session  checksum: ") && !out.contains("lint:"),
        "{out}"
    );
}

#[test]
fn check_6_a_write_reads_the_other_notes_in_the_index() {
    let v = TestVault::new(&[("design/Old.md", "---\nstatus: active\n---\n# Old\n\nOld.\n")]);
    let out = v.create("design/New", "# New\n\nNew.\n\n## Relations\n- supersedes [[Old]]\n");
    assert!(
        out.ends_with(
            "lint: 1 warning; the note is saved\n- line 6: supersedes [[Old]], but design/Old has status \"active\", \
             not \"superseded\"\n"
        ),
        "{out}"
    );
    // A note that says it is superseded, with no edge to confirm it.
    let out = v.create("design/Stale", "---\nstatus: superseded\n---\n# Stale\n\nStale.\n");
    assert!(
        out.ends_with(
            "lint: 1 warning; the note is saved\n- status is \"superseded\", but no note links to it with supersedes\n"
        ),
        "{out}"
    );
}

#[test]
fn an_entry_point_spelled_index_in_capitals_warns_on_create() {
    let v = TestVault::new(&[]);
    let out = v.create("design/Index", "# Index\n\nEntry.\n");
    assert!(
        out.ends_with("lint: 1 warning; the note is saved\n- entry point is spelled \"Index.md\"; spell it index.md\n"),
        "{out}"
    );
}

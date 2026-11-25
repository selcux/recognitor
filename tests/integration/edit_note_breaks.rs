//! `edit_note` line breaks and section headings (doc 07 §9, "Line breaks", §5.3): a line break typed in `find` matches
//! LF or CRLF in the note, new text is written with the note's own line breaks, and `replace_section` refuses text that
//! does not start with a heading line.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::edit_note::{Op, render};
use recognitor::{Error, Index};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

const LIMIT: usize = 4000;

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(rel: &str, bytes: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-edit-breaks-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
        fs::write(root.join(rel), bytes).unwrap();
        TestVault {
            root,
            db: base.join("index.db"),
        }
    }

    fn bytes(&self, rel: &str) -> String {
        String::from_utf8(fs::read(self.root.join(rel)).unwrap()).unwrap()
    }

    fn edit(&self, note: &str, op: Op) -> String {
        render(&self.index().edit_note(note, &op, LIMIT).unwrap())
    }

    fn index(&self) -> Index {
        Index::open(&self.root, Some(&self.db)).unwrap()
    }

    fn section_sum(&self, note: &str, section: &str) -> String {
        let read = self.index().read(note, Some(section), None, LIMIT).unwrap();
        let recognitor::read::Read::Whole(text) = read else {
            panic!("expected one whole section");
        };
        text.checksum.unwrap()
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

/// True when every `\n` of the text comes with its `\r`.
fn all_crlf(text: &str) -> bool {
    text.matches('\n').count() == text.matches("\r\n").count()
}

/// True when the text has no `\r\n`.
fn all_lf(text: &str) -> bool {
    !text.contains("\r\n")
}

const CRLF_NOTE: &str = "---\r\ntype: note\r\n---\r\n# Sleep\r\n\r\nSleep repairs the day.\r\nIt also sorts memories.\r\n\r\n## Plan\r\n\r\n- go to bed early\r\n- no screens\r\n\r\n## Observations\r\n- [fact] Adults need 7 hours.\r\n\r\n## Relations\r\n- part_of [[Health]]\r\n";

#[test]
fn a_find_typed_with_lf_matches_a_crlf_note_and_the_replacement_is_written_with_crlf() {
    let v = TestVault::new("a/Sleep.md", CRLF_NOTE);
    let out = v.edit(
        "a/Sleep",
        Op::FindReplace {
            find: "Sleep repairs the day.\nIt also sorts memories.".to_string(),
            text: "Sleep repairs the day.\nIt sorts memories.\nIt clears waste.".to_string(),
        },
    );
    let expected = CRLF_NOTE.replace(
        "Sleep repairs the day.\r\nIt also sorts memories.",
        "Sleep repairs the day.\r\nIt sorts memories.\r\nIt clears waste.",
    );
    assert_eq!(v.bytes("a/Sleep.md"), expected);
    assert!(all_crlf(&v.bytes("a/Sleep.md")));
    assert_eq!(
        out,
        format!(
            "a/Sleep  checksum: {}\n",
            recognitor::hash::sha256_prefix(expected.as_bytes(), 12)
        )
    );
}

#[test]
fn a_find_typed_with_crlf_matches_an_lf_note() {
    let v = TestVault::new("a/N.md", "# N\n\nline one\nline two\n");
    v.edit(
        "a/N",
        Op::FindReplace {
            find: "line one\r\nline two".to_string(),
            text: "merged\r\nlines".to_string(),
        },
    );
    // The note's own breaks, whatever the agent typed.
    assert_eq!(v.bytes("a/N.md"), "# N\n\nmerged\nlines\n");
}

#[test]
fn a_multi_line_find_with_an_empty_replacement_removes_the_lines_of_a_crlf_note() {
    let v = TestVault::new("a/Sleep.md", CRLF_NOTE);
    v.edit(
        "a/Sleep",
        Op::FindReplace {
            find: "- go to bed early\n- no screens".to_string(),
            text: String::new(),
        },
    );
    assert_eq!(
        v.bytes("a/Sleep.md"),
        CRLF_NOTE.replace("- go to bed early\r\n- no screens\r\n", "")
    );
}

#[test]
fn a_repeated_multi_line_find_is_refused_whatever_the_breaks() {
    let v = TestVault::new("a/N.md", "x\r\ny\nx\ny\n");
    let out = v.edit(
        "a/N",
        Op::FindReplace {
            find: "x\ny".to_string(),
            text: "z".to_string(),
        },
    );
    assert!(
        out.starts_with(
            "The text to replace occurs 2 times, so nothing was written; it must occur exactly once. Current version:\n"
        ),
        "{out}"
    );
}

#[test]
fn every_op_writes_new_text_with_a_crlf_notes_line_breaks() {
    let v = TestVault::new("a/Sleep.md", CRLF_NOTE);
    let sum = v.section_sum("a/Sleep", "Plan");
    v.edit(
        "a/Sleep",
        Op::ReplaceSection {
            section: "Plan".to_string(),
            expected_checksum: sum,
            text: "## Plan\n\n- sleep\n- wake\n".to_string(),
        },
    );
    v.edit(
        "a/Sleep",
        Op::Append {
            section: None,
            text: "First paragraph.\nSecond line.".to_string(),
        },
    );
    v.edit(
        "a/Sleep",
        Op::Append {
            section: Some("Plan".to_string()),
            text: "- read\n- stretch".to_string(),
        },
    );
    v.edit(
        "a/Sleep",
        Op::AddObservation {
            text: "[fact] Naps help.".to_string(),
        },
    );
    v.edit(
        "a/Sleep",
        Op::AddRelation {
            text: "relates_to [[Rest]]".to_string(),
        },
    );
    let file = v.bytes("a/Sleep.md");
    assert!(all_crlf(&file), "{file:?}");
    assert!(
        file.contains("## Plan\r\n\r\n- sleep\r\n- wake\r\n\r\nFirst paragraph.\r\nSecond line.\r\n\r\n- read\r\n- stretch\r\n\r\n## Observations"),
        "{file:?}"
    );
    assert!(file.contains("- [fact] Naps help.\r\n"), "{file:?}");
    assert!(file.ends_with("- relates_to [[Rest]]\r\n"), "{file:?}");
}

#[test]
fn every_op_writes_new_text_with_an_lf_notes_line_breaks_even_when_the_agent_typed_crlf() {
    let v = TestVault::new(
        "a/N.md",
        "# N\n\nIntro.\n\n## A\n\nold\n\n## Observations\n- [fact] x\n",
    );
    let sum = v.section_sum("a/N", "A");
    v.edit(
        "a/N",
        Op::ReplaceSection {
            section: "A".to_string(),
            expected_checksum: sum,
            text: "## A\r\n\r\nnew\r\nlines\r\n".to_string(),
        },
    );
    v.edit(
        "a/N",
        Op::Append {
            section: None,
            text: "para one\r\npara two".to_string(),
        },
    );
    let file = v.bytes("a/N.md");
    assert!(all_lf(&file) && !file.contains('\r'), "{file:?}");
    assert!(file.contains("para one\npara two\n"), "{file:?}");
    assert!(file.contains("## A\n\nnew\nlines\n"), "{file:?}");
}

#[test]
fn replace_section_refuses_text_that_does_not_start_with_a_heading_line() {
    let note = "# N\n\n## Plan\n\n- sleep\n\n## Other\n\nx\n";
    let v = TestVault::new("a/N.md", note);
    let sum = v.section_sum("a/N", "Plan");
    for text in ["- just the body", "x\n## Plan\n- sleep", "```\n## Plan\n```"] {
        let out = v.edit(
            "a/N",
            Op::ReplaceSection {
                section: "Plan".to_string(),
                expected_checksum: sum.clone(),
                text: text.to_string(),
            },
        );
        assert_eq!(
            out,
            "The new section must start with its heading line, so nothing was written. Current heading: ## Plan"
        );
    }
    assert_eq!(v.bytes("a/N.md"), note);
    // A heading of another level or name is the agent's choice.
    v.edit(
        "a/N",
        Op::ReplaceSection {
            section: "Plan".to_string(),
            expected_checksum: sum,
            text: "### Renamed\n\n- sleep\n".to_string(),
        },
    );
    assert!(v.bytes("a/N.md").contains("### Renamed\n\n- sleep\n\n## Other"));
}

#[test]
fn a_setext_section_is_named_by_its_heading_line_as_on_disk() {
    let v = TestVault::new("a/N.md", "# N\n\nPlan\n----\n\n- sleep\n");
    let sum = v.section_sum("a/N", "Plan");
    let out = v.edit(
        "a/N",
        Op::ReplaceSection {
            section: "Plan".to_string(),
            expected_checksum: sum,
            text: "no heading".to_string(),
        },
    );
    assert!(out.ends_with("Current heading: Plan"), "{out}");
}

#[test]
fn whitespace_text_replaces_in_find_replace_and_deletes_a_section() {
    let v = TestVault::new("a/N.md", "# N\n\nfoo  bar and drop that\n\n## A\n\nold\n\n## B\n\nb\n");
    // Two spaces become one: whitespace is a normal replacement, only "" deletes.
    v.edit(
        "a/N",
        Op::FindReplace {
            find: "foo  bar".to_string(),
            text: "foo bar".to_string(),
        },
    );
    // A single space as `text` replaces the found text with that space.
    v.edit(
        "a/N",
        Op::FindReplace {
            find: " drop that".to_string(),
            text: " ".to_string(),
        },
    );
    assert!(
        v.bytes("a/N.md").starts_with("# N\n\nfoo bar and \n\n## A"),
        "{:?}",
        v.bytes("a/N.md")
    );
    // An empty `text` deletes it.
    v.edit(
        "a/N",
        Op::FindReplace {
            find: " and ".to_string(),
            text: String::new(),
        },
    );
    assert!(v.bytes("a/N.md").starts_with("# N\n\nfoo bar\n\n## A"));
    // A section goes on empty or whitespace-only text.
    let sum = v.section_sum("a/N", "A");
    v.edit(
        "a/N",
        Op::ReplaceSection {
            section: "A".to_string(),
            expected_checksum: sum,
            text: " \n\n".to_string(),
        },
    );
    assert_eq!(v.bytes("a/N.md"), "# N\n\nfoo bar\n\n## B\n\nb\n");
}

#[test]
fn a_stale_checksum_with_a_good_heading_still_returns_the_current_section() {
    let v = TestVault::new("a/N.md", "# N\n\n## A\n\nx\n");
    let err = v.index().edit_note(
        "a/N",
        &Op::ReplaceSection {
            section: "Nope".to_string(),
            expected_checksum: "000000000000".to_string(),
            text: "## A\n".to_string(),
        },
        LIMIT,
    );
    assert!(matches!(err, Err(Error::NoSection { .. })));
}

//! `edit_note` tests (doc 07 §9, §5, §1.1, §7.3). Vaults live in temp folders that delete themselves. The tool's
//! argument checks are tested over MCP, in `tests/mcp.rs`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::edit_note::{EditNote, Op, render};
use recognitor::hash::sha256_prefix;
use recognitor::read::{Read, render as render_read};
use recognitor::{Error, Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

const LIMIT: usize = 4000;

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-edit-note-{}-{n}", std::process::id()));
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

    /// A file written by a human, outside Recognitor.
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

    /// The text of an `edit_note` call: the result, or the refusal.
    fn edit(&self, note: &str, op: Op) -> String {
        render(&self.index().edit_note(note, &op, LIMIT).unwrap())
    }

    /// The checksum of a section as `read_note` shows it.
    fn section_sum(&self, note: &str, section: &str) -> String {
        let read = self.index().read(note, Some(section), None, LIMIT).unwrap();
        let Read::Whole(text) = read else {
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

fn sum(text: &str) -> String {
    recognitor::hash::sha256_prefix(text.as_bytes(), 12)
}

fn append(text: &str) -> Op {
    Op::Append {
        section: None,
        text: text.to_string(),
    }
}

fn append_to(section: &str, text: &str) -> Op {
    Op::Append {
        section: Some(section.to_string()),
        text: text.to_string(),
    }
}

fn observation(text: &str) -> Op {
    Op::AddObservation { text: text.to_string() }
}

fn relation(text: &str) -> Op {
    Op::AddRelation { text: text.to_string() }
}

fn find_replace(find: &str, text: &str) -> Op {
    Op::FindReplace {
        find: find.to_string(),
        text: text.to_string(),
    }
}

fn replace_section(section: &str, checksum: &str, text: &str) -> Op {
    Op::ReplaceSection {
        section: section.to_string(),
        expected_checksum: checksum.to_string(),
        text: text.to_string(),
    }
}

const NOTE: &str = "---\ntype: note\ntags: [sleep]\n---\n# Sleep\n\nSleep repairs the day.\n\n## Plan\n\n- go to bed early\n- no screens\n\n## Observations\n- [fact] Adults need 7 hours.\n\n## Relations\n- part_of [[Health]]\n";

#[test]
fn append_without_a_section_goes_before_observations_and_the_index_follows() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let out = v.edit("baseline/Sleep", append("A zebra paragraph."));
    let expected = NOTE.replace(
        "- no screens\n\n## Observations",
        "- no screens\n\nA zebra paragraph.\n\n## Observations",
    );
    // The new text lands in the Plan section, whose end is just before `## Observations`; it joins no list.
    assert_eq!(v.file("baseline/Sleep.md"), expected);
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum(&expected)));
    // Read your own writes: a search finds it, and a read shows the same checksum.
    let mut index = v.index();
    let page = index.search("zebra", None, 1).unwrap();
    assert!(recognitor::search::render(&page).starts_with("1. baseline/Sleep"));
    let read = index.read("baseline/Sleep", None, None, LIMIT).unwrap();
    assert!(render_read(&read).starts_with(&format!("baseline/Sleep  checksum: {}\n", sum(&expected))));
}

#[test]
fn append_to_a_named_section_goes_at_its_end() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    v.edit("baseline/Sleep", append_to("plan", "- read a book"));
    assert_eq!(
        v.file("baseline/Sleep.md"),
        NOTE.replace("- no screens\n", "- no screens\n- read a book\n")
    );
    v.edit("baseline/Sleep", append_to("Plan", "Remember the alarm."));
    assert!(
        v.file("baseline/Sleep.md")
            .contains("- read a book\n\nRemember the alarm.\n\n## Observations")
    );
}

#[test]
fn append_with_a_section_that_matches_several_is_a_normal_result_and_none_is_an_error() {
    let v = TestVault::new(&[(
        "a/N.md",
        "# N\n\n## Storage\n\n### Notes\n\nx\n\n## Search\n\n### Notes\n\ny\n",
    )]);
    let mut index = v.index();
    let several = index.edit_note("a/N", &append_to("Notes", "z"), LIMIT).unwrap();
    assert_eq!(
        render(&several),
        "\"Notes\" matches 2 sections in a/N. Call again with one:\n  Storage > Notes\n  Search > Notes\n"
    );
    assert!(matches!(several, EditNote::Several(_)));
    let err = index.edit_note("a/N", &append_to("Nope", "z"), LIMIT).unwrap_err();
    assert_eq!(
        err.to_string(),
        "No section \"Nope\" in a/N. Sections: \"Storage\", \"Search\"."
    );
    // One call with a path works, and nothing else changed.
    index
        .edit_note("a/N", &append_to("Search > Notes", "z"), LIMIT)
        .unwrap();
    assert_eq!(
        v.file("a/N.md"),
        "# N\n\n## Storage\n\n### Notes\n\nx\n\n## Search\n\n### Notes\n\ny\n\nz\n"
    );
}

#[test]
fn an_observation_is_added_and_indexed_and_the_section_is_created_when_missing() {
    let v = TestVault::new(&[
        ("baseline/Sleep.md", NOTE),
        ("baseline/Bare.md", "# Bare\n\nJust text.\n"),
    ]);
    let out = v.edit(
        "baseline/Sleep",
        observation("[decision] @effective:2026-06-10 Nap at noon. #habit"),
    );
    let expected = NOTE.replace(
        "- [fact] Adults need 7 hours.\n",
        "- [fact] Adults need 7 hours.\n- [decision] @effective:2026-06-10 Nap at noon. #habit\n",
    );
    assert_eq!(v.file("baseline/Sleep.md"), expected);
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum(&expected)));
    let mut index = v.index();
    let page = index.search("Nap noon", None, 1).unwrap();
    let text = recognitor::search::render(&page);
    assert!(
        text.contains("- [decision] @effective:2026-06-10 Nap at noon. #habit"),
        "{text}"
    );
    // A note with no section gets one at the end.
    v.edit("baseline/Bare", observation("[fact] It is bare."));
    assert_eq!(
        v.file("baseline/Bare.md"),
        "# Bare\n\nJust text.\n\n## Observations\n- [fact] It is bare.\n"
    );
}

#[test]
fn a_relation_is_added_and_becomes_an_edge() {
    let v = TestVault::new(&[
        ("baseline/Sleep.md", NOTE),
        ("baseline/Health.md", "# Health\n\nBody.\n"),
        ("baseline/Rest.md", "# Rest\n\nBody.\n"),
    ]);
    v.edit("baseline/Sleep", relation("relates_to [[Rest]] (naps)"));
    assert_eq!(
        v.file("baseline/Sleep.md"),
        NOTE.replace(
            "- part_of [[Health]]\n",
            "- part_of [[Health]]\n- relates_to [[Rest]] (naps)\n"
        )
    );
    let mut index = v.index();
    let context = recognitor::context::render(&index.context("baseline/Sleep", 1).unwrap());
    assert!(context.contains("- relates_to: baseline/Rest"), "{context}");
}

#[test]
fn a_line_that_is_already_there_writes_nothing_and_says_so() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let mut index = v.index();
    let before = fs::metadata(v.root.join("baseline/Sleep.md"))
        .unwrap()
        .modified()
        .unwrap();
    let out = render(
        &index
            .edit_note("baseline/Sleep", &observation("[fact] Adults need 7 hours."), LIMIT)
            .unwrap(),
    );
    assert_eq!(
        out,
        format!(
            "Already there, nothing was written.\nbaseline/Sleep  checksum: {}\n",
            sum(NOTE)
        )
    );
    let out = render(
        &index
            .edit_note("baseline/Sleep", &relation("part_of [[Health]]"), LIMIT)
            .unwrap(),
    );
    assert!(out.starts_with("Already there, nothing was written.\n"), "{out}");
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
    assert_eq!(
        fs::metadata(v.root.join("baseline/Sleep.md"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn a_line_that_would_not_be_indexed_is_refused_and_nothing_changes() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let obs = v.edit("baseline/Sleep", observation("Adults need 7 hours."));
    assert!(
        obs.starts_with("That is not an observation line, so nothing was written."),
        "{obs}"
    );
    let rel = v.edit("baseline/Sleep", relation("see also [[Health]]"));
    assert!(
        rel.starts_with("That is not a typed relation line, so nothing was written."),
        "{rel}"
    );
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
}

#[test]
fn find_replace_replaces_exactly_one_occurrence_frontmatter_included() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let out = v.edit("baseline/Sleep", find_replace("tags: [sleep]", "tags: [sleep, rest]"));
    let expected = NOTE.replace("tags: [sleep]", "tags: [sleep, rest]");
    assert_eq!(v.file("baseline/Sleep.md"), expected);
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum(&expected)));
    // An empty replacement of a whole line removes the line, a part of a line only the part.
    v.edit("baseline/Sleep", find_replace("- no screens", ""));
    assert!(!v.file("baseline/Sleep.md").contains("no screens"));
    assert!(
        v.file("baseline/Sleep.md")
            .contains("- go to bed early\n\n## Observations")
    );
    v.edit("baseline/Sleep", find_replace(" early", ""));
    assert!(v.file("baseline/Sleep.md").contains("- go to bed\n"));
}

#[test]
fn find_replace_that_misses_or_repeats_is_refused_with_the_whole_note() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let missing = v.edit("baseline/Sleep", find_replace("not in the note", "x"));
    assert_eq!(
        missing,
        format!(
            "The text to replace was not found, so nothing was written. Current version:\nbaseline/Sleep  checksum: {}\n{NOTE}",
            sum(NOTE)
        )
    );
    let repeated = v.edit("baseline/Sleep", find_replace("- ", "x"));
    // "- " starts the two Plan items, the observation and the relation.
    assert!(
        repeated.starts_with(
            "The text to replace occurs 4 times, so nothing was written; it must occur exactly once. Current version:\n"
        ),
        "{repeated}"
    );
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
}

#[test]
fn a_refusal_cuts_the_note_at_the_read_limit() {
    let mut long = String::from("# Long\n\nSummary.\n");
    for n in 1..=40 {
        long.push_str(&format!("\n## Part {n}\n{}\n", "word ".repeat(40)));
    }
    let v = TestVault::new(&[("a/Long.md", &long)]);
    let refused = v
        .index()
        .edit_note("a/Long", &find_replace("absent", "x"), 300)
        .unwrap();
    let text = render(&refused);
    // The cut is the read's, and its header line carries the whole-note checksum (doc 07 §5.3, §7.2).
    assert!(
        text.contains(&format!(
            "\na/Long  checksum: {}\n# Long\n",
            sha256_prefix(long.as_bytes(), 12)
        )),
        "{text}"
    );
    assert!(text.contains("[cut at 300 tokens."), "{text}");
    assert_eq!(v.file("a/Long.md"), long);
}

#[test]
fn replace_section_with_the_sections_checksum_replaces_it_and_keeps_the_rest_byte_for_byte() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let checksum = v.section_sum("baseline/Sleep", "Plan");
    let out = v.edit(
        "baseline/Sleep",
        replace_section("plan", &checksum, "## Plan\n\n- sleep\n- wake"),
    );
    let expected = NOTE.replace(
        "## Plan\n\n- go to bed early\n- no screens\n",
        "## Plan\n\n- sleep\n- wake\n",
    );
    assert_eq!(v.file("baseline/Sleep.md"), expected);
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum(&expected)));
    // The new section's checksum is what `read_note` shows now.
    assert_ne!(v.section_sum("baseline/Sleep", "Plan"), checksum);
}

#[test]
fn replace_section_with_empty_text_removes_the_section_heading_included() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let checksum = v.section_sum("baseline/Sleep", "Plan");
    v.edit("baseline/Sleep", replace_section("Plan", &checksum, ""));
    assert_eq!(
        v.file("baseline/Sleep.md"),
        NOTE.replace("## Plan\n\n- go to bed early\n- no screens\n\n", "")
    );
}

#[test]
fn a_stale_section_checksum_is_refused_with_the_section_as_it_is_now() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let stale = v.section_sum("baseline/Sleep", "Plan");
    // A human edits the section after the agent read it.
    let changed = NOTE.replace("- no screens", "- no screens\n- no coffee");
    v.put("baseline/Sleep.md", &changed);
    let out = v.edit("baseline/Sleep", replace_section("Plan", &stale, "## Plan\n\nmine"));
    let now = v.section_sum("baseline/Sleep", "Plan");
    assert_eq!(
        out,
        format!(
            "The section changed since that checksum, so nothing was written. Current version:\n\
             baseline/Sleep > Plan  checksum: {now}\n## Plan\n\n- go to bed early\n- no screens\n- no coffee\n\n"
        )
    );
    assert_eq!(v.file("baseline/Sleep.md"), changed);
    // The new checksum works.
    v.edit("baseline/Sleep", replace_section("Plan", &now, "## Plan\n\nmine"));
    assert!(
        v.file("baseline/Sleep.md")
            .contains("## Plan\n\nmine\n\n## Observations")
    );
}

#[test]
fn an_edit_outside_a_section_leaves_its_checksum_valid() {
    // Doc 07 §5.2: two agents editing different parts both succeed, with no re-read.
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let plan = v.section_sum("baseline/Sleep", "Plan");
    v.edit("baseline/Sleep", observation("[fact] Another fact."));
    v.edit(
        "baseline/Sleep",
        find_replace("Sleep repairs the day.", "Sleep repairs the body."),
    );
    let out = v.edit("baseline/Sleep", replace_section("Plan", &plan, "## Plan\n\nreplaced"));
    assert!(out.starts_with("baseline/Sleep  checksum: "), "{out}");
    let file = v.file("baseline/Sleep.md");
    assert!(file.contains("## Plan\n\nreplaced\n\n## Observations"), "{file}");
    assert!(file.contains("Another fact.") && file.contains("repairs the body."));
}

#[test]
fn replace_section_with_several_matches_or_none() {
    let v = TestVault::new(&[(
        "a/N.md",
        "# N\n\n## Storage\n\n### Notes\n\nx\n\n## Search\n\n### Notes\n\ny\n",
    )]);
    let several = v.edit("a/N", replace_section("Notes", "000000000000", "x"));
    assert_eq!(
        several,
        "\"Notes\" matches 2 sections in a/N. Call again with one:\n  Storage > Notes\n  Search > Notes\n"
    );
    let err = v
        .index()
        .edit_note("a/N", &replace_section("Nope", "000000000000", "x"), LIMIT)
        .unwrap_err();
    assert!(matches!(err, Error::NoSection { .. }));
    // A heading inside code is text, so it names no section.
    let v = TestVault::new(&[("a/C.md", "# C\n\n```\n## Fake\n```\n\n## Real\n\nx\n")]);
    let err = v
        .index()
        .edit_note("a/C", &replace_section("Fake", "000000000000", "x"), LIMIT)
        .unwrap_err();
    assert!(matches!(err, Error::NoSection { .. }));
}

#[test]
fn the_edit_reads_the_disk_not_the_index() {
    // Doc 07 §5.1 step 2: a human edited the file inside the sweep window, and the edit sees it.
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    v.put(
        "baseline/Sleep.md",
        &format!("{NOTE}\n## Human\n\nA human wrote this a moment ago.\n"),
    );
    index
        .edit_note("baseline/Sleep", &find_replace("a moment ago", "just now"), LIMIT)
        .unwrap();
    assert!(v.file("baseline/Sleep.md").ends_with("A human wrote this just now.\n"));
    index
        .edit_note("baseline/Sleep", &observation("[fact] After the human."), LIMIT)
        .unwrap();
    assert!(v.file("baseline/Sleep.md").contains("After the human."));
    assert!(v.file("baseline/Sleep.md").contains("## Human"));
}

#[test]
fn a_note_is_named_as_in_every_tool_and_a_missing_one_is_not_found() {
    let v = TestVault::new(&[("a/Alpha.md", "# Alpha\n\nx\n"), ("b/Alpha.md", "# Alpha\n\ny\n")]);
    let mut index = v.index();
    let several = index.edit_note("Alpha", &append("z"), LIMIT).unwrap();
    assert_eq!(
        render(&several),
        "\"Alpha\" matches 2 notes. Call again with one id:\n  a/Alpha\n  b/Alpha\n"
    );
    assert!(matches!(several, EditNote::Several(_)));
    let err = index.edit_note("nope", &append("z"), LIMIT).unwrap_err();
    assert_eq!(err.to_string(), "\"nope\" not found");
    // By name inside its namespace, by prefix, by path with `.md`.
    index.edit_note("a::Alpha", &append("z"), LIMIT).unwrap();
    index.edit_note("b/Alpha.md", &append("w"), LIMIT).unwrap();
    assert_eq!(v.file("a/Alpha.md"), "# Alpha\n\nx\n\nz\n");
    assert_eq!(v.file("b/Alpha.md"), "# Alpha\n\ny\n\nw\n");
}

#[test]
fn the_archive_takes_append_and_refuses_every_other_op() {
    let record = "# Session\n\nRaw transcript.\n";
    let v = TestVault::new(&[("archive/rec/Session.md", record)]);
    let out = v.edit("archive/rec/Session", append("A later line."));
    let expected = "# Session\n\nRaw transcript.\n\nA later line.\n";
    assert_eq!(out, format!("archive/rec/Session  checksum: {}\n", sum(expected)));
    assert_eq!(v.file("archive/rec/Session.md"), expected);
}

#[test]
fn every_other_op_on_an_archive_note_is_refused_and_returns_nothing() {
    let record = "# Session\n\n## Part\n\nRaw transcript.\n";
    let v = TestVault::new(&[("archive/rec/Session.md", record)]);
    let refusal = "Archive notes are append only: use append to add to one.";
    let sec = v.section_sum("archive/rec/Session", "Part");
    for op in [
        observation("[fact] x"),
        relation("part_of [[X]]"),
        find_replace("Raw", "Cooked"),
        replace_section("Part", &sec, "## Part\n"),
    ] {
        assert_eq!(v.edit("archive/rec/Session", op), refusal);
    }
    assert_eq!(v.file("archive/rec/Session.md"), record);
}

#[test]
fn a_bootstrap_note_carries_the_notice_on_every_op_that_writes() {
    let v = TestVault::new(&[(
        "bootstrap/Profile.md",
        "# Profile\n\nShort answers.\n\n## Rules\n\n- be brief\n",
    )]);
    let notice = "notice: bootstrap/Profile loads as instructions at session start\n";
    for op in [
        append("More."),
        observation("[fact] A fact."),
        relation("relates_to [[Other]]"),
        find_replace("Short answers.", "Brief answers."),
    ] {
        let out = v.edit("bootstrap/Profile", op);
        assert!(
            out.starts_with("bootstrap/Profile  checksum: ") && out.ends_with(notice),
            "{out}"
        );
    }
    let sum_rules = v.section_sum("bootstrap/Profile", "Rules");
    let out = v.edit(
        "bootstrap/Profile",
        replace_section("Rules", &sum_rules, "## Rules\n\n- be kind"),
    );
    assert!(out.ends_with(notice), "{out}");
    // A knowledge note carries none.
    let k = TestVault::new(&[("a/N.md", "# N\n")]);
    assert!(!k.edit("a/N", append("x")).contains("notice"));
}

#[test]
fn a_refusal_changes_neither_the_disk_nor_the_index() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    for _ in 0..2 {
        index
            .edit_note("baseline/Sleep", &find_replace("absent", "Not this"), LIMIT)
            .unwrap();
        index
            .edit_note(
                "baseline/Sleep",
                &replace_section("Plan", "000000000000", "Not this"),
                LIMIT,
            )
            .unwrap();
        index
            .edit_note("baseline/Sleep", &observation("Not this"), LIMIT)
            .unwrap();
    }
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
    let page = index.search("Not this", None, 1).unwrap();
    assert!(recognitor::search::render(&page).starts_with("No notes match"));
}

#[test]
fn the_next_sync_after_edits_reads_no_files_and_finds_the_index_in_step() {
    let v = TestVault::new(&[
        ("baseline/Sleep.md", NOTE),
        ("baseline/Health.md", "# Health\n\nBody.\n"),
    ]);
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    index.edit_note("baseline/Sleep", &append("More."), LIMIT).unwrap();
    index
        .edit_note("baseline/Sleep", &observation("[fact] One."), LIMIT)
        .unwrap();
    index
        .edit_note("baseline/Sleep", &relation("relates_to [[Health]]"), LIMIT)
        .unwrap();
    index
        .edit_note("baseline/Sleep", &find_replace("More.", "More!"), LIMIT)
        .unwrap();
    let plan = v.section_sum("baseline/Sleep", "Plan");
    index
        .edit_note(
            "baseline/Sleep",
            &replace_section("Plan", &plan, "## Plan\n\nnew"),
            LIMIT,
        )
        .unwrap();
    // The files were saved after the last sweep started, so the first sweep re-hashes them once (the racy rule of
    // doc 02 §4); it finds nothing new, changed or gone, because the index already followed the files.
    let first = index.sync(SweepOptions::default()).unwrap();
    assert!(first.added.is_empty() && first.changed.is_empty() && first.deleted.is_empty());
    assert!(first.retitled.is_empty() && first.warnings.is_empty());
    assert_eq!(first.files_read, first.rehashed, "{first:?}");
    let second = index.sync(SweepOptions::default()).unwrap();
    assert_eq!(second.files_read, 0, "{second:?}");
}

#[test]
fn bytes_outside_the_edit_are_untouched_including_line_endings_and_a_byte_order_mark() {
    let text = "\u{feff}---\r\ntype: note\r\n---\r\n# T\r\n\r\nSum.\r\n\r\n## Observations\r\n- [fact] a\r\n";
    let v = TestVault::new(&[("a/T.md", text)]);
    let bytes_before = fs::read(v.root.join("a/T.md")).unwrap();
    v.edit("a/T", observation("[fact] b"));
    let after = fs::read(v.root.join("a/T.md")).unwrap();
    assert!(after.starts_with(&bytes_before));
    assert_eq!(String::from_utf8(after).unwrap(), format!("{text}- [fact] b\r\n"));
}

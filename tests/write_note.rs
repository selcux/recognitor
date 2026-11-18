//! `write_note` tests (doc 07 §11, §5, §6, §1.1). Vaults live in temp folders that delete themselves. Case twins can
//! exist only on a case-sensitive file system, so the tests that need them probe for one (on Windows they switch the
//! test folder to NTFS's per-folder case sensitivity) and stop early where there is none; the matching rules are also
//! unit-tested in the library on listings.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::bootstrap::estimate_tokens;
use recognitor::read::render as render_read;
use recognitor::write_note::{WriteNote, render};
use recognitor::{Error, Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

const LIMIT: usize = 4000;

/// A note over the default read limit (about 4,400 tokens, estimated), made of sections.
fn long_note() -> String {
    let mut long = String::from("# Long\n\nSummary.\n");
    for n in 1..=40 {
        long.push_str(&format!("\n## Part {n}\n{}\n", "word ".repeat(100)));
    }
    long
}

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-write-note-{}-{n}", std::process::id()));
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

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    fn index(&self) -> Index {
        Index::open(&self.root, Some(&self.db)).unwrap()
    }

    /// The text of a `write_note` call: the result, or the refusal.
    fn write(&self, path: &str, content: &str, checksum: Option<&str>) -> String {
        render(&self.index().write_note(path, content, checksum, LIMIT).unwrap())
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

/// Makes `dir`, which must be empty, tell `Probe.md` from `probe.md`, and says whether it does. Linux does anyway; on
/// Windows the folder gets NTFS's per-folder case sensitivity (`fsutil`), which new subfolders inherit. Where neither
/// works (macOS, a Windows without the feature) the caller stops early: twins cannot exist there.
fn make_case_sensitive(dir: &std::path::Path) -> bool {
    fs::create_dir_all(dir).unwrap();
    if cfg!(windows) {
        let _ = std::process::Command::new("fsutil.exe")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(dir)
            .arg("enable")
            .output();
    }
    fs::write(dir.join("Probe.md"), "x").unwrap();
    fs::write(dir.join("probe.md"), "y").unwrap();
    let both = fs::read_dir(dir).unwrap().count() == 2;
    let _ = fs::remove_file(dir.join("Probe.md"));
    let _ = fs::remove_file(dir.join("probe.md"));
    both
}

fn top_folders(v: &TestVault) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(&v.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn sum(text: &str) -> String {
    recognitor::hash::sha256_prefix(text.as_bytes(), 12)
}

const NOTE: &str = "---\ntype: note\n---\n# Sleep\n\nSleep is where the day is repaired.\n";

#[test]
fn a_create_writes_the_exact_bytes_and_returns_the_id_and_the_short_checksum() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    let out = v.write("baseline/Sleep.md", NOTE, None);
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum(NOTE)));
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
    // The checksum is 12 lowercase hex characters.
    let checksum = out.trim_end().rsplit(' ').next().unwrap();
    assert_eq!(checksum.len(), 12);
    assert!(checksum.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
}

#[test]
fn md_is_optional_and_no_other_file_is_left_behind() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    assert_eq!(
        v.write("baseline/Plain", "# Plain\n", None),
        format!("baseline/Plain  checksum: {}\n", sum("# Plain\n"))
    );
    assert!(v.exists("baseline/Plain.md"));
    let mut names: Vec<String> = fs::read_dir(v.root.join("baseline"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["Plain.md", "index.md"]);
}

#[test]
fn a_new_path_reuses_the_spelling_of_existing_folders() {
    // Doc 07 §6: `Baseline/x.md` lands in `baseline/`, and so does every folder of the path.
    let v = TestVault::new(&[("baseline/Nutrition/index.md", "# Nutrition\n")]);
    let out = v.write("Baseline/NUTRITION/x.md", "# X\n", None);
    assert_eq!(out, format!("baseline/Nutrition/x  checksum: {}\n", sum("# X\n")));
    assert_eq!(v.file("baseline/Nutrition/x.md"), "# X\n");
    let top: Vec<String> = fs::read_dir(&v.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(top, ["baseline"]);
}

#[test]
fn folders_are_created_as_needed_and_a_new_part_keeps_its_typed_spelling() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    let out = v.write("Baseline/Sleep/Deep Notes/Night 1", "# Night 1\n", None);
    assert_eq!(
        out,
        format!("baseline/Sleep/Deep Notes/Night 1  checksum: {}\n", sum("# Night 1\n"))
    );
    assert!(v.exists("baseline/Sleep/Deep Notes/Night 1.md"));
    // The first note of a new top-level folder: a namespace without an index.md is fine (doc 07 §3).
    let out = v.write("Shelf/Item", "# Item\n", None);
    assert_eq!(out, format!("Shelf/Item  checksum: {}\n", sum("# Item\n")));
    assert!(v.exists("Shelf/Item.md"));
}

#[test]
fn a_create_on_an_existing_note_is_refused_and_returns_the_note_with_its_checksum() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let out = v.write("baseline/Sleep", "# Mine\n\nOverwrite attempt.\n", None);
    assert_eq!(
        out,
        format!(
            "The note already exists, so nothing was written. To replace it, call again with its checksum as \
             expected_checksum. Current version:\nbaseline/Sleep  checksum: {}\n{NOTE}",
            sum(NOTE)
        )
    );
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
}

#[test]
fn a_create_onto_another_case_of_an_existing_note_is_refused_with_that_note() {
    // The key matches, so it exists (doc 07 §6, table row 1), shown with the id as it is on disk.
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let out = v.write("Baseline/SLEEP.md", "# Other\n", None);
    assert!(
        out.contains(&format!("baseline/Sleep  checksum: {}", sum(NOTE))),
        "{out}"
    );
    assert_eq!(top_folders(&v), ["baseline"]);
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
}

#[test]
fn a_refusal_returns_the_note_cut_at_the_read_limit() {
    let mut long = String::from("# Long\n\nSummary.\n");
    for n in 1..=40 {
        long.push_str(&format!("\n## Part {n}\n{}\n", "word ".repeat(40)));
    }
    let v = TestVault::new(&[("a/Long.md", &long)]);
    let mut index = v.index();
    let refused = index.write_note("a/Long", "# New\n", None, 300).unwrap();
    let WriteNote::Refused(text) = refused else {
        panic!("a create on an existing note must be refused");
    };
    // A note over the read limit gets the variant text (doc 07 §5.3): the checksum the plain text asks for could never
    // be used. The note follows as the read cuts it, with the whole-note checksum in its header line and a closing line
    // (§7.2).
    assert!(
        text.starts_with(&format!(
            "The note already exists and is too long to replace whole, so nothing was written. Change it with \
             edit_note. Current version:\na/Long  checksum: {}\n# Long\n",
            sum(&long)
        )),
        "{text}"
    );
    assert!(text.contains("[cut at 300 tokens."), "{text}");
    assert!(
        estimate_tokens(text.chars().count()) < 400,
        "{}",
        estimate_tokens(text.chars().count())
    );
    assert_eq!(v.file("a/Long.md"), long);
}

#[test]
fn a_file_a_human_saved_that_the_index_does_not_know_is_still_found_on_disk() {
    // Doc 07 §5.1 step 2: the check reads the disk, never the index.
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    v.put("baseline/Sleep.md", "# Sleep\n\nA human wrote this a moment ago.\n");
    let out = render(&index.write_note("baseline/sleep", "# Agent\n", None, LIMIT).unwrap());
    assert!(out.contains("baseline/Sleep  checksum:"), "{out}");
    assert!(out.contains("A human wrote this a moment ago."), "{out}");
    assert_eq!(
        v.file("baseline/Sleep.md"),
        "# Sleep\n\nA human wrote this a moment ago.\n"
    );
}

#[test]
fn case_twin_files_on_disk_refuse_a_create_and_return_every_path_without_content() {
    let v = TestVault::new(&[]);
    if !make_case_sensitive(&v.root.join("baseline")) {
        return;
    }
    v.put("baseline/Nutrition.md", "# Upper\n\nUPPER BODY\n");
    v.put("baseline/nutrition.md", "# Lower\n\nLOWER BODY\n");
    let out = v.write("baseline/nutrition", "# Agent\n", None);
    assert_eq!(
        out,
        "Names that differ only in case cannot exist together, so nothing was written. On disk:\n  \
         baseline/Nutrition.md\n  baseline/nutrition.md\n"
    );
    assert!(!out.contains("BODY"));
    // An overwrite, with any checksum, meets the same twins.
    let out = v.write("baseline/nutrition", "# Agent\n", Some(&sum("# Lower\n\nLOWER BODY\n")));
    assert!(out.starts_with("Names that differ only in case"), "{out}");
    assert_eq!(v.file("baseline/nutrition.md"), "# Lower\n\nLOWER BODY\n");
}

#[test]
fn twin_folders_on_disk_refuse_a_create() {
    let v = TestVault::new(&[]);
    if !make_case_sensitive(&v.root) {
        return;
    }
    v.put("Baseline/a.md", "# A\n");
    v.put("baseline/b.md", "# B\n");
    let out = v.write("BASELINE/new", "# New\n", None);
    assert_eq!(
        out,
        "Names that differ only in case cannot exist together, so nothing was written. On disk:\n  Baseline/\n  baseline/\n"
    );
    assert!(!v.exists("BASELINE"));
}

#[test]
fn a_path_without_a_top_level_folder_or_with_non_ascii_or_outside_the_vault_is_refused() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    for (path, reason) in [
        ("loose.md", "no top-level folder"),
        ("loose", "no top-level folder"),
        ("caf\u{e9}/x.md", "non-ASCII"),
        ("baseline/caf\u{e9}.md", "non-ASCII"),
        ("../escape.md", "not a note path"),
        ("baseline/../../escape.md", "not a note path"),
        (".recognitor/trash/x.md", "not a note path"),
        ("baseline/a:b.md", "not a note path"),
    ] {
        let out = v.write(path, "# X\n", None);
        assert!(out.contains(reason), "{path}: {out}");
    }
    // Nothing was written, inside the vault or next to it.
    assert!(!v.exists("loose.md") && !v.exists(".recognitor"));
    assert!(!v.root.parent().unwrap().join("escape.md").exists());
    let mut top: Vec<String> = fs::read_dir(&v.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, ["baseline"]);
}

#[test]
fn an_overwrite_with_the_current_checksum_replaces_the_note_and_the_index_follows() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let new = "---\ntype: note\n---\n# Sleep\n\nA brand new zebra paragraph.\n";
    let out = v.write("baseline/Sleep", new, Some(&sum(NOTE)));
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum(new)));
    assert_eq!(v.file("baseline/Sleep.md"), new);
    // Read your own writes: a search in the same moment sees it, and the read shows the same checksum.
    let mut index = v.index();
    let page = index.search("zebra", None, 1).unwrap();
    assert!(recognitor::search::render(&page).starts_with("1. baseline/Sleep"));
    let read = index.read("baseline/Sleep", None, None, LIMIT).unwrap();
    assert!(render_read(&read).starts_with(&format!("baseline/Sleep  checksum: {}\n", sum(new))));
}

#[test]
fn an_overwrite_with_a_stale_checksum_is_refused_and_returns_the_current_note() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let moved_on = format!("{NOTE}\nA human added this line.\n");
    v.put("baseline/Sleep.md", &moved_on);
    let out = v.write("baseline/Sleep", "# Agent\n", Some(&sum(NOTE)));
    assert_eq!(
        out,
        format!(
            "The note changed since that checksum, so nothing was written. Current version:\n\
             baseline/Sleep  checksum: {}\n{moved_on}",
            sum(&moved_on)
        )
    );
    assert_eq!(v.file("baseline/Sleep.md"), moved_on);
}

#[test]
fn an_overwrite_finds_the_note_by_key_and_keeps_its_spelling() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let out = v.write("Baseline/SLEEP.md", "# Sleep\n", Some(&sum(NOTE)));
    assert_eq!(out, format!("baseline/Sleep  checksum: {}\n", sum("# Sleep\n")));
    assert_eq!(top_folders(&v), ["baseline"]);
    assert_eq!(fs::read_dir(v.root.join("baseline")).unwrap().count(), 1);
}

#[test]
fn an_overwrite_of_a_note_that_does_not_exist_is_not_found() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    let err = v
        .index()
        .write_note("baseline/Missing", "# X\n", Some("3fa9c1e2b7d0"), LIMIT)
        .unwrap_err();
    assert!(matches!(err, Error::NoteNotFound(_)));
    assert_eq!(err.to_string(), "\"baseline/Missing\" not found");
    assert!(!v.exists("baseline/Missing.md"));
}

#[test]
fn the_archive_takes_creates_and_refuses_every_overwrite() {
    let v = TestVault::new(&[("archive/index.md", "# Archive\n")]);
    let record = "# Session\n\nRaw transcript.\n";
    // Create: allowed, in any spelling of the folder.
    assert_eq!(
        v.write("Archive/recognitor/Session 1", record, None),
        format!("archive/recognitor/Session 1  checksum: {}\n", sum(record))
    );
    // Overwrite: refused, with the right checksum too.
    let out = v.write("archive/recognitor/Session 1", "# Changed\n", Some(&sum(record)));
    assert_eq!(
        out,
        "Archive notes are append only: write_note never overwrites one. To add to it, use edit_note with append."
    );
    assert_eq!(v.file("archive/recognitor/Session 1.md"), record);
    // A create on an existing archive note is refused like any other.
    assert!(
        v.write("archive/recognitor/Session 1", "# X\n", None)
            .starts_with("The note already exists")
    );
}

#[test]
fn a_bootstrap_note_carries_the_notice_on_create_and_on_overwrite() {
    let v = TestVault::new(&[("bootstrap/index.md", "# Bootstrap\n")]);
    let out = v.write("Bootstrap/Profile", "# Profile\n", None);
    assert_eq!(
        out,
        format!(
            "bootstrap/Profile  checksum: {}\nnotice: bootstrap/Profile loads as instructions at session start\n",
            sum("# Profile\n")
        )
    );
    let out = v.write("bootstrap/Profile", "# Profile\n\nMore.\n", Some(&sum("# Profile\n")));
    assert!(
        out.ends_with("notice: bootstrap/Profile loads as instructions at session start\n"),
        "{out}"
    );
    // Knowledge notes carry no notice.
    let out = v.write("baseline/Sleep", NOTE, None);
    assert!(!out.contains("notice"), "{out}");
}

#[test]
fn a_new_note_resolves_the_ghost_links_that_named_it() {
    let v = TestVault::new(&[(
        "baseline/Linker.md",
        "# Linker\n\nSee [[Sleep]].\n\n## Relations\n- relates_to [[Sleep]]\n",
    )]);
    let mut index = v.index();
    let before = recognitor::context::render(&index.context("baseline/Linker", 1).unwrap());
    assert!(before.contains("(not written yet)"), "{before}");
    index.write_note("baseline/Sleep", NOTE, None, LIMIT).unwrap();
    let after = recognitor::context::render(&index.context("baseline/Linker", 1).unwrap());
    assert!(after.contains("relates_to: baseline/Sleep"), "{after}");
    assert!(!after.contains("(not written yet)"), "{after}");
}

#[test]
fn the_next_sync_after_writes_reads_no_files_and_finds_the_index_in_step() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    index
        .write_note("baseline/One", "# One\n\nFirst.\n", None, LIMIT)
        .unwrap();
    let one = sum("# One\n\nFirst.\n");
    index
        .write_note("baseline/One", "# One\n\nSecond.\n", Some(&one), LIMIT)
        .unwrap();
    index.write_note("Shelf/Two", "# Two\n", None, LIMIT).unwrap();
    // The files were saved after the last sweep started, so the first sweep re-hashes them once (the racy rule of
    // doc 02 §4); it finds nothing new, changed or gone, because the index already followed the files.
    let first = index.sync(SweepOptions::default()).unwrap();
    assert!(first.added.is_empty() && first.changed.is_empty() && first.deleted.is_empty());
    assert!(first.retitled.is_empty() && first.warnings.is_empty());
    assert_eq!(first.files_read, first.rehashed, "{first:?}");
    // The second sweep reads no file.
    let second = index.sync(SweepOptions::default()).unwrap();
    assert_eq!(second.files_read, 0, "{second:?}");
    // A fresh connection agrees.
    let mut again = v.index();
    assert_eq!(again.sync(SweepOptions::default()).unwrap().files_read, 0);
}

#[test]
fn a_refusal_changes_neither_the_disk_nor_the_index() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    for _ in 0..2 {
        index
            .write_note("baseline/Sleep", "# Other\n\nNot this.\n", None, LIMIT)
            .unwrap();
        index
            .write_note("baseline/Sleep", "# Other\n\nNot this.\n", Some("000000000000"), LIMIT)
            .unwrap();
    }
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
    let page = index.search("Not this", None, 1).unwrap();
    assert!(recognitor::search::render(&page).starts_with("No notes match"));
}

#[test]
fn empty_or_whitespace_content_is_refused_on_create_and_overwrite() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    for content in [
        "", " ", "

", " 	
 ",
    ] {
        assert_eq!(v.write("baseline/Empty", content, None), "write_note needs content");
        assert_eq!(
            v.write("baseline/Sleep", content, Some(&sum(NOTE))),
            "write_note needs content"
        );
    }
    assert!(!v.exists("baseline/Empty.md"));
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
}

#[test]
fn an_empty_expected_checksum_counts_as_absent_so_the_call_is_a_create() {
    let v = TestVault::new(&[("baseline/Sleep.md", NOTE)]);
    // A new path: created.
    assert_eq!(
        v.write(
            "baseline/Fresh",
            "# Fresh
",
            Some("")
        ),
        format!(
            "baseline/Fresh  checksum: {}
",
            sum("# Fresh
")
        )
    );
    // An existing note: refused like any create, never overwritten.
    let out = v.write(
        "baseline/Sleep",
        "# Other
",
        Some(""),
    );
    assert!(out.starts_with("The note already exists"), "{out}");
    assert_eq!(v.file("baseline/Sleep.md"), NOTE);
    // On an archive path an empty checksum is a create, not an overwrite.
    assert!(
        v.write(
            "archive/x/Record",
            "# R
",
            Some("")
        )
        .starts_with("archive/x/Record  checksum:")
    );
    // The checksum is matched ignoring case.
    let upper = sum(NOTE).to_uppercase();
    assert_eq!(
        v.write(
            "baseline/Sleep",
            "# Sleep
",
            Some(&upper)
        ),
        format!(
            "baseline/Sleep  checksum: {}
",
            sum("# Sleep
")
        )
    );
}

#[test]
fn content_is_written_as_given_without_changes() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n")]);
    let content = "no heading, CRLF\r\nline two\r\n\n\ttabbed \u{e9}\u{4e2d}\n";
    v.write("baseline/Raw", content, None);
    assert_eq!(fs::read(v.root.join("baseline/Raw.md")).unwrap(), content.as_bytes());
}

// Long notes (doc 07 §11, §5.3): an overwrite of a note over the read limit is refused, whatever the checksum.

const OVERWRITE_TOO_LONG: &str = "The note is too long to read whole, so write_note cannot replace it and nothing was written. Change it with edit_note, a section at a time.";

#[test]
fn an_overwrite_of_a_note_over_the_read_limit_is_refused_before_the_checksum_and_returns_no_note() {
    let long = long_note();
    let v = TestVault::new(&[("a/Long.md", &long)]);
    // The right checksum, from a cut read (doc 07 §7.2), and a wrong one get the same refusal, and nothing is returned.
    let shown = {
        let mut index = v.index();
        let cut = render_read(&index.read("a/Long", None, None, LIMIT).unwrap());
        assert!(cut.contains("[cut at 4000 tokens."), "the note should be cut");
        cut.lines()
            .next()
            .unwrap()
            .rsplit_once("checksum: ")
            .unwrap()
            .1
            .to_string()
    };
    assert_eq!(shown, sum(&long));
    for checksum in [shown.as_str(), "000000000000"] {
        assert_eq!(v.write("a/Long", "# New\n", Some(checksum)), OVERWRITE_TOO_LONG);
    }
    assert_eq!(v.file("a/Long.md"), long);
}

#[test]
fn the_read_limit_for_an_overwrite_is_the_one_read_note_cuts_at() {
    // About 200 tokens (estimate): whole at the default limit, cut at 50.
    let text = format!("# Mid\n\n## Part\n\n{}\n", "word ".repeat(160));
    let v = TestVault::new(&[("a/Mid.md", &text)]);
    let tight = v.index().write_note("a/Mid", "# New\n", Some(&sum(&text)), 50).unwrap();
    assert_eq!(tight, WriteNote::Refused(OVERWRITE_TOO_LONG.to_string()));
    assert_eq!(v.file("a/Mid.md"), text);
    let ok = v.write("a/Mid", "# New\n", Some(&sum(&text)));
    assert!(ok.starts_with("a/Mid  checksum: "), "{ok}");
}

/// Order of checks for an overwrite: not found, archive, read limit, checksum (doc 07 §11).
#[test]
fn an_overwrite_checks_not_found_then_the_archive_then_the_read_limit_then_the_checksum() {
    let long = long_note();
    let v = TestVault::new(&[
        ("archive/Record.md", &long),
        ("a/Long.md", &long),
        ("a/Short.md", "# Short\n\nText.\n"),
    ]);
    // Not found comes first, even for a path in the archive.
    let mut index = v.index();
    assert!(matches!(
        index.write_note("archive/Nothing", "# N\n", Some("000000000000"), LIMIT),
        Err(Error::NoteNotFound(p)) if p == "archive/Nothing"
    ));
    // The archive comes before the read limit: a long archive note gets the archive text.
    assert_eq!(
        v.write("archive/Record", "# N\n", Some(&sum(&long))),
        "Archive notes are append only: write_note never overwrites one. To add to it, use edit_note with append."
    );
    // The read limit comes before the checksum (above), and the checksum guards a short note.
    let stale = v.write("a/Short", "# N\n", Some("000000000000"));
    assert!(
        stale.starts_with("The note changed since that checksum, so nothing was written. Current version:"),
        "{stale}"
    );
}

/// A create on a path where a long note exists carries the note cut, with its checksum, after the variant text.
#[test]
fn a_create_on_a_long_note_names_edit_note_and_shows_the_cut_with_its_checksum() {
    let long = long_note();
    let v = TestVault::new(&[("a/Long.md", &long)]);
    let text = v.write("a/Long", "# New\n", None);
    assert!(
        text.starts_with(&format!(
            "The note already exists and is too long to replace whole, so nothing was written. Change it with edit_note. Current version:\na/Long  checksum: {}\n",
            sum(&long)
        )),
        "{}",
        &text[..220]
    );
    assert!(text.trim_end().ends_with(']'));
    assert_eq!(v.file("a/Long.md"), long);
}

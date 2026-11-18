//! `delete_note` tests (doc 07 §10, §5, §1.1, §6.3). Vaults live in temp folders that delete themselves. The tool's
//! argument checks are tested over MCP, in `tests/mcp.rs`.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use recognitor::delete_note::{DeleteNote, render};
use recognitor::{Error, Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

const LIMIT: usize = 4000;

/// 2023-11-14T22:13:20Z.
fn fixed_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

const STAMP: &str = ".recognitor/trash/2023-11-14T22-13-20Z";

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-delete-note-{}-{n}", std::process::id()));
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

    /// A `delete_note` call at a fixed time: the outcome.
    fn delete_at(&self, note: &str, checksum: &str, now: SystemTime) -> DeleteNote {
        self.index().delete_note_at(note, checksum, LIMIT, now).unwrap()
    }

    /// The text of a `delete_note` call: the result, or the refusal.
    fn delete(&self, note: &str, checksum: &str) -> String {
        render(&self.delete_at(note, checksum, fixed_time()))
    }

    /// `(notes, ghosts)` in the index after a sweep.
    fn counts(&self) -> (i64, i64) {
        let mut index = self.index();
        index.sync(SweepOptions::default()).unwrap();
        let c = index.connection();
        (
            c.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0)).unwrap(),
            c.query_row("SELECT COUNT(*) FROM edges WHERE target_key IS NULL", [], |r| r.get(0))
                .unwrap(),
        )
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

const CACHING: &str = "---\ntype: note\n---\n# Caching\n\nA cache keeps answers close.\n";
const AUTH: &str = "# Auth Flow\n\nSessions.\n\n## Relations\n- relates_to [[Caching]]\n";
const PLAN: &str = "# Plan\n\nSee [[ops::Caching]] and also [[ops::Caching|the cache]].\n";

fn vault() -> TestVault {
    TestVault::new(&[
        ("ops/Caching.md", CACHING),
        ("backend/Auth Flow.md", AUTH),
        ("design/Plan.md", PLAN),
        ("design/Other.md", "# Other\n\nNo links.\n"),
    ])
}

// The happy path.

#[test]
fn delete_moves_the_file_to_the_trash_and_lists_the_links_that_now_point_nowhere() {
    let v = vault();
    let text = v.delete("ops/Caching", &sum(CACHING));
    assert_eq!(
        text,
        format!(
            "deleted: ops/Caching\n\
             trash: {STAMP}/ops/Caching.md\n\
             broken links: 2 links in 2 notes\n\
             - backend/Auth Flow: relates_to Caching\n\
             - design/Plan: links_to ops::Caching\n"
        )
    );
    // The file is gone from its place and whole in the trash.
    assert!(!v.exists("ops/Caching.md"));
    assert_eq!(v.file(&format!("{STAMP}/ops/Caching.md")), CACHING);
    // No temporary file is left behind.
    let names: Vec<String> = fs::read_dir(v.root.join(STAMP).join("ops"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Caching.md"]);
}

#[test]
fn the_index_follows_the_delete_and_a_later_sweep_never_indexes_the_trash() {
    let v = vault();
    assert_eq!(v.counts(), (4, 0));
    v.delete("ops/Caching", &sum(CACHING));
    // Straight after the delete, and after a full rebuild: three notes, and the two links are ghosts.
    assert_eq!(v.counts(), (3, 2));
    let mut index = v.index();
    index.rebuild(SweepOptions::default()).unwrap();
    let (notes, ghosts): (i64, i64) = (
        index
            .connection()
            .query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))
            .unwrap(),
        index
            .connection()
            .query_row("SELECT COUNT(*) FROM edges WHERE target_key IS NULL", [], |r| r.get(0))
            .unwrap(),
    );
    assert_eq!((notes, ghosts), (3, 2));
    // The note is not found by name any more, and not in search.
    let found: i64 = index
        .connection()
        .query_row("SELECT COUNT(*) FROM notes WHERE key LIKE '%caching%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(found, 0);
}

#[test]
fn the_checksum_matches_ignoring_case_and_the_note_can_be_named_by_its_name() {
    let v = vault();
    let text = v.delete("CACHING", &sum(CACHING).to_uppercase());
    assert!(text.starts_with("deleted: ops/Caching\n"), "{text}");
}

#[test]
fn a_delete_with_nothing_pointing_to_the_note_says_so() {
    let v = vault();
    let text = v.delete("design/Other", &sum("# Other\n\nNo links.\n"));
    assert_eq!(
        text,
        format!("deleted: design/Other\ntrash: {STAMP}/design/Other.md\nbroken links: none\n")
    );
}

// The guard (doc 07 §5.2, §5.3).

#[test]
fn a_stale_checksum_deletes_nothing_and_returns_the_current_note() {
    let v = vault();
    let stale = sum(CACHING);
    // A human saves the file after the checksum was read, inside the sweep window.
    let changed = "---\ntype: note\n---\n# Caching\n\nA human edited this.\n";
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    v.put("ops/Caching.md", changed);
    let out = index
        .delete_note_at("ops/Caching", &stale, LIMIT, fixed_time())
        .unwrap();
    let DeleteNote::Refused(text) = out else {
        panic!("expected a refusal, got {out:?}");
    };
    assert_eq!(
        text,
        format!(
            "The note changed since that checksum, so nothing was deleted. Current version:\n\
             ops/Caching  checksum: {}\n{changed}",
            sum(changed)
        )
    );
    assert_eq!(v.file("ops/Caching.md"), changed);
    assert!(!v.exists(".recognitor"));
    // The index still has the note, and its links still resolve.
    assert_eq!(v.counts(), (4, 0));
    // The retry with the new checksum works.
    let again = v.delete("ops/Caching", &sum(changed));
    assert!(again.starts_with("deleted: ops/Caching\n"), "{again}");
}

#[test]
fn a_whole_checksum_or_a_wrong_one_is_stale() {
    let v = vault();
    for bad in ["000000000000", "", &recognitor::hash::sha256_hex(CACHING.as_bytes())] {
        let out = v.delete_at("ops/Caching", bad, fixed_time());
        assert!(
            matches!(&out, DeleteNote::Refused(t) if t.starts_with("The note changed since that checksum")),
            "{bad}: {out:?}"
        );
    }
    assert!(v.exists("ops/Caching.md"));
}

const TOO_LONG: &str = "The note is too long to read whole, so delete_note cannot check its checksum and nothing was deleted. Only the user can delete the file.";

#[test]
fn a_note_over_the_read_limit_is_refused_before_the_checksum_and_returns_no_note() {
    let mut long = String::from("# Long\n\nSummary.\n");
    for i in 0..40 {
        long.push_str(&format!("\n## Part {i}\n\n{}\n", "word ".repeat(400)));
    }
    let v = TestVault::new(&[("ops/Long.md", &long)]);
    // A wrong checksum and the right one get the same refusal: it comes before the guard, and nothing is returned.
    for checksum in ["000000000000".to_string(), sum(&long)] {
        assert_eq!(
            v.delete_at("ops/Long", &checksum, fixed_time()),
            DeleteNote::Refused(TOO_LONG.to_string())
        );
    }
    assert_eq!(v.file("ops/Long.md"), long);
    assert!(!v.exists(".recognitor"));
    assert_eq!(v.counts(), (1, 0));
}

#[test]
fn the_read_limit_is_the_one_read_note_cuts_at() {
    // About 200 tokens (estimate) of text: whole at the default limit, cut at 50, as `read_note` decides.
    let text = format!("# Mid\n\n## Part\n\n{}\n", "word ".repeat(160));
    let v = TestVault::new(&[("ops/Mid.md", &text)]);
    let tight = v
        .index()
        .delete_note_at("ops/Mid", &sum(&text), 50, fixed_time())
        .unwrap();
    assert_eq!(tight, DeleteNote::Refused(TOO_LONG.to_string()));
    assert!(v.exists("ops/Mid.md"));
    let default = v.delete("ops/Mid", &sum(&text));
    assert!(default.starts_with("deleted: ops/Mid\n"), "{default}");
}

// Stores (doc 07 §1.1).

#[test]
fn an_archive_note_is_never_deleted_even_with_its_checksum() {
    let record = "# Session\n\nA record.\n";
    let v = TestVault::new(&[("archive/2026-11-01 session.md", record), ("ops/Caching.md", CACHING)]);
    let text = v.delete("archive/2026-11-01 session", &sum(record));
    assert_eq!(
        text,
        "Archive notes are append only: delete_note never removes one. Only the user can delete the file."
    );
    assert!(matches!(
        v.delete_at("Archive/2026-11-01 session", &sum(record), fixed_time()),
        DeleteNote::Refused(_)
    ));
    assert_eq!(v.file("archive/2026-11-01 session.md"), record);
    assert!(!v.exists(".recognitor"));
}

#[test]
fn a_bootstrap_note_is_deleted_with_the_notice_and_leaves_a_ghost_in_the_tree() {
    let index_note = "# Bootstrap\n\nStanding instructions.\n\n## Relations\n- indexes [[Profile]]\n";
    let profile = "# Profile\n\nThe user likes short answers.\n";
    let v = TestVault::new(&[("bootstrap/index.md", index_note), ("bootstrap/Profile.md", profile)]);
    let text = v.delete("bootstrap/Profile", &sum(profile));
    assert_eq!(
        text,
        format!(
            "deleted: bootstrap/Profile\n\
             trash: {STAMP}/bootstrap/Profile.md\n\
             broken links: 1 link in 1 note\n\
             - bootstrap/index, titled \"Bootstrap\": indexes Profile\n\
             notice: bootstrap/Profile no longer loads as instructions at session start\n"
        )
    );
    let payload = v.index().bootstrap().unwrap().render(8000);
    assert!(payload.contains("not loaded: Profile (not written yet)"), "{payload}");
    assert!(!payload.contains("short answers"));
}

// Names (doc 07 §6.3).

#[test]
fn a_name_that_matches_several_notes_runs_on_none_of_them() {
    let v = TestVault::new(&[("ops/Caching.md", CACHING), ("research/Caching.md", CACHING)]);
    let out = v.delete_at("Caching", &sum(CACHING), fixed_time());
    assert_eq!(
        out,
        DeleteNote::Several(
            "\"Caching\" matches 2 notes. Call again with one id:\n  ops/Caching\n  research/Caching\n".to_string()
        )
    );
    assert_eq!(
        render(&out),
        "\"Caching\" matches 2 notes. Call again with one id:\n  ops/Caching\n  research/Caching\n"
    );
    assert!(v.exists("ops/Caching.md") && v.exists("research/Caching.md"));
    assert!(!v.exists(".recognitor"));
    // `ops::Caching` always matches one.
    let text = v.delete("ops::Caching", &sum(CACHING));
    assert!(text.starts_with("deleted: ops/Caching\n"), "{text}");
}

#[test]
fn a_note_that_is_not_there_is_not_found_quoting_the_name_as_given() {
    let v = vault();
    let err = v
        .index()
        .delete_note_at("Nope/Gone", "000000000000", LIMIT, fixed_time());
    assert!(
        matches!(&err, Err(Error::NoteNotFound(n)) if n == "Nope/Gone"),
        "{err:?}"
    );
    assert_eq!(err.unwrap_err().to_string(), "\"Nope/Gone\" not found");
}

#[test]
fn a_note_the_index_knows_but_the_disk_lost_is_not_found() {
    let v = vault();
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    fs::remove_file(v.root.join("ops/Caching.md")).unwrap();
    let err = index.delete_note_at("ops/Caching", &sum(CACHING), LIMIT, fixed_time());
    assert!(matches!(err, Err(Error::NoteNotFound(_))));
}

// The trash (doc 07 §10).

#[test]
fn two_deletes_of_the_same_path_keep_both_versions() {
    let v = vault();
    let first = v.delete("ops/Caching", &sum(CACHING));
    assert!(first.contains(&format!("trash: {STAMP}/ops/Caching.md\n")), "{first}");
    // The path is written again, and deleted in the same second and then a minute later.
    let second_text = "# Caching\n\nSecond life.\n";
    v.put("ops/Caching.md", second_text);
    v.counts(); // a sweep, as the next call's 2-second gate would not have run one yet
    let second = v.delete("ops/Caching", &sum(second_text));
    assert!(
        second.contains(&format!("trash: {STAMP}-2/ops/Caching.md\n")),
        "{second}"
    );
    let third_text = "# Caching\n\nThird life.\n";
    v.put("ops/Caching.md", third_text);
    v.counts();
    let later = fixed_time() + Duration::from_secs(60);
    let third = render(&v.delete_at("ops/Caching", &sum(third_text), later));
    assert!(
        third.contains("trash: .recognitor/trash/2023-11-14T22-14-20Z/ops/Caching.md\n"),
        "{third}"
    );
    assert_eq!(v.file(&format!("{STAMP}/ops/Caching.md")), CACHING);
    assert_eq!(v.file(&format!("{STAMP}-2/ops/Caching.md")), second_text);
    assert_eq!(
        v.file(".recognitor/trash/2023-11-14T22-14-20Z/ops/Caching.md"),
        third_text
    );
}

#[test]
fn the_trash_keeps_the_folders_of_the_original_path() {
    let deep = "# Deep\n\nDown here.\n";
    let v = TestVault::new(&[("Ops/runbooks/db/Deep.md", deep)]);
    let text = v.delete("Ops/runbooks/db/Deep", &sum(deep));
    assert!(
        text.contains(&format!("trash: {STAMP}/Ops/runbooks/db/Deep.md\n")),
        "{text}"
    );
    assert_eq!(v.file(&format!("{STAMP}/Ops/runbooks/db/Deep.md")), deep);
}

// The broken-links list (doc 07 §10).

#[test]
fn a_link_that_now_reaches_another_note_is_listed_with_that_note() {
    // `[[Caching]]` in ops reached ops/Caching by the namespace rule; with it gone the name is unique anywhere, so
    // it now reaches research/Caching. A link that was a ghost before, and a link to the note itself, are not listed.
    let v = TestVault::new(&[
        ("ops/Caching.md", CACHING),
        ("research/Caching.md", "# Caching\n\nResearch.\n"),
        ("ops/Runbook.md", "# Runbook\n\nSee [[Caching]].\n"),
        ("ops/Self.md", "# Self\n\nSee [[Self]] and [[Gone Already]].\n"),
    ]);
    let text = v.delete("ops/Caching", &sum(CACHING));
    assert_eq!(
        text,
        format!(
            "deleted: ops/Caching\n\
             trash: {STAMP}/ops/Caching.md\n\
             broken links: 1 link in 1 note\n\
             - ops/Runbook: links_to Caching  (now research/Caching)\n"
        )
    );
    // The redirect is not a ghost: the one ghost left is the one that was always one.
    assert_eq!(v.counts(), (3, 1));
}

/// Doc 07 §10's example, with the one change its third line needs: `[[ideas/Caching]]` written in `ops` would look for
/// `ops/ideas/Caching` (doc 03 §4, step 1), so the link there is written `[[../ideas/Caching]]`.
#[test]
fn the_docs_example_comes_out_of_the_code() {
    let caching = "# Caching\n\nIdeas.\n";
    let v = TestVault::new(&[
        ("ideas/Caching.md", caching),
        ("ideas/index.md", "# index\n\n## Relations\n- indexes [[Caching]]\n"),
        ("design/Caching.md", "# Caching\n\nDesign.\n"),
        ("research/Caching.md", "# Caching\n\nResearch.\n"),
        (
            "ops/Runbook.md",
            "# Runbook v2\n\nSee [[ideas::Caching]].\n\n## Relations\n- relates_to [[../ideas/Caching]]\n",
        ),
    ]);
    // 2026-11-18T19:18:52Z, the doc's time.
    let at = UNIX_EPOCH + Duration::from_secs(1_795_029_532);
    let out = v.delete_at("ideas/Caching", &sum(caching), at);
    assert_eq!(
        render(&out),
        "deleted: ideas/Caching\n\
         trash: .recognitor/trash/2026-11-18T19-18-52Z/ideas/Caching.md\n\
         broken links: 3 links in 2 notes\n\
         - ideas/index: indexes Caching  (ambiguous)\n\
         - ops/Runbook, titled \"Runbook v2\": links_to ideas::Caching\n\
         - ops/Runbook, titled \"Runbook v2\": relates_to ../ideas/Caching\n"
    );
}

#[test]
fn a_name_that_now_matches_two_notes_is_listed_as_ambiguous() {
    let v = TestVault::new(&[
        ("ops/Caching.md", CACHING),
        ("research/Caching.md", "# Caching\n\nResearch.\n"),
        ("design/Caching.md", "# Caching\n\nDesign.\n"),
        ("ops/Runbook.md", "# Runbook\n\nSee [[Caching]].\n"),
    ]);
    let text = v.delete("ops/Caching", &sum(CACHING));
    assert!(
        text.ends_with("broken links: 1 link in 1 note\n- ops/Runbook: links_to Caching  (ambiguous)\n"),
        "{text}"
    );
}

#[test]
fn a_link_to_the_notes_title_and_a_markdown_link_are_listed_as_written() {
    let v = TestVault::new(&[
        ("ops/Caching.md", "# Cache design\n\nText.\n"),
        ("a/Zed.md", "# Zed\n\nSee [[Cache design]].\n"),
        (
            "a/alpha.md",
            "# The Alpha\n\n## Relations\n- depends_on [[ops::Cache design]]\n\nAlso [x](../ops/Caching.md).\n",
        ),
    ]);
    let text = v.delete("ops/Caching", &sum("# Cache design\n\nText.\n"));
    assert_eq!(
        text,
        format!(
            "deleted: ops/Caching\n\
             trash: {STAMP}/ops/Caching.md\n\
             broken links: 3 links in 2 notes\n\
             - a/alpha, titled \"The Alpha\": depends_on ops::Cache design\n\
             - a/alpha, titled \"The Alpha\": links_to ../ops/Caching.md\n\
             - a/Zed: links_to Cache design\n"
        )
    );
}

#[test]
fn a_link_that_was_a_ghost_before_is_not_listed() {
    let v = TestVault::new(&[
        ("ops/Caching.md", CACHING),
        ("a/B.md", "# B\n\nSee [[Caching]] and [[Never Written]].\n"),
    ]);
    let text = v.delete("ops/Caching", &sum(CACHING));
    assert!(
        text.ends_with("broken links: 1 link in 1 note\n- a/B: links_to Caching\n"),
        "{text}"
    );
}

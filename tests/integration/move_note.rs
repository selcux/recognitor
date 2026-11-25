//! `move_note` tests (doc 07 §6.1, §11, §5, §1.1, §6.3). Vaults live in temp folders that delete themselves. The tool's
//! argument checks are tested over MCP, in `tests/mcp.rs`. Case twins can exist only on a case-sensitive file system, so
//! the test that needs them probes for one and stops early where there is none.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::move_note::{MoveNote, render};
use recognitor::{Error, Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

const LIMIT: usize = 4000;

struct TestVault {
    root: PathBuf,
    db: PathBuf,
    base: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-move-note-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let v = TestVault {
            root,
            db: base.join("index.db"),
            base,
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

    /// A `move_note` call: the outcome.
    fn move_to(&self, note: &str, new_path: &str, checksum: &str) -> MoveNote {
        self.index().move_note(note, new_path, checksum, LIMIT).unwrap()
    }

    /// The text of a `move_note` call: the result, or the refusal.
    fn mv(&self, note: &str, new_path: &str, checksum: &str) -> String {
        render(&self.move_to(note, new_path, checksum))
    }

    /// The text of a `move_note` call with the right checksum, read from the file as it is now.
    fn mv_ok(&self, note: &str, new_path: &str) -> String {
        let rel = self.path_of(note);
        self.mv(note, new_path, &sum(&self.file(&rel)))
    }

    /// The path of the file a note argument names, found by key on the disk.
    fn path_of(&self, note: &str) -> String {
        let key = note.to_lowercase();
        let mut found = Vec::new();
        walk(&self.root, &self.root, &mut found);
        found
            .into_iter()
            .find(|p| p.to_lowercase().trim_end_matches(".md") == key.trim_end_matches(".md"))
            .unwrap_or_else(|| panic!("no file for {note}"))
    }

    /// The index after a sweep, and the same vault indexed from scratch in another database: they must agree.
    fn assert_index_matches_a_rebuild(&self) {
        let mut index = self.index();
        index.sync(SweepOptions::default()).unwrap();
        let fresh_db = self.base.join("fresh.db");
        let _ = fs::remove_file(&fresh_db);
        let mut fresh = Index::open(&self.root, Some(&fresh_db)).unwrap();
        fresh.rebuild(SweepOptions::default()).unwrap();
        assert_eq!(state(&index), state(&fresh));
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            walk(root, &e.path(), out);
        } else if name.to_lowercase().ends_with(".md") {
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
}

/// Notes, links and their targets, observations and the full-text hits of a fixed word: what a rebuild must also give.
fn state(index: &Index) -> Vec<String> {
    let c = index.connection();
    let mut out = Vec::new();
    let mut notes = c
        .prepare("SELECT key, namespace, title, path, summary FROM notes ORDER BY key")
        .unwrap();
    for r in notes
        .query_map([], |r| {
            Ok(format!(
                "note {} {} {} {} {:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?
            ))
        })
        .unwrap()
    {
        out.push(r.unwrap());
    }
    let mut edges = c
        .prepare(
            "SELECT source_key, predicate, ref_key, target_ref, target_key FROM edges \
             ORDER BY source_key, predicate, ref_key",
        )
        .unwrap();
    for r in edges
        .query_map([], |r| {
            Ok(format!(
                "edge {} {} {} {} {:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?
            ))
        })
        .unwrap()
    {
        out.push(r.unwrap());
    }
    let mut obs = c
        .prepare("SELECT note_key, category, content FROM observations ORDER BY note_key, line_number")
        .unwrap();
    for r in obs
        .query_map([], |r| {
            Ok(format!(
                "obs {} {} {}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        })
        .unwrap()
    {
        out.push(r.unwrap());
    }
    let hits: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM notes_fts WHERE notes_fts MATCH 'cache'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    out.push(format!("fts cache {hits}"));
    out
}

fn sum(text: &str) -> String {
    recognitor::hash::sha256_prefix(text.as_bytes(), 12)
}

const CACHING: &str = "---\ntype: note\n---\n# Caching\n\nA cache keeps answers close.\n\n## Observations\n- [fact] A cache is fast\n\n## Relations\n- relates_to [[./Setup]]\n";
const SETUP: &str = "# Setup\n\nHow to set it up.\n";
const IDEAS_INDEX: &str = "# Ideas\n\nAll ideas.\n\n## Relations\n- indexes [[Caching]]\n";
const RUNBOOK: &str = "# Runbook v2\n\nSteps.\n\nSee [[ideas::Caching]] and [the note](../ideas/Caching.md).\n\n## Relations\n- relates_to [[../ideas/Caching]]\n";
const NOTES: &str = "# Notes\n\nPlain name: [[Caching]].\n";

fn vault() -> TestVault {
    TestVault::new(&[
        ("ideas/Caching.md", CACHING),
        ("ideas/Setup.md", SETUP),
        ("ideas/index.md", IDEAS_INDEX),
        ("ops/Runbook.md", RUNBOOK),
        ("backend/Notes.md", NOTES),
        ("design/index.md", "# Design\n"),
    ])
}

// The happy path and the report of doc 07 §6.1.

#[test]
fn a_cross_namespace_move_reports_every_link_it_broke_and_gives_the_relink_call() {
    let v = vault();
    let text = v.mv("ideas/Caching", "design/b/Caching", &sum(CACHING));
    assert_eq!(
        text,
        "moved: ideas/Caching -> design/b/Caching\n\
         broken links: 4 links in 2 notes\n\
         - design/b/Caching: relates_to ./Setup\n\
         - ops/Runbook, titled \"Runbook v2\": links_to ../ideas/Caching.md\n\
         - ops/Runbook, titled \"Runbook v2\": links_to ideas::Caching\n\
         - ops/Runbook, titled \"Runbook v2\": relates_to ../ideas/Caching\n\
         relink(old: \"ideas/Caching\", new: \"design/b/Caching\", notes: [\"ops/Runbook\"])\n"
    );
    // The file moved whole, and the folder it left stays.
    assert!(!v.exists("ideas/Caching.md"));
    assert_eq!(v.file("design/b/Caching.md"), CACHING);
    assert!(v.exists("ideas/Setup.md"));
}

#[test]
fn the_index_follows_the_file_exactly_as_a_rebuild_would_have_it() {
    let v = vault();
    v.mv_ok("ideas/Caching", "design/b/Caching");
    v.assert_index_matches_a_rebuild();
    let index = v.index();
    let c = index.connection();
    let keys: Vec<String> = c
        .prepare("SELECT key FROM notes WHERE key LIKE '%caching'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(keys, ["design/b/caching"]);
}

#[test]
fn the_printed_relink_call_is_one_relink_accepts_and_the_links_resolve_again() {
    let v = vault();
    let text = v.mv_ok("ideas/Caching", "design/b/Caching");
    let call = text.lines().find(|l| l.starts_with("relink(")).unwrap();
    // old: "..", new: "..", notes: [".."] : the arguments as written.
    let quoted: Vec<&str> = call.split('"').skip(1).step_by(2).collect();
    assert_eq!(quoted, ["ideas/Caching", "design/b/Caching", "ops/Runbook"]);
    let relinked = v
        .index()
        .relink(
            quoted[0],
            quoted[1],
            &quoted[2..].iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(recognitor::relink::render(&relinked), "relinked: 3 links in 1 note\n");
    let ghosts: i64 = {
        let mut index = v.index();
        index.sync(SweepOptions::default()).unwrap();
        index
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE target_key IS NULL AND source_key = 'ops/runbook'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(ghosts, 0);
    v.assert_index_matches_a_rebuild();
}

#[test]
fn a_rename_in_place_keeps_name_links_and_lists_nothing() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", CACHING),
        ("ideas/Setup.md", SETUP),
        ("ideas/index.md", IDEAS_INDEX),
        ("backend/Notes.md", NOTES),
    ]);
    // `[[Caching]]` names the title too, so a new filename keeps it working.
    let text = v.mv_ok("ideas/Caching", "ideas/Cache layer");
    assert_eq!(text, "moved: ideas/Caching -> ideas/Cache layer\nbroken links: none\n");
    v.assert_index_matches_a_rebuild();
}

#[test]
fn a_name_that_now_matches_two_notes_is_ambiguous_and_one_that_now_prefers_the_moved_note_is_redirected() {
    let v = TestVault::new(&[
        ("ops/Caching.md", "# Caching\n\nOps cache.\n"),
        ("ideas/Draft.md", "# Draft\n\nA draft.\n"),
        ("ideas/Own.md", "# Own\n\nSee [[Caching]].\n"),
        ("backend/Users.md", "# Users\n\nSee [[Caching]].\n"),
    ]);
    let text = v.mv_ok("ideas/Draft", "ideas/Caching");
    assert_eq!(
        text,
        "moved: ideas/Draft -> ideas/Caching\n\
         broken links: 2 links in 2 notes\n\
         - backend/Users: links_to Caching  (ambiguous)\n\
         - ideas/Own: links_to Caching  (now ideas/Caching)\n"
    );
    // The two links never pointed to the moved note, so `relink` has nothing to fix and no call is printed.
    assert!(!text.contains("relink("));
    v.assert_index_matches_a_rebuild();
}

#[test]
fn a_link_that_led_nowhere_before_is_not_listed_even_when_it_reaches_the_note_now() {
    let v = TestVault::new(&[
        ("ideas/Draft.md", "# Draft\n\nA draft.\n"),
        ("backend/Wants.md", "# Wants\n\nSee [[Caching]].\n"),
    ]);
    assert_eq!(
        v.mv_ok("ideas/Draft", "ideas/Caching"),
        "moved: ideas/Draft -> ideas/Caching\nbroken links: none\n"
    );
    v.assert_index_matches_a_rebuild();
}

#[test]
fn the_moved_notes_relative_links_are_listed_when_the_new_folder_cannot_reach_them() {
    let v = vault();
    // `[[./Setup]]` starts from the new folder now; `ideas/b/Setup` does not exist.
    let text = v.mv_ok("ideas/Caching", "ideas/b/Caching");
    assert_eq!(
        text,
        "moved: ideas/Caching -> ideas/b/Caching\n\
         broken links: 3 links in 2 notes\n\
         - ideas/b/Caching: relates_to ./Setup\n\
         - ops/Runbook, titled \"Runbook v2\": links_to ../ideas/Caching.md\n\
         - ops/Runbook, titled \"Runbook v2\": relates_to ../ideas/Caching\n\
         relink(old: \"ideas/Caching\", new: \"ideas/b/Caching\", notes: [\"ops/Runbook\"])\n"
    );
    v.assert_index_matches_a_rebuild();
}

#[test]
fn a_move_that_leaves_every_relative_link_working_lists_none_of_them() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", CACHING),
        ("ideas/Setup.md", SETUP),
        ("ideas/b/Setup.md", "# Setup\n\nAnother.\n"),
    ]);
    // `./Setup` resolves to another note from the new folder: the link changed target, so it is listed with the note.
    assert_eq!(
        v.mv_ok("ideas/Caching", "ideas/b/Caching"),
        "moved: ideas/Caching -> ideas/b/Caching\n\
         broken links: 1 link in 1 note\n\
         - ideas/b/Caching: relates_to ./Setup  (now ideas/b/Setup)\n"
    );
}

// A case-only rename (doc 07 §11).

#[test]
fn a_case_only_rename_changes_the_spelling_and_breaks_nothing() {
    let v = TestVault::new(&[
        ("ideas/caching.md", "# caching\n\nA cache.\n"),
        ("ops/Runbook.md", "# Runbook\n\nSee [[ideas::caching]].\n"),
    ]);
    let text = v.mv_ok("ideas/caching", "ideas/Caching");
    assert_eq!(text, "moved: ideas/caching -> ideas/Caching\nbroken links: none\n");
    assert!(v.exists("ideas/Caching.md"));
    let names: Vec<String> = fs::read_dir(v.root.join("ideas"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Caching.md"], "no temporary file is left behind");
    v.assert_index_matches_a_rebuild();
}

#[test]
fn the_extension_alone_can_change_case_and_the_same_path_is_refused() {
    let v = TestVault::new(&[("ideas/Note.MD", "# Note\n")]);
    assert_eq!(
        v.mv_ok("ideas/Note", "ideas/Note"),
        "moved: ideas/Note -> ideas/Note\nbroken links: none\n"
    );
    assert!(v.exists("ideas/Note.md"));
    // Now it is already there.
    assert_eq!(
        v.mv_ok("ideas/Note", "Ideas/Note.md"),
        "The note is already at ideas/Note, so nothing was moved."
    );
}

// Refusals (doc 07 §11, §5.3).

#[test]
fn a_move_onto_an_existing_note_is_refused_and_returns_that_note() {
    let v = vault();
    let out = v.mv("ideas/Caching", "ideas/Setup", &sum(CACHING));
    assert_eq!(
        out,
        format!(
            "A note already exists at that path, so nothing was moved. Current version:\n\
             ideas/Setup  checksum: {}\n{SETUP}",
            sum(SETUP)
        )
    );
    // Another case of the same name is the same key.
    let out = v.mv("ideas/Caching", "Ideas/SETUP.md", &sum(CACHING));
    assert!(out.contains(&format!("ideas/Setup  checksum: {}", sum(SETUP))), "{out}");
    assert_eq!(v.file("ideas/Caching.md"), CACHING);
    assert_eq!(v.file("ideas/Setup.md"), SETUP);
}

#[test]
fn a_file_the_index_does_not_know_yet_still_blocks_the_move() {
    let v = vault();
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    // A human saves it after the sweep: the check reads the disk, not the index (doc 07 §5.1 step 2).
    v.put("design/Fresh.md", "# Fresh\n\nMine.\n");
    let out = render(
        &index
            .move_note("ideas/Caching", "design/fresh", &sum(CACHING), LIMIT)
            .unwrap(),
    );
    assert!(
        out.starts_with("A note already exists at that path, so nothing was moved."),
        "{out}"
    );
    assert_eq!(v.file("design/Fresh.md"), "# Fresh\n\nMine.\n");
    assert!(v.exists("ideas/Caching.md"));
}

#[test]
fn a_stale_checksum_is_refused_with_the_current_note() {
    let v = vault();
    for bad in ["000000000000", "", &recognitor::hash::sha256_hex(CACHING.as_bytes())] {
        let out = v.mv("ideas/Caching", "design/Caching", bad);
        assert_eq!(
            out,
            format!(
                "The note changed since that checksum, so nothing was moved. Current version:\n\
                 ideas/Caching  checksum: {}\n{CACHING}",
                sum(CACHING)
            ),
            "{bad}"
        );
    }
    assert!(v.exists("ideas/Caching.md"));
    assert!(!v.exists("design/Caching.md"));
    // The checksum is matched ignoring case, and a human edit after the sweep makes the old checksum stale.
    let upper = sum(CACHING).to_uppercase();
    v.put("ideas/Setup.md", "# Setup\n\nEdited by a human.\n");
    let stale = v.mv("ideas/Setup", "design/Setup", &sum(SETUP));
    assert!(stale.contains("Edited by a human."), "{stale}");
    assert!(v.mv("ideas/Caching", "design/Caching", &upper).starts_with("moved:"));
}

#[test]
fn bad_target_paths_are_refused_before_anything_moves() {
    let v = vault();
    let cases = [
        (
            "Caching.md",
            "The path has no top-level folder. Write it from the vault root, as folder/name.",
        ),
        (
            "caf\u{e9}/Caching",
            "The path has a non-ASCII character. Note and folder names are ASCII only.",
        ),
        (
            "design/../x",
            "The path is not a note path. Use folder/name from the vault root, with no empty, hidden or \"..\" parts and none of \\ : * ? \" < > |.",
        ),
        (
            ".recognitor/Caching",
            "The path is not a note path. Use folder/name from the vault root, with no empty, hidden or \"..\" parts and none of \\ : * ? \" < > |.",
        ),
    ];
    for (path, expected) in cases {
        assert_eq!(v.mv("ideas/Caching", path, &sum(CACHING)), expected, "{path}");
    }
    assert_eq!(v.file("ideas/Caching.md"), CACHING);
}

#[test]
fn a_new_path_reuses_existing_folder_spelling_and_creates_the_rest() {
    let v = vault();
    let text = v.mv_ok("ideas/Caching", "Design/Deep Notes/Caching.md");
    assert!(
        text.starts_with("moved: ideas/Caching -> design/Deep Notes/Caching\n"),
        "{text}"
    );
    assert!(v.exists("design/Deep Notes/Caching.md"));
    let top: Vec<String> = {
        let mut t: Vec<String> = fs::read_dir(&v.root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        t.sort();
        t
    };
    assert_eq!(top, ["backend", "design", "ideas", "ops"]);
    // A new top-level folder keeps its typed spelling.
    let text = v.mv_ok("design/index", "Shelf/Start");
    assert!(text.starts_with("moved: design/index -> Shelf/Start\n"), "{text}");
    assert!(v.exists("Shelf/Start.md"));
}

#[test]
fn a_move_onto_case_twins_is_refused_with_every_path() {
    let probe = std::env::temp_dir().join(format!("recognitor-move-probe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&probe);
    let sensitive = make_case_sensitive(&probe);
    let _ = fs::remove_dir_all(&probe);
    if !sensitive {
        return;
    }
    let v = TestVault::new(&[]);
    make_case_sensitive(&v.root);
    v.put("ideas/Caching.md", CACHING);
    v.put("design/Plan.md", "# Plan\n");
    v.put("design/plan.md", "# plan\n");
    let out = v.mv("ideas/Caching", "design/PLAN", &sum(CACHING));
    assert_eq!(
        out,
        "Names that differ only in case cannot exist together, so nothing was moved. On disk:\n  design/Plan.md\n  design/plan.md\n"
    );
    assert!(v.exists("ideas/Caching.md"));
}

fn make_case_sensitive(dir: &Path) -> bool {
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

#[test]
fn a_missing_note_is_an_error_and_several_matches_are_a_normal_result() {
    let v = TestVault::new(&[
        ("ops/Caching.md", "# Caching\n"),
        ("research/Caching.md", "# Caching\n"),
    ]);
    assert!(matches!(
        v.index().move_note("ideas/Nothing", "ops/X", "000000000000", LIMIT),
        Err(Error::NoteNotFound(n)) if n == "ideas/Nothing"
    ));
    assert_eq!(
        v.mv("Caching", "ops/X", "000000000000"),
        "\"Caching\" matches 2 notes. Call again with one id:\n  ops/Caching\n  research/Caching\n"
    );
    assert!(v.exists("ops/Caching.md") && v.exists("research/Caching.md"));
    // A file the index knows and the disk lost is not found.
    let mut index = v.index();
    index.sync(SweepOptions::default()).unwrap();
    fs::remove_file(v.root.join("ops/Caching.md")).unwrap();
    assert!(matches!(
        index.move_note("ops/Caching", "ops/X", "000000000000", LIMIT),
        Err(Error::NoteNotFound(_))
    ));
}

// Stores (doc 07 §1.1).

const RECORD: &str = "# Session\n\nA record of a session about a cache.\n\nSee [[Caching]].\n";

#[test]
fn a_move_out_of_the_archive_or_inside_it_is_refused_even_with_the_checksum() {
    let v = TestVault::new(&[("archive/2026-11-01 session.md", RECORD), ("ideas/Caching.md", CACHING)]);
    let refused = "Archive notes are append only: move_note never moves one. Only the user can move the file.";
    assert_eq!(
        v.mv("archive/2026-11-01 session", "ideas/Session", &sum(RECORD)),
        refused
    );
    assert_eq!(
        v.mv("Archive/2026-11-01 session", "archive/Renamed", &sum(RECORD)),
        refused
    );
    assert_eq!(v.file("archive/2026-11-01 session.md"), RECORD);
    assert!(!v.exists("ideas/Session.md"));
}

#[test]
fn a_move_into_the_archive_counts_as_a_create_and_the_note_loses_its_links() {
    let v = vault();
    let text = v.mv_ok("ideas/Caching", "archive/Caching");
    assert!(text.starts_with("moved: ideas/Caching -> archive/Caching\n"), "{text}");
    assert!(v.exists("archive/Caching.md"));
    let index = v.index();
    let c = index.connection();
    let (ns, edges): (String, i64) = c
        .query_row(
            "SELECT n.namespace, (SELECT COUNT(*) FROM edges WHERE source_key = n.key) FROM notes n WHERE n.key = 'archive/caching'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (ns.as_str(), edges),
        ("archive", 0),
        "archive notes have no edges (doc 03 §3)"
    );
    drop(index);
    v.assert_index_matches_a_rebuild();
}

#[test]
fn a_move_into_the_bootstrap_carries_the_notice_and_so_does_a_move_out_of_it() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Bootstrap\n\n## Relations\n- indexes [[Profile]]\n",
        ),
        ("bootstrap/Profile.md", "# Profile\n\nShort answers.\n"),
        ("ideas/Caching.md", CACHING),
        ("ideas/Setup.md", SETUP),
    ]);
    // Into the bootstrap.
    let text = v.mv_ok("ideas/Setup", "bootstrap/Setup");
    assert_eq!(
        text,
        "moved: ideas/Setup -> bootstrap/Setup\n\
         broken links: 1 link in 1 note\n\
         - ideas/Caching: relates_to ./Setup\n\
         relink(old: \"ideas/Setup\", new: \"bootstrap/Setup\", notes: [\"ideas/Caching\"])\n\
         notice: bootstrap/Setup loads as instructions at session start\n"
    );
    // A rename inside the bootstrap: it still loads, and the notice names the new id.
    let text = v.mv_ok("bootstrap/Setup", "bootstrap/Setup guide");
    assert!(
        text.ends_with("notice: bootstrap/Setup guide loads as instructions at session start\n"),
        "{text}"
    );
    // Out of the bootstrap: it no longer loads. The name link in the bootstrap still reaches the note (step 4), so it is
    // not listed: broken means changed.
    let text = v.mv_ok("bootstrap/Profile", "ideas/Profile");
    assert_eq!(
        text,
        "moved: bootstrap/Profile -> ideas/Profile\n\
         broken links: none\n\
         notice: bootstrap/Profile no longer loads as instructions at session start\n"
    );
}

// Long notes (doc 07 §7.2, §11): a move is guarded by the checksum of a cut read, like a delete.

fn long_note() -> String {
    let mut long = String::from("# Long\n\nSummary of a long note about a cache.\n");
    for i in 0..40 {
        long.push_str(&format!("\n## Part {i}\n\n{}\n", "word ".repeat(400)));
    }
    long
}

#[test]
fn a_long_note_moves_with_the_checksum_of_a_cut_read() {
    let long = long_note();
    let v = TestVault::new(&[("ideas/Long.md", &long)]);
    let shown = {
        let mut index = v.index();
        let cut = recognitor::read::render(&index.read("ideas/Long", None, None, LIMIT).unwrap());
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
    // A stale checksum returns the cut note, with the checksum to retry with in its header line.
    let stale = v.mv("ideas/Long", "design/Long", "000000000000");
    assert!(
        stale.starts_with(&format!(
            "The note changed since that checksum, so nothing was moved. Current version:\nideas/Long  checksum: {shown}\n"
        )),
        "{}",
        &stale[..200]
    );
    assert!(v.exists("ideas/Long.md"));
    assert_eq!(
        v.mv("ideas/Long", "design/Long", &shown),
        "moved: ideas/Long -> design/Long\nbroken links: none\n"
    );
    assert_eq!(v.file("design/Long.md"), long);
}

// The shared write path (doc 07 §5.1).

#[test]
fn a_move_leaves_no_temporary_file_and_does_not_touch_the_notes_bytes() {
    let v = vault();
    let crlf = "# Win\r\n\r\nLine one\r\nlast without break";
    v.put("ideas/Win.md", crlf);
    v.mv_ok("ideas/Win", "design/Win");
    assert_eq!(fs::read(v.root.join("design/Win.md")).unwrap(), crlf.as_bytes());
    let mut all = Vec::new();
    fn names(dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            out.push(e.file_name().to_string_lossy().into_owned());
            if e.file_type().unwrap().is_dir() {
                names(&e.path(), out);
            }
        }
    }
    names(&v.root.join("design"), &mut all);
    all.sort();
    assert_eq!(all, ["Win.md", "index.md"]);
}

#[test]
fn a_second_move_of_the_same_note_works_and_links_follow_it_each_time() {
    let v = vault();
    v.mv_ok("ideas/Caching", "design/Caching");
    let text = v.mv_ok("design/Caching", "backend/Caching");
    assert!(text.starts_with("moved: design/Caching -> backend/Caching\n"), "{text}");
    v.assert_index_matches_a_rebuild();
}

/// Doc 07 §11: lines are sorted by linking note (its id, ignoring case), so `ops/Run` comes before `ops/Run book` even
/// though `ops/Run book.md` sorts before `ops/Run.md` as a path.
#[test]
fn linking_notes_are_sorted_by_id_not_by_file_name() {
    let v = TestVault::new(&[
        (
            "ideas/Caching.md",
            "# Caching

A cache.
",
        ),
        (
            "ops/Run.md",
            "# Run

See [[ideas::Caching]].
",
        ),
        (
            "ops/Run book.md",
            "# Run book

See [[ideas::Caching]].
",
        ),
    ]);
    let text = v.mv_ok("ideas/Caching", "design/Caching");
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(
        lines,
        [
            "- ops/Run: links_to ideas::Caching",
            "- ops/Run book: links_to ideas::Caching"
        ]
    );
}

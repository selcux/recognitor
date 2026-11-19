//! `recognitor move` tests (doc 07 §6.1 "CLI", §11, §12). The binary runs with piped stdin: a pipe is not a terminal, so
//! the prompt is driven with `--relink ask`. Vaults live in temp folders that delete themselves.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    base: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-move-cli-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        for (rel, text) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        TestVault { root, base }
    }

    fn file(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    /// The vault as the finish command prints it, when it came from `--vault`.
    fn flag(&self) -> String {
        format!("--vault \"{}\"", self.root.display())
    }

    /// Runs the binary with `stdin` piped, closed after `input`.
    fn run(&self, args: &[&str], input: &str) -> Run {
        let mut child = Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .arg("--vault")
            .arg(&self.root)
            .env("RECOGNITOR_DB", self.base.join("index.db"))
            .env("RECOGNITOR_CONFIG", self.base.join("config.toml"))
            .env_remove("RECOGNITOR_VAULT")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

const CACHING: &str = "# Caching\n\nA cache keeps answers close.\n";
const RUNBOOK: &str = "# Runbook\n\nSee [[ideas::Caching]].\n";
const OTHER: &str = "# Other\n\nSee [[ideas::Caching#Keys|keys]] too.\n";

/// A note with two linking notes, both by a prefix that stops working when the note leaves `ideas`.
fn vault() -> TestVault {
    TestVault::new(&[
        ("ideas/Caching.md", CACHING),
        ("ops/Runbook.md", RUNBOOK),
        ("ops/Other.md", OTHER),
    ])
}

fn block(v: &TestVault) -> String {
    format!(
        "moved: ideas/Caching -> design/Caching\n\
         broken links: 2 links in 2 notes\n\
         - ops/Other: links_to ideas::Caching\n\
         - ops/Runbook: links_to ideas::Caching\n\
         recognitor relink \"ideas/Caching\" \"design/Caching\" \"ops/Other\" \"ops/Runbook\" {}\n",
        v.flag()
    )
}

#[test]
fn all_moves_the_note_and_rewrites_every_linking_note() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "all"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(r.stdout, format!("{}relinked: 2 links in 2 notes\n", block(&v)));
    assert_eq!(r.stderr, "");
    assert!(!v.exists("ideas/Caching.md"));
    assert_eq!(v.file("design/Caching.md"), CACHING);
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[design::Caching]].\n");
    assert_eq!(
        v.file("ops/Other.md"),
        "# Other\n\nSee [[design::Caching#Keys|keys]] too.\n"
    );
}

#[test]
fn none_moves_the_note_and_leaves_the_links_with_a_command_that_finishes_the_job() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "none"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(r.stdout, block(&v));
    assert_eq!(r.stderr, "");
    assert_eq!(v.file("ops/Runbook.md"), RUNBOOK);
    // The printed command does what the prompt would have done.
    let done = v.run(
        &["relink", "ideas/Caching", "design/Caching", "ops/Other", "ops/Runbook"],
        "",
    );
    assert_eq!(done.stdout, "relinked: 2 links in 2 notes\n");
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[design::Caching]].\n");
}

#[test]
fn ask_relinks_the_numbers_the_user_picks() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "2\n");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    // stdout is the Markdown text only; the prompt is on stderr.
    assert!(
        r.stdout.starts_with(&format!(
            "{}relinked: 1 link in 1 note
",
            block(&v)
        )),
        "{}",
        r.stdout
    );
    assert_eq!(
        r.stderr,
        "2 notes have a link to the moved note:\n  1. ops/Other\n  2. ops/Runbook\n\
         Rewrite the links in which? [all / none / numbers, e.g. 1,3] (Enter = none): "
    );
    assert_eq!(v.file("ops/Other.md"), OTHER);
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[design::Caching]].\n");
}

#[test]
fn ask_with_all_and_with_enter() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "all\n");
    assert!(r.stdout.ends_with("relinked: 2 links in 2 notes\n"), "{}", r.stdout);
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "\n");
    assert_eq!(r.code, Some(0));
    assert_eq!(r.stdout, block(&v));
    assert!(
        r.stderr
            .ends_with("No links were rewritten. The relink command above does it later.\n"),
        "{}",
        r.stderr
    );
    assert_eq!(v.file("ops/Runbook.md"), RUNBOOK);
}

#[test]
fn a_bad_answer_is_asked_again() {
    let v = vault();
    let r = v.run(
        &["move", "ideas/Caching", "design/Caching", "--relink", "ask"],
        "7\n1\n",
    );
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert!(
        r.stderr.contains("7 is not in the list, which has 2 notes."),
        "{}",
        r.stderr
    );
    assert!(r.stdout.contains("relinked: 1 link in 1 note\n"), "{}", r.stdout);
    assert_eq!(v.file("ops/Runbook.md"), RUNBOOK);
    assert_eq!(
        v.file("ops/Other.md"),
        "# Other\n\nSee [[design::Caching#Keys|keys]] too.\n"
    );
}

#[test]
fn the_end_of_input_at_the_prompt_leaves_the_move_done_and_the_links_alone() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(r.stdout, block(&v));
    assert!(v.exists("design/Caching.md"));
    assert_eq!(v.file("ops/Runbook.md"), RUNBOOK);
}

#[test]
fn without_a_terminal_and_without_the_flag_the_links_are_left_and_stderr_says_so() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching"], "all\n");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(r.stdout, block(&v));
    assert!(r.stderr.contains("stdin is not a terminal"), "{}", r.stderr);
    // Nothing was read from stdin: the "all" did not relink.
    assert_eq!(v.file("ops/Runbook.md"), RUNBOOK);
}

#[test]
fn no_broken_links_means_no_prompt() {
    let v = TestVault::new(&[("ideas/Caching.md", CACHING)]);
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(r.stdout, "moved: ideas/Caching -> design/Caching\nbroken links: none\n");
    assert_eq!(r.stderr, "");
}

#[test]
fn the_moved_notes_own_links_are_listed_to_fix_by_hand_and_are_no_prompt() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", "# Caching\n\nSee [[./Setup]].\n"),
        ("ideas/Setup.md", "# Setup\n\nHow.\n"),
    ]);
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "moved: ideas/Caching -> design/Caching\n\
         broken links: 1 link in 1 note\n\
         - design/Caching: links_to ./Setup\n\
         edit by hand: 1 link in design/Caching (relink does not touch the moved note's own links)\n"
    );
    assert_eq!(r.stderr, "");
}

#[test]
fn a_refusal_is_an_error_with_the_current_note_and_moves_nothing() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", CACHING),
        ("design/Caching.md", "# Design\n\nAlready here.\n"),
        ("ops/Runbook.md", RUNBOOK),
    ]);
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "all"], "");
    assert_eq!(r.code, Some(1));
    assert_eq!(r.stdout, "");
    assert!(
        r.stderr
            .starts_with("error: A note already exists at that path, so nothing was moved. Current version:\n"),
        "{}",
        r.stderr
    );
    assert!(r.stderr.contains("Already here."), "{}", r.stderr);
    assert!(v.exists("ideas/Caching.md"));
    assert_eq!(v.file("ops/Runbook.md"), RUNBOOK);
}

#[test]
fn the_same_path_is_a_normal_result() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "ideas/caching.md", "--relink", "ask"], "");
    // A new spelling of the same key is a case-only rename: it is a move, not the same-path result.
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "ideas/Caching", "--relink", "ask"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "The note is already at ideas/Caching, so nothing was moved.\n"
    );
    assert_eq!(r.stderr, "");
}

#[test]
fn a_name_that_matches_several_notes_lists_them_and_moves_none() {
    let v = TestVault::new(&[("a/Index.md", "# Index\n"), ("b/Index.md", "# Index\n")]);
    let r = v.run(&["move", "index", "c/Index"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "\"index\" matches 2 notes. Call again with one id:\n  a/Index\n  b/Index\n"
    );
    assert!(v.exists("a/Index.md") && v.exists("b/Index.md"));
}

#[test]
fn a_note_that_is_not_there_is_an_error() {
    let v = vault();
    let r = v.run(&["move", "nothing", "design/Nothing"], "");
    assert_eq!(r.code, Some(1));
    assert_eq!(r.stdout, "");
    assert_eq!(r.stderr, "error: \"nothing\" not found\n");
}

#[test]
fn an_archive_note_is_refused_with_the_text_of_the_doc() {
    let v = TestVault::new(&[("archive/Old.md", "# Old\n")]);
    let r = v.run(&["move", "archive/Old", "ideas/Old"], "");
    assert_eq!(r.code, Some(1));
    assert_eq!(
        r.stderr,
        "error: Archive notes are append only: move_note never moves one. Only the user can move the file.\n"
    );
}

#[test]
fn a_bootstrap_notice_comes_last_in_the_move_block_and_after_a_relink_of_a_bootstrap_note() {
    let v = TestVault::new(&[
        ("bootstrap/index.md", "# Bootstrap\n\n- [[ideas::Caching]]\n"),
        ("ideas/Caching.md", CACHING),
    ]);
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "all"], "");
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert!(r.stdout.contains("relinked: 1 link in 1 note\n"), "{}", r.stdout);
    assert!(
        r.stdout
            .ends_with("notice: bootstrap/index loads as instructions at session start\n"),
        "{}",
        r.stdout
    );
}

#[test]
fn the_usage_is_checked_before_anything_moves() {
    let v = vault();
    for args in [
        &["move"][..],
        &["move", "ideas/Caching"],
        &["move", "ideas/Caching", "design/Caching", "extra"],
        &["move", "ideas/Caching", "design/Caching", "--relink", "maybe"],
        &["move", "ideas/Caching", "design/Caching", "--relink"],
        &["search", "x", "--relink", "all"],
    ] {
        let r = v.run(args, "");
        assert_eq!(r.code, Some(2), "{args:?}: {}", r.stderr);
        assert_eq!(r.stdout, "");
    }
    assert!(v.exists("ideas/Caching.md"));
}

/// Like `run`, but the vault comes from `RECOGNITOR_VAULT`, so no `--vault` is given and none is printed.
fn run_with_env_vault(v: &TestVault, args: &[&str], input: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(args)
        .env("RECOGNITOR_VAULT", &v.root)
        .env("RECOGNITOR_DB", v.base.join("index.db"))
        .env("RECOGNITOR_CONFIG", v.base.join("config.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// The example transcript of doc 07 §12.1, with the answer `all`. stdout and stderr are checked apart: the terminal
/// shows them interleaved, with the typed answer echoed after the prompt.
#[test]
fn the_transcript_of_doc_07_12_1_holds() {
    let v = TestVault::new(&[
        ("ideas/Caching.md", "# Caching\n\nSee [[./Setup]].\n"),
        ("ideas/Setup.md", "# Setup\n\nHow.\n"),
        (
            "ops/Runbook.md",
            "# Runbook v2\n\nSee [[ideas::Caching]].\n\n## Relations\n- relates_to [[../ideas/Caching]]\n",
        ),
    ]);
    let r = run_with_env_vault(
        &v,
        &["move", "ideas/Caching", "design/b/Caching", "--relink", "ask"],
        "all\n",
    );
    assert_eq!(r.code, Some(0), "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "moved: ideas/Caching -> design/b/Caching\n\
         broken links: 3 links in 2 notes\n\
         - design/b/Caching: links_to ./Setup\n\
         - ops/Runbook, titled \"Runbook v2\": links_to ideas::Caching\n\
         - ops/Runbook, titled \"Runbook v2\": relates_to ../ideas/Caching\n\
         recognitor relink \"ideas/Caching\" \"design/b/Caching\" \"ops/Runbook\"\n\
         edit by hand: 1 link in design/b/Caching (relink does not touch the moved note's own links)\n\
         relinked: 2 links in 1 note\n"
    );
    assert_eq!(
        r.stderr,
        "1 note has a link to the moved note:\n  1. ops/Runbook\n\
         Rewrite the links in which? [all / none / numbers, e.g. 1,3] (Enter = none): "
    );
}

#[test]
fn numbers_that_leave_notes_out_print_the_command_again_for_those_notes() {
    let v = vault();
    let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], "2\n");
    assert_eq!(
        r.stdout,
        format!(
            "{}relinked: 1 link in 1 note\nrecognitor relink \"ideas/Caching\" \"design/Caching\" \"ops/Other\" {}\n",
            block(&v),
            v.flag()
        )
    );
    // The line works as printed.
    let rest = v.run(&["relink", "ideas/Caching", "design/Caching", "ops/Other"], "");
    assert_eq!(rest.stdout, "relinked: 1 link in 1 note\n");
    assert_eq!(
        v.file("ops/Other.md"),
        "# Other\n\nSee [[design::Caching#Keys|keys]] too.\n"
    );
}

#[test]
fn the_command_is_not_repeated_for_all_none_or_numbers_that_pick_every_note() {
    for input in ["all\n", "1,2\n", "1-2\n"] {
        let v = vault();
        let r = v.run(&["move", "ideas/Caching", "design/Caching", "--relink", "ask"], input);
        assert_eq!(
            r.stdout,
            format!("{}relinked: 2 links in 2 notes\n", block(&v)),
            "{input:?}"
        );
    }
    let v = vault();
    let r = v.run(
        &["move", "ideas/Caching", "design/Caching", "--relink", "ask"],
        "none\n",
    );
    assert_eq!(r.stdout, block(&v));
}

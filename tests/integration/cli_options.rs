//! An option on the wrong command is a bad argument (doc 07 §12), never ignored: exit 2, the message that names the
//! commands that take it, the usage text on stderr, nothing on stdout.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "recognitor-options-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("vault/design")).unwrap();
    fs::write(dir.join("vault/design/index.md"), "# Index\n\nThe entry point.\n").unwrap();
    Scratch(dir)
}

fn run(s: &Scratch, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(args)
        .arg("--vault")
        .arg(s.0.join("vault"))
        .env("RECOGNITOR_DB", s.0.join("index.db"))
        .env("RECOGNITOR_CONFIG", s.0.join("config.toml"))
        .env_remove("RECOGNITOR_VAULT")
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_option_on_a_command_that_does_not_take_it_is_refused_with_the_names_of_the_commands_that_do() {
    let s = scratch();
    let cases: &[(&[&str], &str)] = &[
        (
            &["read", "design/index", "--page", "2"],
            "--page belongs to search and context",
        ),
        (&["bootstrap", "--page", "2"], "--page belongs to search and context"),
        (&["ghosts", "--page", "2"], "--page belongs to search and context"),
        (&["lint", "--page", "2"], "--page belongs to search and context"),
        (&["sync", "--page", "2"], "--page belongs to search and context"),
        (
            &["read", "design/index", "--report"],
            "--report belongs to sync and rebuild",
        ),
        (&["search", "entry", "--report"], "--report belongs to sync and rebuild"),
        (
            &["context", "design/index", "--report"],
            "--report belongs to sync and rebuild",
        ),
        (&["lint", "--report"], "--report belongs to sync and rebuild"),
        (&["ghosts", "--report"], "--report belongs to sync and rebuild"),
        (&["bootstrap", "--report"], "--report belongs to sync and rebuild"),
        (&["mcp", "--report"], "--report belongs to sync and rebuild"),
        (&["context", "design/index", "--ns", "design"], "--ns belongs to search"),
        (
            &["search", "entry", "--section", "x"],
            "--section and --query belong to read",
        ),
        (
            &["search", "entry", "--query", "x"],
            "--section and --query belong to read",
        ),
        (&["sync", "--relink", "all"], "--relink belongs to move"),
        (&["read", "design/index", "--list"], "--list belongs to bootstrap"),
        (&["sync", "--strict"], "--strict belongs to lint"),
    ];
    for (args, message) in cases {
        let (code, stdout, stderr) = run(&s, args);
        assert_eq!(code, Some(2), "{args:?}: {stderr}");
        assert!(stderr.starts_with(&format!("error: {message}\n")), "{args:?}: {stderr}");
        assert!(stderr.contains("usage: recognitor"), "{args:?}: the usage follows");
        assert_eq!(stdout, "", "{args:?}");
    }
}

#[test]
fn the_commands_that_take_an_option_still_run_with_it() {
    let s = scratch();
    for args in [
        &["sync", "--report"][..],
        &["rebuild", "--report"],
        &["search", "entry", "--page", "1"],
        &["context", "design/index", "--page", "1"],
    ] {
        let (code, _, stderr) = run(&s, args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
    }
}

#[test]
fn a_bad_page_number_is_still_named_before_the_command_is_checked() {
    let s = scratch();
    let (code, _, stderr) = run(&s, &["read", "design/index", "--page", "0"]);
    assert_eq!(code, Some(2));
    assert!(stderr.starts_with("error: --page needs a number from 1\n"), "{stderr}");
}

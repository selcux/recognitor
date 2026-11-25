//! `recognitor bootstrap remove` (doc 04 §5.3, doc 07 §12.7). Hermetic via `RECOGNITOR_CONFIG` and absolute harness
//! directories in a temp folder. No vault or database is set: the command must not need either.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::harness::render_block;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch {
    base: PathBuf,
    config: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-bootstrap-remove-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let s = Scratch {
            config: base.join("config.toml"),
            base,
        };
        fs::write(&s.config, "").unwrap();
        s
    }

    fn harness_dir(&self, name: &str) -> PathBuf {
        self.base.join("harnesses").join(name)
    }

    /// The path as the config spells it: the temp folder with `/` separators, then the file.
    fn installed_path(&self, dir: &Path, file: &str) -> PathBuf {
        PathBuf::from(dir.display().to_string().replace('\\', "/")).join(file)
    }

    /// One harness table. `files` omitted means the command's default, `AGENTS.md`.
    fn table(name: &str, dir: &Path, files: Option<&[&str]>) -> String {
        let dir = dir.display().to_string().replace('\\', "/");
        let files_line = match files {
            None => String::new(),
            Some([]) => "files = []\n".to_string(),
            Some(list) => {
                let listed: Vec<String> = list.iter().map(|f| format!("\"{f}\"")).collect();
                format!("files = [{}]\n", listed.join(", "))
            }
        };
        format!("[harness.{name}]\ndirectory = \"{dir}\"\n{files_line}")
    }

    fn install(&self, name: &str, dir: &Path, files: Option<&[&str]>) {
        fs::write(&self.config, Self::table(name, dir, files)).unwrap();
    }

    fn install_all(&self, harnesses: &[(&str, &Path, Option<&[&str]>)]) {
        let text: String = harnesses
            .iter()
            .map(|(name, dir, files)| Self::table(name, dir, *files))
            .collect();
        fs::write(&self.config, text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .env("RECOGNITOR_CONFIG", &self.config)
            .env_remove("RECOGNITOR_VAULT")
            .env_remove("RECOGNITOR_DB")
            .output()
            .unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn remove_strips_the_block_and_reports_the_harness_and_path() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    let content = [
        b"before\n".as_slice(),
        &render_block(b"payload\n"),
        b"after\n".as_slice(),
    ]
    .concat();
    fs::write(&file, &content).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("Removed:\ntool: {}\n", file.display()));
    assert_eq!(stderr(&out), "");
    assert_eq!(fs::read(&file).unwrap(), b"before\nafter\n");
}

#[test]
fn remove_leaves_a_file_without_a_block_alone() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    fs::write(&file, "mine\n").unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert_eq!(fs::read(&file).unwrap(), b"mine\n");
}

#[test]
fn remove_deletes_an_out_of_date_block() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    fs::write(&file, render_block(b"old payload\n")).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("Removed:\ntool: {}\n", file.display()));
    assert_eq!(fs::read(&file).unwrap(), b"");
}

#[test]
fn a_second_remove_changes_nothing() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    fs::write(&file, render_block(b"payload\n")).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(fs::read(&file).unwrap(), b"");
}

#[test]
fn a_missing_file_stops_before_any_edit() {
    let s = Scratch::new();
    let good = s.harness_dir("good");
    let missing = s.harness_dir("missing");
    fs::create_dir_all(&good).unwrap();
    fs::create_dir_all(&missing).unwrap();
    let good_file = s.installed_path(&good, "AGENTS.md");
    let before = [b"mine\n".as_slice(), &render_block(b"payload\n")].concat();
    fs::write(&good_file, &before).unwrap();
    s.install_all(&[
        ("good", &good, Some(&["AGENTS.md"])),
        ("missing", &missing, Some(&["AGENTS.md"])),
    ]);

    let out = s.run(&["bootstrap", "remove", "good", "missing"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("instruction file is missing"), "{}", stderr(&out));
    assert_eq!(fs::read(&good_file).unwrap(), before);
}

#[test]
fn a_one_marker_file_stops_before_any_edit() {
    let s = Scratch::new();
    let good = s.harness_dir("good");
    let broken = s.harness_dir("broken");
    fs::create_dir_all(&good).unwrap();
    fs::create_dir_all(&broken).unwrap();
    let good_file = s.installed_path(&good, "AGENTS.md");
    let before = [b"mine\n".as_slice(), &render_block(b"payload\n")].concat();
    fs::write(&good_file, &before).unwrap();
    fs::write(broken.join("AGENTS.md"), "<!-- recognitor bootstrap end -->\n").unwrap();
    s.install_all(&[
        ("good", &good, Some(&["AGENTS.md"])),
        ("broken", &broken, Some(&["AGENTS.md"])),
    ]);

    let out = s.run(&["bootstrap", "remove", "good", "broken"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("misconfigured"), "{}", stderr(&out));
    assert_eq!(fs::read(&good_file).unwrap(), before);
}

#[test]
fn an_unknown_harness_is_a_bad_argument() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("no harness named \"nope\""), "{}", stderr(&out));
    assert!(stderr(&out).contains("tool"), "{}", stderr(&out));
}

#[test]
fn remove_with_no_installed_harness_needs_no_vault() {
    let s = Scratch::new();
    let absent = s.base.join("not-installed");
    s.install("tool", &absent, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert!(!absent.exists());
}

#[test]
fn bootstrap_remove_needs_a_harness_name() {
    let s = Scratch::new();
    let out = s.run(&["bootstrap", "remove"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("needs at least one harness"), "{}", stderr(&out));
}

#[test]
fn list_is_rejected_on_remove() {
    let s = Scratch::new();
    let out = s.run(&["bootstrap", "remove", "--list", "tool"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--list belongs to bootstrap"), "{}", stderr(&out));
}

#[test]
fn multiple_harnesses_are_removed_in_the_order_named_and_duplicates_once() {
    let s = Scratch::new();
    let claude = s.harness_dir("claude");
    let codex = s.harness_dir("codex");
    fs::create_dir_all(&claude).unwrap();
    fs::create_dir_all(&codex).unwrap();
    let claude_file = s.installed_path(&claude, "CLAUDE.md");
    let codex_file = s.installed_path(&codex, "AGENTS.md");
    fs::write(&claude_file, render_block(b"claude\n")).unwrap();
    fs::write(&codex_file, render_block(b"codex\n")).unwrap();
    s.install_all(&[("claude", &claude, Some(&["CLAUDE.md"])), ("codex", &codex, None)]);

    let out = s.run(&["bootstrap", "remove", "codex", "claude", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "Removed:\ncodex: {}\nclaude: {}\n",
            codex_file.display(),
            claude_file.display()
        )
    );
    assert_eq!(fs::read(&claude_file).unwrap(), b"");
    assert_eq!(fs::read(&codex_file).unwrap(), b"");
}

#[test]
fn a_harness_with_two_files_reports_both_lines() {
    let s = Scratch::new();
    let dir = s.harness_dir("pi");
    fs::create_dir_all(&dir).unwrap();
    let agents = s.installed_path(&dir, "AGENTS.md");
    let extra = s.installed_path(&dir, "extra.md");
    fs::write(&agents, render_block(b"agents\n")).unwrap();
    fs::write(&extra, render_block(b"extra\n")).unwrap();
    s.install("pi", &dir, Some(&["AGENTS.md", "extra.md"]));

    let out = s.run(&["bootstrap", "remove", "pi"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("Removed:\npi: {}\npi: {}\n", agents.display(), extra.display())
    );
    assert_eq!(fs::read(&agents).unwrap(), b"");
    assert_eq!(fs::read(&extra).unwrap(), b"");
}

#[test]
fn a_harness_directory_that_is_a_file_is_an_error() {
    let s = Scratch::new();
    let path = s.base.join("not-a-dir");
    fs::write(&path, "file").unwrap();
    s.install("tool", &path, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("not a directory"), "{}", stderr(&out));
}

#[test]
fn an_instruction_path_that_is_a_directory_is_an_error() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(s.installed_path(&dir, "AGENTS.md")).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("is a directory"), "{}", stderr(&out));
}

#[test]
fn a_crlf_file_keeps_its_line_endings_around_the_removed_block() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    let content = [
        b"before\r\n".as_slice(),
        &render_block(b"payload\n"),
        b"after\r\n".as_slice(),
    ]
    .concat();
    fs::write(&file, &content).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fs::read(&file).unwrap(), b"before\r\nafter\r\n");
}

#[test]
fn a_block_at_the_end_without_a_final_newline_is_removed() {
    let s = Scratch::new();
    let dir = s.harness_dir("tool");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    let mut block = render_block(b"payload\n");
    block.pop();
    let content = [b"before\n".as_slice(), &block].concat();
    fs::write(&file, &content).unwrap();
    s.install("tool", &dir, None);

    let out = s.run(&["bootstrap", "remove", "tool"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(fs::read(&file).unwrap(), b"before\n");
}

//! `recognitor bootstrap write` (doc 04 §5.3, doc 07 §12.6). Hermetic via `RECOGNITOR_CONFIG` and absolute harness
//! directories in a temp folder, so the real home profile is never touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::harness::render_block;
use recognitor::{Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch {
    base: PathBuf,
    vault: PathBuf,
    db: PathBuf,
    config: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-bootstrap-write-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let vault = base.join("vault");
        fs::create_dir_all(vault.join("bootstrap")).unwrap();
        fs::write(
            vault.join("bootstrap/index.md"),
            "# Bootstrap\n\nStanding instructions.\n",
        )
        .unwrap();
        let s = Scratch {
            base: base.clone(),
            vault,
            db: base.join("index.db"),
            config: base.join("config.toml"),
        };
        // No harness table: a test that needs one writes it with `install` or `install_all`.
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

    fn payload(&self) -> String {
        let mut index = Index::open(&self.vault, Some(&self.db)).unwrap();
        index.rebuild(SweepOptions::default()).unwrap();
        index
            .bootstrap()
            .unwrap()
            .render(recognitor::bootstrap::DEFAULT_MAX_TOKENS)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .arg("--vault")
            .arg(&self.vault)
            .env("RECOGNITOR_DB", &self.db)
            .env("RECOGNITOR_CONFIG", &self.config)
            .env_remove("RECOGNITOR_VAULT")
            .output()
            .unwrap()
    }

    fn run_without_vault(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .env("RECOGNITOR_DB", &self.db)
            .env("RECOGNITOR_CONFIG", &self.config)
            .env_remove("RECOGNITOR_VAULT")
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
fn write_appends_the_block_and_reports_the_file() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "CLAUDE.md");
    fs::write(&file, "# Claude\n\nMine.\n").unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("Wrote:\nclaude: {}\n", file.display()));
    assert_eq!(stderr(&out), "");

    let payload = s.payload();
    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(
        text,
        format!(
            "# Claude\n\nMine.\n{}",
            String::from_utf8(render_block(payload.as_bytes())).unwrap()
        )
    );

    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "claude +\n");
}

#[test]
fn write_fills_an_empty_instruction_file() {
    let s = Scratch::new();
    let dir = s.harness_dir("codex");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "AGENTS.md");
    fs::write(&file, "").unwrap();
    s.install("codex", &dir, None);

    let out = s.run(&["bootstrap", "write", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("Wrote:\ncodex: {}\n", file.display()));

    let payload = s.payload();
    assert_eq!(fs::read(&file).unwrap(), render_block(payload.as_bytes()));
}

#[test]
fn write_updates_every_file_of_one_harness() {
    let s = Scratch::new();
    let dir = s.harness_dir("pi");
    fs::create_dir_all(&dir).unwrap();
    let agents = s.installed_path(&dir, "AGENTS.md");
    let extra = s.installed_path(&dir, "extra.md");
    fs::write(&agents, "agents\n").unwrap();
    fs::write(&extra, "extra\n").unwrap();
    s.install("pi", &dir, Some(&["AGENTS.md", "extra.md"]));

    let out = s.run(&["bootstrap", "write", "pi"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("Wrote:\npi: {}\npi: {}\n", agents.display(), extra.display())
    );

    let payload = s.payload();
    let block = render_block(payload.as_bytes());
    assert_eq!(fs::read(&agents).unwrap(), [b"agents\n".as_slice(), &block].concat());
    assert_eq!(fs::read(&extra).unwrap(), [b"extra\n".as_slice(), &block].concat());
}

#[test]
fn a_second_write_changes_nothing() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "CLAUDE.md");
    fs::write(&file, "# Claude\n").unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let first = fs::read(&file).unwrap();

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(fs::read(&file).unwrap(), first);
}

#[test]
fn write_replaces_an_out_of_date_block_and_keeps_the_rest() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "CLAUDE.md");
    let old = [
        b"before\n\n".as_slice(),
        &render_block(b"old payload\n"),
        b"\nafter\n".as_slice(),
    ]
    .concat();
    fs::write(&file, old).unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("Wrote:\nclaude: {}\n", file.display()));

    let payload = s.payload();
    let expected = [
        b"before\n\n".as_slice(),
        &render_block(payload.as_bytes()),
        b"\nafter\n".as_slice(),
    ]
    .concat();
    assert_eq!(fs::read(&file).unwrap(), expected);
}

#[test]
fn write_leaves_an_up_to_date_block_alone() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "CLAUDE.md");
    let payload = s.payload();
    let text = [b"# Claude\n".as_slice(), &render_block(payload.as_bytes())].concat();
    fs::write(&file, &text).unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(fs::read(&file).unwrap(), text);
}

#[test]
fn a_missing_file_stops_before_any_edit() {
    let s = Scratch::new();
    let good = s.harness_dir("good");
    let missing = s.harness_dir("missing");
    fs::create_dir_all(&good).unwrap();
    fs::create_dir_all(&missing).unwrap();
    let good_file = s.installed_path(&good, "AGENTS.md");
    fs::write(&good_file, "mine\n").unwrap();
    let before = fs::read(&good_file).unwrap();
    s.install_all(&[
        ("good", &good, Some(&["AGENTS.md"])),
        ("missing", &missing, Some(&["AGENTS.md"])),
    ]);

    let out = s.run(&["bootstrap", "write", "good", "missing"]);
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
    fs::write(&good_file, "mine\n").unwrap();
    let before = fs::read(&good_file).unwrap();
    fs::write(broken.join("AGENTS.md"), "<!-- recognitor bootstrap end -->\n").unwrap();
    s.install_all(&[
        ("good", &good, Some(&["AGENTS.md"])),
        ("broken", &broken, Some(&["AGENTS.md"])),
    ]);

    let out = s.run(&["bootstrap", "write", "good", "broken"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("misconfigured"), "{}", stderr(&out));
    assert_eq!(fs::read(&good_file).unwrap(), before);
}

#[test]
fn an_unknown_harness_is_a_bad_argument() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("no harness named \"nope\""), "{}", stderr(&out));
    assert!(stderr(&out).contains("claude"), "{}", stderr(&out));
}

#[test]
fn write_with_no_installed_harness_needs_no_vault() {
    let s = Scratch::new();
    let absent = s.base.join("not-installed");
    s.install("claude", &absent, Some(&["CLAUDE.md"]));

    let out = s.run_without_vault(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert!(!absent.exists());
}

#[test]
fn a_harness_directory_that_is_a_file_is_an_error() {
    let s = Scratch::new();
    let path = s.base.join("not-a-dir");
    fs::write(&path, "file").unwrap();
    s.install("claude", &path, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("not a directory"), "{}", stderr(&out));
}

#[test]
fn an_instruction_path_that_is_a_directory_is_an_error() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(s.installed_path(&dir, "CLAUDE.md")).unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("is a directory"), "{}", stderr(&out));
}

#[test]
fn a_bad_harness_table_stops_before_any_edit_and_needs_no_vault() {
    let s = Scratch::new();
    fs::write(&s.config, "[harness.claude]\ndirectory = \".claude\"\n").unwrap();

    let out = s.run_without_vault(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("relative"), "{}", stderr(&out));
}

#[test]
fn bootstrap_write_needs_a_harness_name() {
    let s = Scratch::new();
    let out = s.run(&["bootstrap", "write"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("needs at least one harness"), "{}", stderr(&out));
}

#[test]
fn list_is_rejected_on_write() {
    let s = Scratch::new();
    let out = s.run(&["bootstrap", "write", "--list", "claude"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--list belongs to bootstrap"), "{}", stderr(&out));
}

#[test]
fn write_refuses_a_payload_with_a_marker_line() {
    let s = Scratch::new();
    fs::write(
        s.vault.join("bootstrap/index.md"),
        "# Bootstrap\n\n<!-- recognitor bootstrap end -->\n",
    )
    .unwrap();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "CLAUDE.md");
    fs::write(&file, "mine\n").unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("marker line"), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine\n");
}

#[test]
fn multiple_harnesses_are_written_in_the_order_named_and_duplicates_once() {
    let s = Scratch::new();
    let claude = s.harness_dir("claude");
    let codex = s.harness_dir("codex");
    fs::create_dir_all(&claude).unwrap();
    fs::create_dir_all(&codex).unwrap();
    let claude_file = s.installed_path(&claude, "CLAUDE.md");
    let codex_file = s.installed_path(&codex, "AGENTS.md");
    fs::write(&claude_file, "claude\n").unwrap();
    fs::write(&codex_file, "codex\n").unwrap();
    s.install_all(&[("claude", &claude, Some(&["CLAUDE.md"])), ("codex", &codex, None)]);

    let out = s.run(&["bootstrap", "write", "codex", "claude", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "Wrote:\ncodex: {}\nclaude: {}\n",
            codex_file.display(),
            claude_file.display()
        )
    );
}

#[test]
fn a_crlf_block_is_replaced() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let file = s.installed_path(&dir, "CLAUDE.md");
    let old = "before\r\n<!-- recognitor bootstrap sha=".to_string()
        + &"0".repeat(64)
        + " -->\r\nold\r\n<!-- recognitor bootstrap end -->\r\nafter\r\n";
    fs::write(&file, old).unwrap();
    s.install("claude", &dir, Some(&["CLAUDE.md"]));

    let out = s.run(&["bootstrap", "write", "claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let payload = s.payload();
    let expected = [
        b"before\r\n".as_slice(),
        &render_block(payload.as_bytes()),
        b"after\r\n".as_slice(),
    ]
    .concat();
    assert_eq!(fs::read(&file).unwrap(), expected);
}

#[test]
fn doctor_suggests_the_write_command_with_space_separated_names() {
    let s = Scratch::new();
    let claude = s.harness_dir("claude");
    let codex = s.harness_dir("codex");
    fs::create_dir_all(&claude).unwrap();
    fs::create_dir_all(&codex).unwrap();
    fs::write(s.installed_path(&claude, "CLAUDE.md"), "claude\n").unwrap();
    fs::write(s.installed_path(&codex, "AGENTS.md"), "codex\n").unwrap();
    s.install_all(&[("claude", &claude, Some(&["CLAUDE.md"])), ("codex", &codex, None)]);

    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "claude -\ncodex -\n---\nrecognitor bootstrap write claude codex\n"
    );

    // The suggestion runs as printed.
    let out = s.run(&["bootstrap", "write", "claude", "codex"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

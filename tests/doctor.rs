//! `recognitor doctor` (doc 04 §5.3, doc 07 §12.5). Hermetic via `RECOGNITOR_CONFIG` and absolute harness
//! directories in a temp folder, so the real home profile is never touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::hash::sha256_hex;
use recognitor::{Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch {
    base: PathBuf,
    vault: PathBuf,
    db: PathBuf,
    config: PathBuf,
    home: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-doctor-{}-{n}", std::process::id()));
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
            home: base.join("home"),
        };
        fs::create_dir_all(&s.home).unwrap();
        // Point every built-in at an absent directory under the scratch home, so the real profile is never read.
        s.write_absent_builtins();
        s
    }

    fn write_absent_builtins(&self) {
        let toml = format!(
            r#"[harness.claude]
directory = "{0}/.claude"

[harness.antigravity]
directory = "{0}/.gemini"

[harness.codex]
directory = "{0}/.codex"

[harness.opencode]
directory = "{0}/.config/opencode"
"#,
            self.home.display().to_string().replace('\\', "/")
        );
        fs::write(&self.config, toml).unwrap();
    }

    fn harness_dir(&self, name: &str) -> PathBuf {
        self.base.join("harnesses").join(name)
    }

    /// Only one installed harness: overlays a built-in onto `dir`, others stay absent under the scratch home.
    fn install_builtin(&self, name: &str, dir: &Path, files: Option<&[&str]>) {
        let dir_s = dir.display().to_string().replace('\\', "/");
        let files_line = match files {
            None => String::new(),
            Some([]) => "files = []\n".to_string(),
            Some(list) => {
                let listed: Vec<String> = list.iter().map(|f| format!("\"{f}\"")).collect();
                format!("files = [{}]\n", listed.join(", "))
            }
        };
        let home = self.home.display().to_string().replace('\\', "/");
        let mut parts = String::new();
        for builtin in ["claude", "antigravity", "codex", "opencode"] {
            let d = if builtin == name {
                dir_s.clone()
            } else {
                let rest = match builtin {
                    "claude" => ".claude",
                    "antigravity" => ".gemini",
                    "codex" => ".codex",
                    "opencode" => ".config/opencode",
                    _ => unreachable!(),
                };
                format!("{home}/{rest}")
            };
            parts.push_str(&format!("[harness.{builtin}]\ndirectory = \"{d}\"\n"));
            if builtin == name {
                parts.push_str(&files_line);
            }
            parts.push('\n');
        }
        fs::write(&self.config, parts).unwrap();
    }

    fn ensure_index(&self) {
        let mut index = Index::open(&self.vault, Some(&self.db)).unwrap();
        index.rebuild(SweepOptions::default()).unwrap();
    }

    fn payload(&self) -> String {
        self.ensure_index();
        let mut index = Index::open(&self.vault, Some(&self.db)).unwrap();
        let boot = index.bootstrap().unwrap();
        boot.render(recognitor::bootstrap::DEFAULT_MAX_TOKENS)
    }

    fn write_block(&self, path: &Path, payload: &str, stamp: Option<&str>) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let hash = stamp.unwrap_or(&sha256_hex(payload.as_bytes())).to_string();
        let text = format!("<!-- recognitor bootstrap sha={hash} -->\n{payload}<!-- recognitor bootstrap end -->\n");
        fs::write(path, text).unwrap();
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
fn all_harnesses_absent_prints_nothing_and_exits_0() {
    let s = Scratch::new();
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert!(!s.home.join(".claude").exists());
}

#[test]
fn block_absent_suggests_write_and_exits_1() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("CLAUDE.md"), "# Claude\n").unwrap();
    s.install_builtin("claude", &dir, Some(&["CLAUDE.md"]));
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "claude -\n---\nrecognitor bootstrap write claude\n"
    );
    assert_eq!(stderr(&out), "");
}

#[test]
fn up_to_date_exits_0() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let payload = s.payload();
    s.write_block(&dir.join("CLAUDE.md"), &payload, None);
    s.install_builtin("claude", &dir, Some(&["CLAUDE.md"]));
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "claude +\n");
}

#[test]
fn out_of_date_from_wrong_stamp_suggests_write() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let payload = s.payload();
    let wrong = "0".repeat(64);
    s.write_block(&dir.join("CLAUDE.md"), &payload, Some(&wrong));
    s.install_builtin("claude", &dir, Some(&["CLAUDE.md"]));
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "claude \u{2191}\n---\nrecognitor bootstrap write claude\n"
    );
}

#[test]
fn crlf_body_is_out_of_date_against_lf_payload() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(&dir).unwrap();
    let payload = s.payload();
    let crlf = payload.replace('\n', "\r\n");
    let hash = sha256_hex(crlf.as_bytes());
    let file = format!("<!-- recognitor bootstrap sha={hash} -->\r\n{crlf}<!-- recognitor bootstrap end -->\r\n");
    fs::write(dir.join("CLAUDE.md"), file).unwrap();
    s.install_builtin("claude", &dir, Some(&["CLAUDE.md"]));
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stdout(&out).starts_with("claude \u{2191}\n"), "{}", stdout(&out));
}

#[test]
fn misconfigured_and_missing_get_no_suggestion_and_do_not_stop_the_rest() {
    let s = Scratch::new();
    let a = s.harness_dir("a");
    let b = s.harness_dir("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("AGENTS.md"), "<!-- recognitor bootstrap end -->\n").unwrap();
    // b has the directory but no AGENTS.md
    let toml = format!(
        r#"[harness.claude]
directory = "{home}/.claude"
[harness.antigravity]
directory = "{home}/.gemini"
[harness.codex]
directory = "{home}/.codex"
[harness.opencode]
directory = "{home}/.config/opencode"
[harness.tool-a]
directory = "{a}"
[harness.tool-b]
directory = "{b}"
"#,
        home = s.home.display().to_string().replace('\\', "/"),
        a = a.display().to_string().replace('\\', "/"),
        b = b.display().to_string().replace('\\', "/"),
    );
    fs::write(&s.config, toml).unwrap();
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "tool-a ?\ntool-b \u{00d7}\n");
}

#[test]
fn directory_that_is_a_file_exits_1_with_empty_stdout() {
    let s = Scratch::new();
    let path = s.base.join("not-a-dir");
    fs::write(&path, "file").unwrap();
    s.install_builtin("claude", &path, Some(&["CLAUDE.md"]));
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("not a directory"), "{}", stderr(&out));
}

#[test]
fn instruction_path_that_is_a_directory_exits_1_with_empty_stdout() {
    let s = Scratch::new();
    let dir = s.harness_dir("claude");
    fs::create_dir_all(dir.join("CLAUDE.md")).unwrap();
    s.install_builtin("claude", &dir, Some(&["CLAUDE.md"]));
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("directory") || stderr(&out).contains("not a file"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn bad_table_exits_1_before_any_harness_file_is_created() {
    let s = Scratch::new();
    fs::write(&s.config, "[harness.claude]\ndirectory = \".claude\"\n").unwrap();
    let marker = s.base.join("should-not-exist");
    assert!(!marker.exists());
    let out = s.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert!(stderr(&out).contains("relative"), "{}", stderr(&out));
    assert!(!marker.exists());
    assert!(!s.home.join(".claude").exists());
}

#[test]
fn extra_arguments_exit_2() {
    let s = Scratch::new();
    let out = s.run(&["doctor", "claude"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("unexpected argument"), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

#[test]
fn config_get_and_set_ignore_harness_tables() {
    let s = Scratch::new();
    let before = fs::read_to_string(&s.config).unwrap();
    assert!(before.contains("[harness.claude]"));

    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["config", "set", "vault.path"])
        .arg(&s.vault)
        .env("RECOGNITOR_CONFIG", &s.config)
        .env_remove("RECOGNITOR_VAULT")
        .env("RECOGNITOR_DB", &s.db)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let after = fs::read_to_string(&s.config).unwrap();
    assert!(after.contains("[harness.claude]"), "{after}");
    assert!(after.contains("path"), "{after}");

    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["config", "get", "harness.claude.directory"])
        .env("RECOGNITOR_CONFIG", &s.config)
        .env_remove("RECOGNITOR_VAULT")
        .env("RECOGNITOR_DB", &s.db)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("unknown config key"), "{}", stderr(&out));
}

#[test]
fn no_vault_is_usage_error_even_when_harnesses_are_absent() {
    let s = Scratch::new();
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["doctor"])
        .env("RECOGNITOR_CONFIG", &s.config)
        .env_remove("RECOGNITOR_VAULT")
        .env("RECOGNITOR_DB", &s.db)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--vault"), "{}", stderr(&out));
}

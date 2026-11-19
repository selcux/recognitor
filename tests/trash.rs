//! `recognitor trash empty` (doc 07 §10, §12.4). Removes `.recognitor/trash` and does not open the index.
//! Vaults are synthetic and live in temp folders that delete themselves.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Scratch {
    root: PathBuf,
    base: PathBuf,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Scratch {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-trash-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        Scratch { root, base }
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .arg("--vault")
            .arg(&self.root)
            .env("RECOGNITOR_DB", self.base.join("index.db"))
            .env("RECOGNITOR_CONFIG", self.base.join("config.toml"))
            .env_remove("RECOGNITOR_VAULT")
            .output()
            .unwrap();
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn empty(&self) -> Run {
        self.run(&["trash", "empty"])
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn assert_cleared(run: &Run) {
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "");
}

#[test]
fn empty_removes_timestamp_folders_and_leaves_the_rest_of_the_vault() {
    let s = Scratch::new();
    s.write("design/Keep.md", "# Keep\n");
    s.write(".recognitor/schemas.toml", "types = []\n");
    s.write(".recognitor/trash/2026-11-18T19-18-52Z/ideas/Caching.md", "# Caching\n");
    s.write(
        ".recognitor/trash/2026-11-18T19-18-52Z-2/ideas/Caching.md",
        "# Caching again\n",
    );
    s.write(".recognitor/trash/dropped.txt", "not a note\n");
    let run = s.empty();
    assert_cleared(&run);
    assert!(s.root.join("design/Keep.md").is_file());
    assert_eq!(fs::read_to_string(s.root.join("design/Keep.md")).unwrap(), "# Keep\n");
    assert_eq!(
        fs::read_to_string(s.root.join(".recognitor/schemas.toml")).unwrap(),
        "types = []\n"
    );
    assert!(!s.root.join(".recognitor/trash").exists());
    assert!(s.root.join(".recognitor").is_dir());
    assert!(
        !s.base.join("index.db").exists(),
        "trash empty does not open the database"
    );
}

#[test]
fn a_missing_or_empty_trash_directory_is_already_clear() {
    let s = Scratch::new();
    s.write("design/Keep.md", "# Keep\n");
    assert_cleared(&s.empty());
    assert!(s.root.join("design/Keep.md").is_file());
    assert!(!s.root.join(".recognitor").exists());

    fs::create_dir_all(s.root.join(".recognitor/trash")).unwrap();
    assert_cleared(&s.empty());
    assert!(!s.root.join(".recognitor/trash").exists());
    assert!(s.root.join(".recognitor").is_dir());
    assert!(s.root.join("design/Keep.md").is_file());
}

#[test]
fn bad_arguments_exit_2_with_the_usage_and_delete_nothing() {
    let s = Scratch::new();
    s.write(".recognitor/trash/2026-11-18T19-18-52Z/ideas/Caching.md", "# Caching\n");
    let cases: &[(&[&str], &str)] = &[
        (&["trash"], "usage: trash empty"),
        (&["trash", "other"], "usage: trash empty"),
        (&["trash", "empty", "extra"], "unexpected argument: extra"),
        (&["trash", "empty", "--ns", "design"], "--ns belongs to search"),
        (&["trash", "empty", "--strict"], "--strict belongs to lint"),
    ];
    for (args, message) in cases {
        let run = s.run(args);
        assert_eq!(run.code, Some(2), "{args:?}: {}", run.stderr);
        assert!(
            run.stderr.starts_with(&format!("error: {message}\n")),
            "{args:?}: {}",
            run.stderr
        );
        assert!(
            run.stderr.contains("recognitor trash empty"),
            "{args:?}: the usage lists the command"
        );
        assert_eq!(run.stdout, "");
        assert!(
            s.root
                .join(".recognitor/trash/2026-11-18T19-18-52Z/ideas/Caching.md")
                .is_file(),
            "{args:?} deleted the trash"
        );
    }
}

#[test]
fn a_junction_inside_the_trash_is_not_followed() {
    let s = Scratch::new();
    let outside = s.base.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "keep\n").unwrap();
    s.write("design/Keep.md", "# Keep\n");
    s.write(".recognitor/schemas.toml", "types = []\n");
    let link = s.root.join(".recognitor/trash/2026-11-18T19-18-52Z/linked");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    junction(&link, &outside).unwrap();
    let file_link = s.root.join(".recognitor/trash/2026-11-18T19-18-52Z/file-link");
    let file_symlink = symlink_file(&outside.join("secret.txt"), &file_link);

    let run = s.empty();
    assert_cleared(&run);
    assert_eq!(fs::read_to_string(outside.join("secret.txt")).unwrap(), "keep\n");
    assert!(outside.is_dir());
    assert!(s.root.join("design/Keep.md").is_file());
    assert!(s.root.join(".recognitor/schemas.toml").is_file());
    assert!(!s.root.join(".recognitor/trash").exists());
    if !file_symlink {
        eprintln!("skipped a file symlink inside the trash: creating it needs elevation on this machine");
    }
}

#[test]
fn a_junction_in_place_of_the_trash_directory_is_not_followed() {
    let s = Scratch::new();
    let outside = s.base.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "keep\n").unwrap();
    s.write("design/Keep.md", "# Keep\n");
    fs::create_dir_all(s.root.join(".recognitor")).unwrap();
    let trash = s.root.join(".recognitor/trash");
    junction(&trash, &outside).unwrap();

    let run = s.empty();
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .contains("refusing to follow a link out of .recognitor/trash"),
        "{}",
        run.stderr
    );
    assert_eq!(fs::read_to_string(outside.join("secret.txt")).unwrap(), "keep\n");
    assert!(trash.symlink_metadata().is_ok(), "the junction was deleted");
    assert!(s.root.join("design/Keep.md").is_file());
}

/// `New-Item -ItemType Junction` creates a directory junction without the symlink privilege.
/// `cmd /C mklink` re-parses quotes and treats a path ending near `\trash` as a switch, so this does not use it.
fn junction(link: &Path, target: &Path) -> std::io::Result<()> {
    let script = format!(
        "New-Item -ItemType Junction -Path '{}' -Value '{}' | Out-Null",
        link.display().to_string().replace('\'', "''"),
        target.display().to_string().replace('\'', "''"),
    );
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "New-Item Junction failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )))
    }
}

/// A file symlink needs developer mode or an elevated process. Returns false when the OS refuses.
fn symlink_file(original: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(original, link).is_ok()
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(original, link).is_ok()
    }
}

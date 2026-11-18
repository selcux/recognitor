//! The file half of the write path (doc 07 §5.1 step 4): new bytes go to a temporary file next to the note, which is
//! then renamed over it. A reader sees the old file or the new one, never half of one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// The temporary file next to `path`. It is hidden (a leading `.`) and does not end in `.md`, so neither the sweep nor
/// an editor picks it up.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.recognitor-tmp"))
}

/// How often a rename is tried again when Windows answers "access denied", and how long to wait between tries.
const RENAME_TRIES: u32 = 5;
const RENAME_WAIT: Duration = Duration::from_millis(25);

/// Replaces the file at `path` with `bytes`. The temporary file is removed when the rename fails.
///
/// On Windows a virus scanner or an indexer can hold a file it has just seen open for a few milliseconds, and the
/// rename over it is then denied. That passes, so a denied rename is tried again a few times before it counts as a
/// failure.
pub(crate) fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    fs::write(&tmp, bytes)?;
    let mut tries = 1;
    loop {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && tries < RENAME_TRIES => {
                tries += 1;
                thread::sleep(RENAME_WAIT);
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// Creates the file at `path` with `bytes`, and fails with [`io::ErrorKind::AlreadyExists`] when something is there
/// (doc 07 §5.1 step 4: a file a human saves after the existence check is never overwritten).
///
/// `std::fs::rename` replaces its target on every platform, so the new file is linked into place instead: a hard link
/// fails when the target exists, and the file appears with its whole content at once. The temporary file is removed
/// either way. A file system without hard links makes the create fail with that file system's own error.
pub(crate) fn create_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    fs::write(&tmp, bytes)?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    linked
}

/// The line a write tool adds for a note in the bootstrap (doc 07 §1.1, doc 04 §2.3), without its line break.
pub(crate) fn bootstrap_notice(id: &str) -> String {
    format!("notice: {id} loads as instructions at session start")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recognitor-write-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_file_writes_a_new_file_and_leaves_no_temporary_file() {
        let dir = scratch("create");
        let target = dir.join("New.md");
        create_file(&target, b"# New\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"# New\n");
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["New.md"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_file_never_replaces_an_existing_file() {
        let dir = scratch("exists");
        let target = dir.join("Mine.md");
        fs::write(&target, b"a human wrote this").unwrap();
        let err = create_file(&target, b"an agent wrote this").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"a human wrote this");
        assert!(!temp_path(&target).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

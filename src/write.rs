// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! The file half of the write path (doc 07 §5.1 step 4): new bytes go to a temporary file next to the note, which is
//! then renamed over it. A reader sees the old file or the new one, never half of one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Renames `from` to `to`, and tries again a few times when Windows answers "access denied".
///
/// On Windows a virus scanner or an indexer can hold a file it has just seen open for a few milliseconds, and the
/// rename over it is then denied. That passes, so a denied rename is tried again a few times before it counts as a
/// failure.
fn rename_with_retries(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && tries < RENAME_TRIES => {
                tries += 1;
                thread::sleep(RENAME_WAIT);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Replaces the file at `path` with `bytes`. The temporary file is removed when the rename fails.
pub(crate) fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    fs::write(&tmp, bytes)?;
    rename_with_retries(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
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

/// Removes `path`, and tries again a few times when Windows answers "access denied" (see [`rename_with_retries`]).
fn remove_with_retries(path: &Path) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && tries < RENAME_TRIES => {
                tries += 1;
                thread::sleep(RENAME_WAIT);
            }
            Err(e) => return Err(e),
        }
    }
}

/// The temporary name a case-only rename passes through, next to the note. Hidden and not `.md`, like the temporary
/// file of [`replace_file`].
fn move_temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.recognitor-move"))
}

/// Moves the file `from` to `to` and fails with [`io::ErrorKind::AlreadyExists`] when something is at `to` (doc 07
/// §5.1 step 4: a move uses a rename that cannot replace, so a file a human saves after the existence check is never
/// overwritten).
///
/// `std::fs::rename` replaces its target on every platform, so the file is linked into its new place first (a hard
/// link fails when the target exists) and the old name is removed after. When the removal fails, the new link is
/// taken away again, so the move leaves one name or the other, never both.
///
/// `case_only` is a rename whose new name has the key of the old one (`caching.md` to `Caching.md`). On a file system
/// that ignores case, `to` is `from` itself, so the link would fail. The file goes through a temporary name instead:
/// renamed away from `from`, linked to `to`, and the temporary name removed. A failed link puts it back. The temporary
/// name never exists for longer than three file system calls; a crash between them leaves the note under its hidden
/// temporary name, which the sweep does not index (the first step of a repair is to rename it back).
pub(crate) fn move_file(from: &Path, to: &Path, case_only: bool) -> io::Result<()> {
    if case_only {
        let tmp = move_temp_path(from);
        if tmp.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is in the way", tmp.display()),
            ));
        }
        rename_with_retries(from, &tmp)?;
        if let Err(e) = fs::hard_link(&tmp, to) {
            let _ = rename_with_retries(&tmp, from);
            return Err(e);
        }
        return remove_with_retries(&tmp);
    }
    fs::hard_link(from, to)?;
    remove_with_retries(from).inspect_err(|_| {
        let _ = fs::remove_file(to);
    })
}

/// The vault's trash, from the vault root (doc 07 §10). `.recognitor/` is a hidden folder, so the sweep never indexes
/// what is in it.
const TRASH_DIR: &str = ".recognitor/trash";

/// Moves the note at `rel` (a path from the vault root) to `.recognitor/trash/<timestamp>/<rel>` and returns that
/// path (doc 07 §10). A rename inside the vault works on every platform and drive, and it cannot half-fail.
///
/// The timestamp folder is the UTC time of the delete, to the second (see [`trash_stamp`]). A second delete of the
/// same path in the same second gets `-2`, `-3`, ... after the stamp, so a delete never lands on an earlier one. The
/// write lock (doc 07 §5.1) makes the free-name check safe: nothing else writes to the trash.
pub(crate) fn move_to_trash(root: &Path, rel: &str, now: SystemTime) -> io::Result<String> {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let stamp = trash_stamp(secs);
    let mut attempt = 1u32;
    loop {
        let folder = if attempt == 1 {
            stamp.clone()
        } else {
            format!("{stamp}-{attempt}")
        };
        let trash_rel = format!("{TRASH_DIR}/{folder}/{rel}");
        let dest = root.join(&trash_rel);
        if dest.exists() {
            attempt += 1;
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        rename_with_retries(&root.join(rel), &dest)?;
        return Ok(trash_rel);
    }
}

/// `2026-11-18T14-30-05Z`: a UTC time in ISO 8601 order with dashes for the colons, which no Windows file name can
/// hold. Folders sort by time, and the name says when the delete happened.
fn trash_stamp(unix_secs: u64) -> String {
    let secs = i64::try_from(unix_secs).unwrap_or(i64::MAX / 2);
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{minute:02}-{second:02}Z")
}

/// The proleptic Gregorian date of a day count from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// The line a write tool adds for a note in the bootstrap (doc 07 §1.1, doc 04 §2.3), without its line break.
pub(crate) fn bootstrap_notice(id: &str) -> String {
    format!("notice: {id} loads as instructions at session start")
}

/// The line `delete_note` adds for a note that was in the bootstrap (doc 07 §10): it no longer loads.
pub(crate) fn bootstrap_deleted_notice(id: &str) -> String {
    format!("notice: {id} no longer loads as instructions at session start")
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
    fn trash_stamps_are_utc_iso_order_with_dashes() {
        assert_eq!(trash_stamp(0), "1970-01-01T00-00-00Z");
        assert_eq!(trash_stamp(1_000_000_000), "2001-09-09T01-46-40Z");
        assert_eq!(trash_stamp(1_700_000_000), "2023-11-14T22-13-20Z");
        // A leap day, and the day after it.
        assert_eq!(trash_stamp(1_709_164_800), "2024-02-29T00-00-00Z");
        assert_eq!(trash_stamp(1_709_251_199), "2024-02-29T23-59-59Z");
        assert_eq!(trash_stamp(1_709_251_200), "2024-03-01T00-00-00Z");
    }

    #[test]
    fn a_second_delete_of_the_same_path_in_one_second_gets_its_own_folder() {
        let dir = scratch("trash");
        fs::create_dir_all(dir.join("a")).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        fs::write(dir.join("a/N.md"), "one").unwrap();
        let first = move_to_trash(&dir, "a/N.md", now).unwrap();
        fs::write(dir.join("a/N.md"), "two").unwrap();
        let second = move_to_trash(&dir, "a/N.md", now).unwrap();
        assert_eq!(first, ".recognitor/trash/2023-11-14T22-13-20Z/a/N.md");
        assert_eq!(second, ".recognitor/trash/2023-11-14T22-13-20Z-2/a/N.md");
        assert_eq!(fs::read_to_string(dir.join(&first)).unwrap(), "one");
        assert_eq!(fs::read_to_string(dir.join(&second)).unwrap(), "two");
        assert!(!dir.join("a/N.md").exists());
        let _ = fs::remove_dir_all(&dir);
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
    fn move_file_moves_and_never_replaces() {
        let dir = scratch("move");
        fs::write(dir.join("A.md"), b"mine").unwrap();
        fs::write(dir.join("B.md"), b"a human wrote this").unwrap();
        let err = move_file(&dir.join("A.md"), &dir.join("B.md"), false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(dir.join("A.md")).unwrap(), b"mine");
        assert_eq!(fs::read(dir.join("B.md")).unwrap(), b"a human wrote this");
        fs::remove_file(dir.join("B.md")).unwrap();
        move_file(&dir.join("A.md"), &dir.join("B.md"), false).unwrap();
        assert!(!dir.join("A.md").exists());
        assert_eq!(fs::read(dir.join("B.md")).unwrap(), b"mine");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_case_only_move_changes_the_spelling_and_leaves_nothing_behind() {
        let dir = scratch("case-move");
        fs::write(dir.join("caching.md"), b"mine").unwrap();
        move_file(&dir.join("caching.md"), &dir.join("Caching.md"), true).unwrap();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["Caching.md"]);
        assert_eq!(fs::read(dir.join("Caching.md")).unwrap(), b"mine");
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

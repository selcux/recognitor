//! Stat-only directory walk (doc 02 §4). Reads directory listings, never file contents.
//!
//! On Windows the metadata comes from the directory listing itself, so the walk costs one call per folder.
//!
//! The walk also applies the name rules of doc 03 §4: every file and folder name is ASCII, and no two siblings share a
//! lowercase key. A name that breaks a rule is an error, and nothing under it or at it is indexed.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::error::{Error, Result};
use crate::paths::{note_key, strip_md_ignore_case};

/// One Markdown file found by the walk.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path from the vault root with `/` separators, exactly as named on disk.
    pub rel: String,
    pub abs: PathBuf,
    /// The note key this file would have: the path without `.md`, lowercased (doc 03 §4).
    pub key: String,
    /// Modification time, Unix epoch milliseconds.
    pub mtime_ms: i64,
    pub size: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameErrorKind {
    /// A file or folder name has a non-ASCII character (doc 03 §4).
    NonAscii,
    /// Two or more siblings share a key, folders included (doc 03 §4).
    CaseTwin,
}

/// A name the engine refuses to index (doc 03 §4, doc 07 §6). Nothing records it in the database: the next sweep
/// finds it again in the file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameError {
    pub kind: NameErrorKind,
    /// Every path involved, relative to the vault root as named on disk. Folders end with `/`.
    pub paths: Vec<String>,
    /// Notes left out because of this problem: the files themselves, plus every note under a folder.
    pub notes_left_out: usize,
}

/// What the walk skipped. Diagnostics for the shadow-mode report only.
#[derive(Debug, Clone, Default)]
pub struct WalkStats {
    /// Hidden folders skipped (doc 03 §4), by vault-relative path.
    pub hidden_dirs: Vec<String>,
    /// `.md` files inside hidden folders. Counted only when the caller asks, because it walks those folders.
    pub hidden_md_files: Option<usize>,
    /// Files that are not `*.md`, by lowercase extension (`""` for none).
    pub other_files: BTreeMap<String, usize>,
    /// `.md` files directly in the vault root. They are not notes (doc 03 §4).
    pub root_md_files: usize,
    /// Symbolic links (files or folders). Not followed.
    pub symlinks: usize,
}

/// Result of one walk.
#[derive(Debug, Clone, Default)]
pub struct Walked {
    /// Every `.md` file that may be indexed, sorted by path.
    pub files: Vec<FileEntry>,
    pub name_errors: Vec<NameError>,
    /// Files the listing showed but whose metadata could not be read, as `(path, message)`. Not NotFound.
    pub unreadable: Vec<(String, String)>,
    pub stats: WalkStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Dir,
    MdFile,
}

/// Which entries of one folder break a name rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdicts {
    /// Indices of entries with a non-ASCII character in the name.
    pub non_ascii: Vec<usize>,
    /// Groups of indices whose names share a key. Each group has two or more entries.
    pub twins: Vec<Vec<usize>>,
}

/// Applies the name rules to the entries of one folder (doc 03 §4). Folders are compared with folders and files with
/// files; a file's key leaves out `.md`, so `Note.md` and `Note.MD` are twins and `x.md` next to a folder `x` is not.
pub fn classify(entries: &[(String, EntryKind)]) -> Verdicts {
    let mut verdicts = Verdicts::default();
    let mut groups: HashMap<(EntryKind, String), Vec<usize>> = HashMap::new();
    for (i, (name, kind)) in entries.iter().enumerate() {
        if !name.is_ascii() {
            verdicts.non_ascii.push(i);
            continue;
        }
        let stem = match kind {
            EntryKind::Dir => name.as_str(),
            EntryKind::MdFile => strip_md_ignore_case(name).unwrap_or(name),
        };
        groups.entry((*kind, stem.to_lowercase())).or_default().push(i);
    }
    verdicts.twins = groups.into_values().filter(|g| g.len() > 1).collect();
    verdicts.twins.sort();
    verdicts
}

/// Walks `root` and returns every `*.md` file under a top-level folder that is outside hidden folders and passes the
/// name rules, sorted by path.
pub fn walk(root: &Path, count_hidden: bool) -> Result<Walked> {
    walk_with_hook(root, count_hidden, &mut |_| {})
}

/// A moment in the walk where a test can change the file system. Production passes a no-op hook.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub enum WalkPoint<'a> {
    /// Right before the metadata of a listed `.md` file is read.
    BeforeStat(&'a Path),
    /// Right before the walk lists a subfolder.
    BeforeDescend(&'a Path),
}

/// [`walk`] with a hook called at each [`WalkPoint`]. Generic, so the no-op hook of `walk` costs nothing.
#[doc(hidden)]
pub fn walk_with_hook<F: FnMut(WalkPoint<'_>)>(root: &Path, count_hidden: bool, hook: &mut F) -> Result<Walked> {
    let mut walked = Walked::default();
    walk_dir(root, "", &mut walked, hook)?;
    if count_hidden {
        let mut n = 0;
        for rel in &walked.stats.hidden_dirs {
            n += count_md(&root.join(rel), false);
        }
        walked.stats.hidden_md_files = Some(n);
    }
    walked.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    walked.name_errors.sort_by(|a, b| a.paths.cmp(&b.paths));
    Ok(walked)
}

/// A directory entry that took part in the name check.
struct Pending {
    name: String,
    kind: EntryKind,
    path: PathBuf,
    /// `(mtime_ms, size)` of a file, read from the directory listing (no extra call on Windows).
    stat: (i64, i64),
}

fn walk_dir<F: FnMut(WalkPoint<'_>)>(dir: &Path, rel_dir: &str, walked: &mut Walked, hook: &mut F) -> Result<()> {
    let at_root = rel_dir.is_empty();
    let mut pending: Vec<Pending> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A folder removed since its parent was listed: its notes are gone. The vault root itself must exist.
        Err(e) if e.kind() == ErrorKind::NotFound && !at_root => return Ok(()),
        Err(e) => return Err(Error::io(dir, e)),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(dir, e)),
        };
        // A name that is not valid UTF-8 comes out with U+FFFD, which is not ASCII, so the name rules catch it.
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            // Removed between the listing and this call.
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(entry.path(), e)),
        };
        if file_type.is_symlink() {
            walked.stats.symlinks += 1;
        } else if file_type.is_dir() {
            if name.starts_with('.') {
                walked.stats.hidden_dirs.push(join_rel(rel_dir, &name));
            } else {
                pending.push(Pending {
                    name,
                    kind: EntryKind::Dir,
                    path: entry.path(),
                    stat: (0, 0),
                });
            }
        } else if file_type.is_file() {
            match strip_md_ignore_case(&name) {
                Some(stem) if !stem.is_empty() && at_root => walked.stats.root_md_files += 1,
                Some(stem) if !stem.is_empty() => {
                    // Built once and reused below, so the hook adds no allocation in production.
                    let path = entry.path();
                    hook(WalkPoint::BeforeStat(&path));
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        // Deleted or renamed away since the listing: it is not there any more.
                        Err(e) if e.kind() == ErrorKind::NotFound => continue,
                        Err(e) => {
                            // Listed but not statable (a lock): the sweep keeps its stored note and warns.
                            walked.unreadable.push((join_rel(rel_dir, &name), e.to_string()));
                            continue;
                        }
                    };
                    pending.push(Pending {
                        name,
                        kind: EntryKind::MdFile,
                        path,
                        stat: (mtime_ms(&meta), i64::try_from(meta.len()).unwrap_or(i64::MAX)),
                    });
                }
                _ => {
                    let ext = Path::new(&name)
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    *walked.stats.other_files.entry(ext).or_default() += 1;
                }
            }
        }
    }

    let kinds: Vec<(String, EntryKind)> = pending.iter().map(|p| (p.name.clone(), p.kind)).collect();
    let verdicts = classify(&kinds);
    let mut bad = vec![false; pending.len()];
    for &i in &verdicts.non_ascii {
        bad[i] = true;
        let p = &pending[i];
        walked.name_errors.push(NameError {
            kind: NameErrorKind::NonAscii,
            paths: vec![display_path(rel_dir, p)],
            notes_left_out: notes_left_out(p),
        });
    }
    for group in &verdicts.twins {
        let mut paths = Vec::new();
        let mut left_out = 0;
        for &i in group {
            bad[i] = true;
            paths.push(display_path(rel_dir, &pending[i]));
            left_out += notes_left_out(&pending[i]);
        }
        paths.sort();
        walked.name_errors.push(NameError {
            kind: NameErrorKind::CaseTwin,
            paths,
            notes_left_out: left_out,
        });
    }

    for (p, bad) in pending.into_iter().zip(bad) {
        if bad {
            continue;
        }
        let rel = join_rel(rel_dir, &p.name);
        match p.kind {
            EntryKind::Dir => {
                hook(WalkPoint::BeforeDescend(&p.path));
                walk_dir(&p.path, &rel, walked, hook)?;
            }
            EntryKind::MdFile => walked.files.push(FileEntry {
                key: note_key(&rel),
                rel,
                abs: p.path,
                mtime_ms: p.stat.0,
                size: p.stat.1,
            }),
        }
    }
    Ok(())
}

fn join_rel(rel_dir: &str, name: &str) -> String {
    if rel_dir.is_empty() {
        name.to_string()
    } else {
        format!("{rel_dir}/{name}")
    }
}

fn display_path(rel_dir: &str, p: &Pending) -> String {
    let rel = join_rel(rel_dir, &p.name);
    match p.kind {
        EntryKind::Dir => format!("{rel}/"),
        EntryKind::MdFile => rel,
    }
}

fn notes_left_out(p: &Pending) -> usize {
    match p.kind {
        EntryKind::Dir => count_md(&p.path, true),
        EntryKind::MdFile => 1,
    }
}

fn count_md(dir: &Path, skip_hidden: bool) -> usize {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    let mut n = 0;
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if ft.is_dir() {
            if !(skip_hidden && name.starts_with('.')) {
                n += count_md(&entry.path(), skip_hidden);
            }
        } else if ft.is_file() && strip_md_ignore_case(&name).is_some() {
            n += 1;
        }
    }
    n
}

/// Modification time in Unix milliseconds. Times before 1970 come out negative.
pub fn mtime_ms(meta: &fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
        },
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A fresh folder under the temp dir, removed when the guard drops, pass or fail.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("recognitor-test-{}-{n}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            // Test setup: a failure here fails the test.
            fs::create_dir_all(&dir).expect("create the temp folder");
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_folder_removed_before_it_is_listed_is_skipped() {
        let tmp = TempDir::new("walk-folder-vanishes");
        let root = tmp.0.join("vault");
        for rel in ["keep/a.md", "gone/b.md", "gone/deep/c.md", "other/d.md"] {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().expect("every path has a parent")).expect("create folder");
            fs::write(
                &p, "# x
",
            )
            .expect("write file");
        }
        let mut fired = Vec::new();
        let walked = walk_with_hook(&root, false, &mut |point| {
            if let WalkPoint::BeforeDescend(dir) = point {
                fired.push(dir.to_path_buf());
                if dir.ends_with("gone") {
                    fs::remove_dir_all(dir).expect("remove the folder");
                }
            }
        })
        .expect("the walk survives a vanished folder");
        let rels: Vec<&str> = walked.files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["keep/a.md", "other/d.md"]);
        assert!(fired.iter().any(|d| d.ends_with("gone")), "the hook saw the folder");
        assert!(walked.name_errors.is_empty() && walked.unreadable.is_empty());
    }

    fn entries(list: &[(&str, EntryKind)]) -> Vec<(String, EntryKind)> {
        list.iter().map(|(n, k)| (n.to_string(), *k)).collect()
    }

    #[test]
    fn file_twins_ignore_case_and_extension_case() {
        let v = classify(&entries(&[
            ("Nutrition.md", EntryKind::MdFile),
            ("nutrition.md", EntryKind::MdFile),
            ("Note.md", EntryKind::MdFile),
            ("Note.MD", EntryKind::MdFile),
            ("Other.md", EntryKind::MdFile),
        ]));
        assert_eq!(v.twins, vec![vec![0, 1], vec![2, 3]]);
        assert!(v.non_ascii.is_empty());
    }

    #[test]
    fn folder_twins_and_non_twins() {
        let v = classify(&entries(&[
            ("Baseline", EntryKind::Dir),
            ("baseline", EntryKind::Dir),
            // A file and a folder with the same stem are not twins.
            ("guide.md", EntryKind::MdFile),
            ("Guide", EntryKind::Dir),
        ]));
        assert_eq!(v.twins, vec![vec![0, 1]]);
    }

    #[test]
    fn non_ascii_names_are_flagged_and_never_twins() {
        let v = classify(&entries(&[
            ("k\u{131}rm\u{131}z\u{131}.md", EntryKind::MdFile),
            ("KIRMIZI.md", EntryKind::MdFile),
            ("kirmizi.md", EntryKind::MdFile),
            ("\u{c7}al\u{131}\u{15f}ma", EntryKind::Dir),
        ]));
        assert_eq!(v.non_ascii, vec![0, 3]);
        assert_eq!(v.twins, vec![vec![1, 2]]);
    }
}

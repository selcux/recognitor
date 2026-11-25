// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor doctor` (doc 04 §5.3, doc 07 §12.5): the read-only check of the bootstrap block in harness
//! instruction files.

use std::fs;
use std::path::{Component, Path, PathBuf};

use toml_edit::{DocumentMut, Table};

use crate::error::{Error, Result};
use crate::hash::sha256_hex;
use crate::index::Index;

const START_PREFIX: &[u8] = b"<!-- recognitor bootstrap sha=";
const START_SUFFIX: &[u8] = b" -->";
const END_MARKER: &[u8] = b"<!-- recognitor bootstrap end -->";
const DEFAULT_FILES: &[&str] = &["AGENTS.md"];

/// One harness declared in the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    /// Canonical name the CLI prints and that `bootstrap write` will take.
    pub name: String,
    /// Resolved directory path (absolute, or home-expanded).
    pub directory: PathBuf,
    /// Relative paths inside [`Harness::directory`], as written.
    pub files: Vec<String>,
}

/// State of one instruction file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// No start or end marker line.
    BlockAbsent,
    /// One start and one end, end after start, stamp and bytes match the compiled payload.
    UpToDate,
    /// One start and one end, end after start, stamp or bytes disagree with the compiled payload.
    OutOfDate,
    /// Any other marker arrangement.
    Misconfigured,
    /// Named file is not on disk.
    Missing,
}

impl FileState {
    /// One glyph every default terminal font can draw (doc 07 §12.5).
    fn mark(self) -> &'static str {
        match self {
            FileState::UpToDate => "+",
            FileState::BlockAbsent => "-",
            FileState::OutOfDate => "\u{2191}", // ↑
            FileState::Misconfigured => "?",
            FileState::Missing => "\u{00d7}", // ×
        }
    }

    fn suggests_write(self) -> bool {
        matches!(self, FileState::BlockAbsent | FileState::OutOfDate)
    }

    fn failed(self) -> bool {
        !matches!(self, FileState::UpToDate)
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    pub harness: String,
    /// Absolute path of the instruction file (for errors and classification).
    pub path: PathBuf,
    /// Relative path as listed in the harness `files` entry.
    pub file: String,
    pub state: FileState,
}

/// The doctor check result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub files: Vec<FileReport>,
}

impl DoctorReport {
    /// True when any printed file is not up to date (doc 07 §12.5).
    pub fn failed(&self) -> bool {
        self.files.iter().any(|f| f.state.failed())
    }

    /// Report text (doc 07 §12.5): one short line per file, then one `write` command after `---` when any
    /// file is block absent or out of date.
    pub fn render(&self) -> String {
        if self.files.is_empty() {
            return String::new();
        }
        let mut counts: Vec<(String, usize)> = Vec::new();
        for f in &self.files {
            if let Some((_, n)) = counts.iter_mut().find(|(name, _)| name == &f.harness) {
                *n += 1;
            } else {
                counts.push((f.harness.clone(), 1));
            }
        }
        let multi = |name: &str| counts.iter().any(|(n, c)| n == name && *c > 1);

        let mut out = String::new();
        let mut write_names: Vec<String> = Vec::new();
        for f in &self.files {
            if multi(&f.harness) {
                out.push_str(&format!("{} {} {}\n", f.harness, f.file, f.state.mark()));
            } else {
                out.push_str(&format!("{} {}\n", f.harness, f.state.mark()));
            }
            if f.state.suggests_write() && !write_names.iter().any(|n| n == &f.harness) {
                write_names.push(f.harness.clone());
            }
        }
        if !write_names.is_empty() {
            out.push_str("---\n");
            out.push_str(&format!("recognitor bootstrap write {}\n", write_names.join(", ")));
        }
        out
    }
}

#[derive(Clone)]
struct HarnessDraft {
    name: String,
    directory: String,
    /// `None` means the default (`AGENTS.md`); `Some([])` is the same after resolution.
    files: Option<Vec<String>>,
}

/// Parses `[harness.*]` from the config file. A missing file is an empty document: no harness is declared.
/// A bad table is [`Error::Config`].
pub fn load_harnesses(config_path: &Path) -> Result<Vec<Harness>> {
    load_harnesses_with_home(config_path, home_dir()?)
}

/// [`load_harnesses`] with the home directory passed in (tests inject a temp folder).
pub fn load_harnesses_with_home(config_path: &Path, home: PathBuf) -> Result<Vec<Harness>> {
    let doc = read_document(config_path)?;
    let drafts = merge_harnesses(config_path, &doc)?;
    drafts
        .into_iter()
        .map(|d| resolve_harness(config_path, &home, d))
        .collect()
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DocumentMut::new()),
        Err(e) => return Err(Error::io(path, e)),
    };
    text.parse::<DocumentMut>().map_err(|e| Error::Config {
        path: path.to_path_buf(),
        message: e.message().to_string(),
    })
}

fn bad_config(path: &Path, message: impl Into<String>) -> Error {
    Error::Config {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn merge_harnesses(config_path: &Path, doc: &DocumentMut) -> Result<Vec<HarnessDraft>> {
    let Some(item) = doc.get("harness") else {
        return Ok(Vec::new());
    };
    let Some(table) = item.as_table() else {
        return Err(bad_config(config_path, "[harness] must be a table of harnesses"));
    };

    // Detect two keys that differ only by case before reading the entries.
    let mut seen_folded: Vec<(String, String)> = Vec::new();
    for (key, _) in table.iter() {
        let folded = key.to_ascii_lowercase();
        if let Some((_, first)) = seen_folded.iter().find(|(f, _)| f == &folded) {
            return Err(bad_config(
                config_path,
                format!("harness names {first:?} and {key:?} differ only by case"),
            ));
        }
        seen_folded.push((folded, key.to_string()));
    }

    let mut drafts = Vec::new();
    for (key, item) in table.iter() {
        let Some(entry) = item.as_table() else {
            return Err(bad_config(config_path, format!("[harness.{key}] must be a table")));
        };
        let folded = key.to_ascii_lowercase();
        if !is_valid_harness_name(&folded) {
            return Err(bad_config(
                config_path,
                format!(
                    "harness name {key:?} is illegal: use ASCII lowercase letters, digits and hyphens, starting with a letter"
                ),
            ));
        }
        let overlay = parse_overlay(config_path, key, entry)?;
        let directory = overlay
            .directory
            .ok_or_else(|| bad_config(config_path, format!("[harness.{key}] needs a directory")))?;
        drafts.push(HarnessDraft {
            name: folded,
            directory,
            files: overlay.files,
        });
    }
    Ok(drafts)
}

struct Overlay {
    directory: Option<String>,
    files: Option<Vec<String>>,
}

fn parse_overlay(config_path: &Path, name: &str, table: &Table) -> Result<Overlay> {
    let mut directory = None;
    let mut files = None;
    for (key, item) in table.iter() {
        match key {
            "directory" => {
                let Some(s) = item.as_str() else {
                    return Err(bad_config(
                        config_path,
                        format!("[harness.{name}].directory must be a string"),
                    ));
                };
                directory = Some(s.to_string());
            }
            "files" => {
                let Some(arr) = item.as_array() else {
                    return Err(bad_config(
                        config_path,
                        format!("[harness.{name}].files must be an array of strings"),
                    ));
                };
                let mut list = Vec::with_capacity(arr.len());
                for (i, v) in arr.iter().enumerate() {
                    let Some(s) = v.as_str() else {
                        return Err(bad_config(
                            config_path,
                            format!("[harness.{name}].files[{i}] must be a string"),
                        ));
                    };
                    list.push(s.to_string());
                }
                files = Some(list);
            }
            other => {
                return Err(bad_config(
                    config_path,
                    format!("[harness.{name}] has unknown field {other:?}; only directory and files are valid"),
                ));
            }
        }
    }
    Ok(Overlay { directory, files })
}

fn is_valid_harness_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn resolve_harness(config_path: &Path, home: &Path, draft: HarnessDraft) -> Result<Harness> {
    let directory = resolve_directory(config_path, home, &draft.name, &draft.directory)?;
    let files = match draft.files {
        None => DEFAULT_FILES.iter().map(|s| (*s).to_string()).collect(),
        Some(list) if list.is_empty() => DEFAULT_FILES.iter().map(|s| (*s).to_string()).collect(),
        Some(list) => {
            validate_files(config_path, &draft.name, &list)?;
            list
        }
    };
    Ok(Harness {
        name: draft.name,
        directory,
        files,
    })
}

fn resolve_directory(config_path: &Path, home: &Path, name: &str, raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        return Err(bad_config(config_path, format!("[harness.{name}].directory is empty")));
    }
    if raw == "~" {
        return Err(bad_config(
            config_path,
            format!("[harness.{name}].directory is \"~\"; use ~/... or an absolute path"),
        ));
    }
    if let Some(rest) = raw.strip_prefix('~') {
        let first = rest.as_bytes().first().copied();
        if first != Some(b'/') && first != Some(b'\\') {
            return Err(bad_config(
                config_path,
                format!(
                    "[harness.{name}].directory {raw:?} is not a home-relative path; use ~/... or an absolute path"
                ),
            ));
        }
        let joined = home.join(&rest[1..]);
        return Ok(joined);
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(bad_config(
            config_path,
            format!("[harness.{name}].directory {raw:?} is relative; use ~/... or an absolute path"),
        ));
    }
    Ok(path)
}

fn validate_files(config_path: &Path, name: &str, files: &[String]) -> Result<()> {
    let mut seen: Vec<String> = Vec::new();
    for entry in files {
        if entry.is_empty() {
            return Err(bad_config(
                config_path,
                format!("[harness.{name}].files has an empty path"),
            ));
        }
        let path = Path::new(entry);
        if path.is_absolute() {
            return Err(bad_config(
                config_path,
                format!("[harness.{name}].files entry {entry:?} is absolute; use a path inside the harness directory"),
            ));
        }
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    return Err(bad_config(
                        config_path,
                        format!("[harness.{name}].files entry {entry:?} has a .. component"),
                    ));
                }
                Component::Normal(_) | Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => {
                    return Err(bad_config(
                        config_path,
                        format!(
                            "[harness.{name}].files entry {entry:?} is absolute; use a path inside the harness directory"
                        ),
                    ));
                }
            }
        }
        let key = normalize_file_key(entry);
        if seen.iter().any(|s| s == &key) {
            return Err(bad_config(
                config_path,
                format!("[harness.{name}].files repeats {entry:?}"),
            ));
        }
        seen.push(key);
    }
    Ok(())
}

/// Relative path key for the repeat check: `/` and `\` are the same, `.` components are dropped, case as written.
fn normalize_file_key(entry: &str) -> String {
    let path = Path::new(entry);
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    parts.join("/")
}

fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .ok_or_else(|| Error::Config {
            path: PathBuf::from("<home>"),
            message: "no user profile directory found; set harness directories to absolute paths".to_string(),
        })
}

/// Classifies the raw bytes of one instruction file against the compiled payload.
pub fn classify_bytes(content: &[u8], payload: &[u8]) -> FileState {
    let markers = find_markers(content);
    match markers {
        Markers::None => FileState::BlockAbsent,
        Markers::Both { stamp, body } => {
            let body_hash = sha256_hex(body);
            let stamp_ok = stamp.eq_ignore_ascii_case(&body_hash);
            let payload_ok = body_hash == sha256_hex(payload);
            if stamp_ok && payload_ok {
                FileState::UpToDate
            } else {
                FileState::OutOfDate
            }
        }
        Markers::Misconfigured => FileState::Misconfigured,
    }
}

enum Markers<'a> {
    None,
    Both { stamp: String, body: &'a [u8] },
    Misconfigured,
}

fn find_markers(content: &[u8]) -> Markers<'_> {
    let mut starts: Vec<(usize, usize, String)> = Vec::new();
    let mut ends: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0;
    while offset <= content.len() {
        let end = content[offset..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| offset + i)
            .unwrap_or(content.len());
        let mut line = &content[offset..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        if let Some(stamp) = parse_start_line(line) {
            starts.push((offset, end, stamp));
        } else if line == END_MARKER {
            ends.push((offset, end));
        }
        if end == content.len() {
            break;
        }
        offset = end + 1;
    }
    if starts.is_empty() && ends.is_empty() {
        return Markers::None;
    }
    if starts.len() != 1 || ends.len() != 1 {
        return Markers::Misconfigured;
    }
    let (start_off, start_end, stamp) = starts[0].clone();
    let (end_off, _) = ends[0];
    if end_off <= start_off {
        return Markers::Misconfigured;
    }
    // Body starts after the start line's newline.
    let body_start = if start_end < content.len() {
        start_end + 1
    } else {
        // Start line is the last line with no trailing newline: no body.
        content.len()
    };
    if body_start > end_off {
        return Markers::Misconfigured;
    }
    Markers::Both {
        stamp,
        body: &content[body_start..end_off],
    }
}

fn parse_start_line(line: &[u8]) -> Option<String> {
    if !line.starts_with(START_PREFIX) || !line.ends_with(START_SUFFIX) {
        return None;
    }
    let hex = &line[START_PREFIX.len()..line.len() - START_SUFFIX.len()];
    if hex.len() != 64 || !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(String::from_utf8_lossy(hex).into_owned())
}

/// One installed instruction file: harness name, absolute path, relative path as listed in `files`.
pub type InstalledFile = (String, PathBuf, String);

/// Resolves which files to check: skips harnesses whose directory is absent; errors when a path exists and is not a
/// directory. Call this before opening the vault so a shape error never sweeps.
pub fn installed_files(harnesses: &[Harness]) -> Result<Vec<InstalledFile>> {
    let mut out = Vec::new();
    for h in harnesses {
        match fs::metadata(&h.directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(Error::io(&h.directory, e)),
            Ok(meta) if !meta.is_dir() => {
                return Err(Error::Config {
                    path: h.directory.clone(),
                    message: format!("{} exists and is not a directory", h.directory.display()),
                });
            }
            Ok(_) => {}
        }
        for file in &h.files {
            out.push((h.name.clone(), h.directory.join(file), file.clone()));
        }
    }
    Ok(out)
}

/// Classifies each installed instruction file against the compiled bootstrap payload.
///
/// A path that exists and is not a file is an error and no report is produced. A missing named file is a report line.
pub fn doctor(index: &mut Index, installed: &[InstalledFile], max_tokens: usize) -> Result<DoctorReport> {
    let bootstrap = index.bootstrap()?;
    let payload = bootstrap.render(max_tokens);
    let payload_bytes = payload.as_bytes();

    let mut files = Vec::with_capacity(installed.len());
    for (harness, path, file) in installed {
        let state = match fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileState::Missing,
            Err(e) => return Err(Error::io(path, e)),
            Ok(meta) if meta.is_dir() => {
                return Err(Error::Config {
                    path: path.clone(),
                    message: format!("{} is a directory; expected an instruction file", path.display()),
                });
            }
            Ok(meta) if !meta.is_file() => {
                return Err(Error::Config {
                    path: path.clone(),
                    message: format!("{} is not a file", path.display()),
                });
            }
            Ok(_) => {
                let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
                classify_bytes(&bytes, payload_bytes)
            }
        };
        files.push(FileReport {
            harness: harness.clone(),
            path: path.clone(),
            file: file.clone(),
            state,
        });
    }
    Ok(DoctorReport { files })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct Temp {
        base: PathBuf,
    }

    impl Temp {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let base = std::env::temp_dir().join(format!("recognitor-doctor-unit-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            fs::create_dir_all(&base).unwrap();
            Temp { base }
        }

        fn config(&self) -> PathBuf {
            self.base.join("config.toml")
        }

        fn home(&self) -> PathBuf {
            self.base.join("home")
        }

        fn write_config(&self, text: &str) {
            fs::write(self.config(), text).unwrap();
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn abs(t: &Temp, name: &str) -> String {
        t.base.join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn a_missing_config_declares_no_harness() {
        let t = Temp::new();
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn omitted_files_means_agents_md() {
        let t = Temp::new();
        let dir = abs(&t, "my-claude");
        t.write_config(&format!(
            "[harness.claude]\ndirectory = \"{}\"\n",
            dir.replace('\\', "/")
        ));
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "claude");
        assert_eq!(list[0].files, ["AGENTS.md"]);
        assert_eq!(list[0].directory, PathBuf::from(&dir));
    }

    #[test]
    fn empty_files_array_means_agents_md() {
        let t = Temp::new();
        let dir = abs(&t, "ag");
        t.write_config(&format!(
            "[harness.antigravity]\ndirectory = \"{}\"\nfiles = []\n",
            dir.replace('\\', "/")
        ));
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        let h = list.iter().find(|h| h.name == "antigravity").unwrap();
        assert_eq!(h.files, ["AGENTS.md"]);
    }

    #[test]
    fn order_is_the_order_in_the_file() {
        let t = Temp::new();
        let dir = abs(&t, "tool");
        let d = dir.replace('\\', "/");
        t.write_config(&format!(
            "[harness.my-tool]\ndirectory = \"{d}\"\n[harness.claude]\ndirectory = \"{d}\"\nfiles = [\"CLAUDE.md\"]\n"
        ));
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        assert_eq!(
            list.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["my-tool", "claude"]
        );
        assert_eq!(list[0].files, ["AGENTS.md"]);
        assert_eq!(list[1].files, ["CLAUDE.md"]);
    }

    #[test]
    fn mixed_case_selects_the_canonical_name() {
        let t = Temp::new();
        let dir = abs(&t, "c");
        t.write_config(&format!(
            "[harness.Claude]\ndirectory = \"{}\"\n",
            dir.replace('\\', "/")
        ));
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "claude");
        assert_eq!(list[0].directory, PathBuf::from(&dir));
    }

    #[test]
    fn names_that_differ_only_by_case_are_an_error() {
        let t = Temp::new();
        let dir = abs(&t, "x");
        t.write_config(&format!(
            "[harness.foo]\ndirectory = \"{0}\"\n[harness.Foo]\ndirectory = \"{0}\"\n",
            dir.replace('\\', "/")
        ));
        let err = load_harnesses_with_home(&t.config(), t.home()).unwrap_err();
        assert!(err.to_string().contains("differ only by case"), "{err}");
    }

    #[test]
    fn unknown_field_is_an_error() {
        let t = Temp::new();
        let dir = abs(&t, "x");
        t.write_config(&format!(
            "[harness.claude]\ndirectory = \"{}\"\nfile = \"CLAUDE.md\"\n",
            dir.replace('\\', "/")
        ));
        let err = load_harnesses_with_home(&t.config(), t.home()).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn relative_directory_is_an_error() {
        let t = Temp::new();
        t.write_config("[harness.claude]\ndirectory = \".claude\"\n");
        let err = load_harnesses_with_home(&t.config(), t.home()).unwrap_err();
        assert!(err.to_string().contains("relative"), "{err}");
    }

    #[test]
    fn tilde_slash_expands_to_home() {
        let t = Temp::new();
        t.write_config("[harness.claude]\ndirectory = \"~/.claude\"\n");
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        assert_eq!(list[0].directory, t.home().join(".claude"));
        // TOML needs `\\` in the file so the value is `~\.claude`.
        t.write_config("[harness.claude]\ndirectory = \"~\\\\.claude\"\n");
        let list = load_harnesses_with_home(&t.config(), t.home()).unwrap();
        assert_eq!(list[0].directory, t.home().join(".claude"));
    }

    #[test]
    fn tilde_alone_and_tilde_user_are_errors() {
        let t = Temp::new();
        t.write_config("[harness.claude]\ndirectory = \"~\"\n");
        assert!(load_harnesses_with_home(&t.config(), t.home()).is_err());
        t.write_config("[harness.claude]\ndirectory = \"~bob/.claude\"\n");
        assert!(load_harnesses_with_home(&t.config(), t.home()).is_err());
    }

    #[test]
    fn files_with_dotdot_or_absolute_or_repeat_are_errors() {
        let t = Temp::new();
        let dir = abs(&t, "x");
        let d = dir.replace('\\', "/");
        t.write_config(&format!(
            "[harness.my-tool]\ndirectory = \"{d}\"\nfiles = [\"../x.md\"]\n"
        ));
        assert!(
            load_harnesses_with_home(&t.config(), t.home())
                .unwrap_err()
                .to_string()
                .contains("..")
        );
        t.write_config(&format!(
            "[harness.my-tool]\ndirectory = \"{d}\"\nfiles = [\"{d}/a.md\"]\n"
        ));
        assert!(
            load_harnesses_with_home(&t.config(), t.home())
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        t.write_config(&format!(
            "[harness.my-tool]\ndirectory = \"{d}\"\nfiles = [\"a.md\", \"./a.md\"]\n"
        ));
        assert!(
            load_harnesses_with_home(&t.config(), t.home())
                .unwrap_err()
                .to_string()
                .contains("repeats")
        );
    }

    #[test]
    fn new_harness_needs_directory() {
        let t = Temp::new();
        t.write_config("[harness.my-tool]\nfiles = [\"AGENTS.md\"]\n");
        let err = load_harnesses_with_home(&t.config(), t.home()).unwrap_err();
        assert!(err.to_string().contains("needs a directory"), "{err}");
    }

    #[test]
    fn illegal_harness_name_is_an_error() {
        let t = Temp::new();
        let dir = abs(&t, "x");
        t.write_config(&format!("[harness.1bad]\ndirectory = \"{}\"\n", dir.replace('\\', "/")));
        assert!(load_harnesses_with_home(&t.config(), t.home()).is_err());
    }

    #[test]
    fn classify_no_markers_is_block_absent() {
        assert_eq!(classify_bytes(b"hello\n", b"payload"), FileState::BlockAbsent);
    }

    #[test]
    fn classify_matching_block_is_up_to_date() {
        let payload = b"hello\n";
        let hash = sha256_hex(payload);
        let file = format!("<!-- recognitor bootstrap sha={hash} -->\nhello\n<!-- recognitor bootstrap end -->\n");
        assert_eq!(classify_bytes(file.as_bytes(), payload), FileState::UpToDate);
    }

    #[test]
    fn classify_wrong_stamp_is_out_of_date_even_when_bytes_match() {
        let payload = b"hello\n";
        let wrong = "0".repeat(64);
        let file = format!("<!-- recognitor bootstrap sha={wrong} -->\nhello\n<!-- recognitor bootstrap end -->\n");
        assert_eq!(classify_bytes(file.as_bytes(), payload), FileState::OutOfDate);
    }

    #[test]
    fn classify_crlf_body_does_not_match_lf_payload() {
        let payload = b"hello\n";
        let hash = sha256_hex(b"hello\r\n");
        let file =
            format!("<!-- recognitor bootstrap sha={hash} -->\r\nhello\r\n<!-- recognitor bootstrap end -->\r\n");
        assert_eq!(classify_bytes(file.as_bytes(), payload), FileState::OutOfDate);
    }

    #[test]
    fn classify_one_marker_is_misconfigured() {
        assert_eq!(
            classify_bytes(b"<!-- recognitor bootstrap end -->\n", b"p"),
            FileState::Misconfigured
        );
        let hash = "a".repeat(64);
        let file = format!("<!-- recognitor bootstrap sha={hash} -->\nonly\n");
        assert_eq!(classify_bytes(file.as_bytes(), b"p"), FileState::Misconfigured);
    }

    #[test]
    fn classify_end_before_start_is_misconfigured() {
        let hash = "a".repeat(64);
        let file = format!("<!-- recognitor bootstrap end -->\n<!-- recognitor bootstrap sha={hash} -->\nx\n");
        assert_eq!(classify_bytes(file.as_bytes(), b"x\n"), FileState::Misconfigured);
    }

    fn row(harness: &str, file: &str, state: FileState) -> FileReport {
        FileReport {
            harness: harness.into(),
            path: PathBuf::from(format!("/{harness}/{file}")),
            file: file.into(),
            state,
        }
    }

    #[test]
    fn report_collects_write_names_after_a_separator() {
        let report = DoctorReport {
            files: vec![
                row("a", "AGENTS.md", FileState::UpToDate),
                row("b", "AGENTS.md", FileState::BlockAbsent),
                row("c", "AGENTS.md", FileState::Misconfigured),
                row("d", "AGENTS.md", FileState::Missing),
                row("e", "AGENTS.md", FileState::OutOfDate),
            ],
        };
        assert_eq!(
            report.render(),
            "a +\nb -\nc ?\nd \u{00d7}\ne \u{2191}\n---\nrecognitor bootstrap write b, e\n"
        );
        assert!(report.failed());
    }

    #[test]
    fn a_multi_file_harness_shows_the_relative_path() {
        let report = DoctorReport {
            files: vec![
                row("pi", "AGENTS.md", FileState::UpToDate),
                row("pi", "extra.md", FileState::Missing),
                row("claude", "CLAUDE.md", FileState::BlockAbsent),
            ],
        };
        assert_eq!(
            report.render(),
            "pi AGENTS.md +\npi extra.md \u{00d7}\nclaude -\n---\nrecognitor bootstrap write claude\n"
        );
    }

    #[test]
    fn an_all_up_to_date_report_has_no_write_block() {
        let report = DoctorReport {
            files: vec![row("claude", "CLAUDE.md", FileState::UpToDate)],
        };
        assert_eq!(report.render(), "claude +\n");
        assert!(!report.failed());
    }
}

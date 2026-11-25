// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! Harness instruction files shared by `doctor` and `bootstrap write` (doc 04 §5.3, doc 07 §12.5 and §12.6): the
//! `[harness.<name>]` tables, the marker scan, and the block states.

use std::fs;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use toml_edit::{DocumentMut, Table};

use crate::error::{Error, Result};
use crate::hash::sha256_hex;

pub const START_PREFIX: &[u8] = b"<!-- recognitor bootstrap sha=";
pub const START_SUFFIX: &[u8] = b" -->";
pub const END_MARKER: &[u8] = b"<!-- recognitor bootstrap end -->";
const DEFAULT_FILES: &[&str] = &["AGENTS.md"];

/// One harness declared in the config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    /// Canonical name the CLI prints and that `bootstrap write` takes.
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

#[derive(Clone)]
struct HarnessDraft {
    name: String,
    directory: String,
    /// `None` means the default (`AGENTS.md`); `Some([])` is the same after resolution.
    files: Option<Vec<String>>,
}

/// Parses `[harness.*]` from the config file. A missing file is an empty document: no harness is declared.
/// A bad table is [`Error::Config`].
///
/// The home directory is read only when a `directory` starts with `~`, so absolute paths need no user profile.
pub fn load_harnesses(config_path: &Path) -> Result<Vec<Harness>> {
    load_harnesses_inner(config_path, None)
}

/// [`load_harnesses`] with the home directory passed in (tests inject a temp folder).
pub fn load_harnesses_with_home(config_path: &Path, home: PathBuf) -> Result<Vec<Harness>> {
    load_harnesses_inner(config_path, Some(home))
}

fn load_harnesses_inner(config_path: &Path, home: Option<PathBuf>) -> Result<Vec<Harness>> {
    let doc = read_document(config_path)?;
    let drafts = merge_harnesses(config_path, &doc)?;
    drafts
        .into_iter()
        .map(|d| resolve_harness(config_path, home.as_deref(), d))
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

fn resolve_harness(config_path: &Path, home: Option<&Path>, draft: HarnessDraft) -> Result<Harness> {
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

fn resolve_directory(config_path: &Path, home: Option<&Path>, name: &str, raw: &str) -> Result<PathBuf> {
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
        let home = match home {
            Some(home) => home.to_path_buf(),
            None => home_dir()?,
        };
        return Ok(home.join(&rest[1..]));
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

/// The bootstrap block's markers, as found in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// No start or end marker line.
    None,
    /// One start line and one end line, the end after the start.
    Complete(BlockSpan),
    /// Any other marker arrangement.
    Misconfigured,
}

/// Where a complete block sits in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSpan {
    /// Byte offset of the start line's first byte.
    pub start: usize,
    /// Byte offset just past the end line and its line ending, or the end of the file.
    pub end: usize,
    /// The bytes between the two lines.
    body: Range<usize>,
    /// The 64 hex digits the start line carries.
    stamp: String,
}

impl BlockSpan {
    fn body<'a>(&self, content: &'a [u8]) -> &'a [u8] {
        &content[self.body.clone()]
    }
}

/// One line of a file: its bytes without the line ending, and the byte spans around it.
struct Line<'a> {
    /// First byte of the line.
    start: usize,
    /// First byte after the line's `\n`, or the end of the file.
    next: usize,
    /// The line without one trailing `\r`.
    text: &'a [u8],
}

struct Lines<'a> {
    content: &'a [u8],
    offset: usize,
}

fn lines(content: &[u8]) -> Lines<'_> {
    Lines { content, offset: 0 }
}

impl<'a> Iterator for Lines<'a> {
    type Item = Line<'a>;

    fn next(&mut self) -> Option<Line<'a>> {
        if self.offset >= self.content.len() {
            return None;
        }
        let start = self.offset;
        let newline = self.content[start..].iter().position(|&b| b == b'\n');
        let next = newline.map_or(self.content.len(), |i| start + i + 1);
        let mut text = &self.content[start..newline.map_or(self.content.len(), |i| start + i)];
        if text.last() == Some(&b'\r') {
            text = &text[..text.len() - 1];
        }
        self.offset = next;
        Some(Line { start, next, text })
    }
}

/// Scans the marker lines of one instruction file.
pub fn block(content: &[u8]) -> Block {
    let mut starts: Vec<(usize, usize, String)> = Vec::new();
    let mut ends: Vec<(usize, usize)> = Vec::new();
    for line in lines(content) {
        if let Some(stamp) = parse_start_line(line.text) {
            starts.push((line.start, line.next, stamp));
        } else if line.text == END_MARKER {
            ends.push((line.start, line.next));
        }
    }
    if starts.is_empty() && ends.is_empty() {
        return Block::None;
    }
    if starts.len() != 1 || ends.len() != 1 {
        return Block::Misconfigured;
    }
    let Some((start, body_start, stamp)) = starts.pop() else {
        return Block::Misconfigured;
    };
    let Some((body_end, end)) = ends.pop() else {
        return Block::Misconfigured;
    };
    if body_end <= start || body_start > body_end || end < body_end {
        return Block::Misconfigured;
    }
    Block::Complete(BlockSpan {
        start,
        end,
        body: body_start..body_end,
        stamp,
    })
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

/// Whether `content` holds a line that [`block`] would read as a start or end marker.
pub fn has_marker_line(content: &[u8]) -> bool {
    lines(content).any(|line| parse_start_line(line.text).is_some() || line.text == END_MARKER)
}

/// The bytes written between the marker lines: `payload` with a final newline when it has none.
fn block_body(payload: &[u8]) -> Vec<u8> {
    let mut body = payload.to_vec();
    if !body.ends_with(b"\n") {
        body.push(b'\n');
    }
    body
}

/// The two marker lines around `payload`, ending with a newline.
///
/// The sha is the SHA-256 of the bytes written between the lines. A payload without a final newline gets one, and
/// that newline is hashed too, so `classify_bytes` reads the block as up to date.
pub fn render_block(payload: &[u8]) -> Vec<u8> {
    let body = block_body(payload);
    let mut out = Vec::with_capacity(body.len() + START_PREFIX.len() + START_SUFFIX.len() + END_MARKER.len() + 67);
    out.extend_from_slice(START_PREFIX);
    out.extend_from_slice(sha256_hex(&body).as_bytes());
    out.extend_from_slice(START_SUFFIX);
    out.push(b'\n');
    out.extend_from_slice(&body);
    out.extend_from_slice(END_MARKER);
    out.push(b'\n');
    out
}

/// `content` with the block appended. A newline goes between the two when `content` does not end with one.
pub fn append_block(content: &[u8], payload: &[u8]) -> Vec<u8> {
    let block = render_block(payload);
    let mut out = Vec::with_capacity(content.len() + block.len() + 1);
    out.extend_from_slice(content);
    if !content.is_empty() && !content.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(&block);
    out
}

/// `content` with the complete block at `span` replaced. The text before and after keeps its bytes.
pub fn replace_block(content: &[u8], span: &BlockSpan, payload: &[u8]) -> Vec<u8> {
    let block = render_block(payload);
    let mut out = Vec::with_capacity(content.len() + block.len());
    out.extend_from_slice(&content[..span.start]);
    out.extend_from_slice(&block);
    out.extend_from_slice(&content[span.end..]);
    out
}

/// `content` with the complete block at `span` deleted. The text before and after keeps its bytes.
pub fn remove_block(content: &[u8], span: &BlockSpan) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() - (span.end - span.start));
    out.extend_from_slice(&content[..span.start]);
    out.extend_from_slice(&content[span.end..]);
    out
}

/// Classifies the raw bytes of one instruction file against the compiled payload.
pub fn classify_bytes(content: &[u8], payload: &[u8]) -> FileState {
    match block(content) {
        Block::None => FileState::BlockAbsent,
        Block::Misconfigured => FileState::Misconfigured,
        Block::Complete(span) => {
            let body_hash = sha256_hex(span.body(content));
            let stamp_ok = span.stamp.eq_ignore_ascii_case(&body_hash);
            let payload_ok = body_hash == sha256_hex(&block_body(payload));
            if stamp_ok && payload_ok {
                FileState::UpToDate
            } else {
                FileState::OutOfDate
            }
        }
    }
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
            let base = std::env::temp_dir().join(format!("recognitor-harness-unit-{}-{n}", std::process::id()));
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

    #[test]
    fn block_span_covers_the_whole_lines() {
        let payload = b"body\n";
        let wrapped = render_block(payload);
        let file = [b"before\n".as_slice(), &wrapped, b"after\n".as_slice()].concat();
        let Block::Complete(span) = block(&file) else {
            panic!("expected a complete block");
        };
        assert_eq!(span.start, 7);
        assert_eq!(span.end, 7 + wrapped.len());
        assert_eq!(span.body(&file), payload);
        let replaced = replace_block(&file, &span, b"new\n");
        assert_eq!(
            replaced,
            [
                b"before\n".as_slice(),
                render_block(b"new\n").as_slice(),
                b"after\n".as_slice()
            ]
            .concat()
        );
    }

    #[test]
    fn remove_block_keeps_the_text_around_it() {
        let wrapped = render_block(b"body\n");
        let file = [b"before\n".as_slice(), &wrapped, b"after\n".as_slice()].concat();
        let Block::Complete(span) = block(&file) else {
            panic!("expected a complete block");
        };
        assert_eq!(remove_block(&file, &span), b"before\nafter\n");
    }

    #[test]
    fn a_block_at_the_end_without_a_final_newline_is_complete() {
        let mut file = render_block(b"body\n");
        file.pop();
        let Block::Complete(span) = block(&file) else {
            panic!("expected a complete block");
        };
        assert_eq!(span.end, file.len());
        assert_eq!(classify_bytes(&file, b"body\n"), FileState::UpToDate);
    }

    #[test]
    fn append_covers_empty_and_unterminated_files() {
        let block = render_block(b"body\n");
        assert_eq!(append_block(b"", b"body\n"), block);
        assert_eq!(
            append_block(b"before", b"body\n"),
            [b"before\n".as_slice(), &block].concat()
        );
        assert_eq!(
            append_block(b"before\n", b"body\n"),
            [b"before\n".as_slice(), &block].concat()
        );
    }

    #[test]
    fn a_payload_with_a_marker_line_is_detected() {
        assert!(has_marker_line(b"hello\n<!-- recognitor bootstrap end -->\n"));
        assert!(has_marker_line(&render_block(b"x\n")));
        assert!(!has_marker_line(b"hello\n<!-- recognitor bootstrap nope -->\n"));
    }

    #[test]
    fn a_payload_without_a_newline_still_classifies_as_up_to_date() {
        let block = render_block(b"body");
        assert_eq!(classify_bytes(&block, b"body"), FileState::UpToDate);
    }
}

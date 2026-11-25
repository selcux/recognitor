// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor doctor` (doc 04 §5.3, doc 07 §12.5): the read-only check of the bootstrap block in harness
//! instruction files.

use std::fs;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::harness::{FileState, InstalledFile, classify_bytes};
use crate::index::Index;

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
            out.push_str(&format!("recognitor bootstrap write {}\n", write_names.join(" ")));
        }
        out
    }
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
            "a +\nb -\nc ?\nd \u{00d7}\ne \u{2191}\n---\nrecognitor bootstrap write b e\n"
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

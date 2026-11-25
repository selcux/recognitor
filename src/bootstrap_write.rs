// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor bootstrap write` (doc 04 §5.3, doc 07 §12.6): insert or replace the bootstrap block in the
//! instruction files of installed harnesses.
//!
//! Every named file is read and checked before the first edit. A missing file, a file with one marker line, an
//! unknown harness name or a directory that is not a directory stops the command with no file changed.

use std::fs;
use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::harness::{
    Block, FileState, Harness, InstalledFile, append_block, block, classify_bytes, has_marker_line, replace_block,
};
use crate::index::Index;

/// One instruction file read and checked before any edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub harness: String,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub block: Block,
}

/// The harnesses named on the command line, once each, in the order given.
///
/// A name matches a harness case-insensitively. A name the config does not declare is an error.
pub fn select(harnesses: &[Harness], names: &[String]) -> Result<Vec<Harness>> {
    let mut selected: Vec<Harness> = Vec::new();
    for name in names {
        let Some(found) = harnesses.iter().find(|h| h.name.eq_ignore_ascii_case(name)) else {
            return Err(Error::UnknownHarness {
                name: name.clone(),
                harnesses: harnesses.iter().map(|h| h.name.clone()).collect(),
            });
        };
        if selected.iter().all(|h| h.name != found.name) {
            selected.push(found.clone());
        }
    }
    Ok(selected)
}

/// Reads every installed instruction file and refuses the ones `write` and `remove` cannot touch.
///
/// The payload is not needed here: a missing file and a block with one marker line are errors however the payload
/// turns out. A file with no block, a complete block, or an up-to-date block is a target.
pub fn read_targets(installed: &[InstalledFile]) -> Result<Vec<Target>> {
    let mut targets = Vec::with_capacity(installed.len());
    for (harness, path, _) in installed {
        let meta = match fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::InstructionFile {
                    path: path.clone(),
                    message: "the instruction file is missing".to_string(),
                });
            }
            Err(e) => return Err(Error::io(path, e)),
            Ok(meta) => meta,
        };
        if meta.is_dir() {
            return Err(Error::InstructionFile {
                path: path.clone(),
                message: format!("{} is a directory; expected an instruction file", path.display()),
            });
        }
        if !meta.is_file() {
            return Err(Error::InstructionFile {
                path: path.clone(),
                message: format!("{} is not a file", path.display()),
            });
        }
        let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
        let block = block(&bytes);
        if matches!(block, Block::Misconfigured) {
            return Err(Error::InstructionFile {
                path: path.clone(),
                message: "the bootstrap block is misconfigured; the file was not changed. Fix it by hand.".to_string(),
            });
        }
        targets.push(Target {
            harness: harness.clone(),
            path: path.clone(),
            bytes,
            block,
        });
    }
    Ok(targets)
}

/// Writes the compiled payload into each target that needs it and returns the files that changed.
///
/// The payload is compiled once. A payload that itself contains a marker line is refused, because the block it
/// would build could not be read back.
pub fn write(index: &mut Index, targets: &[Target], max_tokens: usize) -> Result<Vec<PathBuf>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let payload = index.bootstrap()?.render(max_tokens);
    let payload = payload.as_bytes();
    if has_marker_line(payload) {
        return Err(Error::PayloadHasMarker);
    }

    let mut written = Vec::new();
    for target in targets {
        let new_bytes = match classify_bytes(&target.bytes, payload) {
            FileState::UpToDate => continue,
            FileState::BlockAbsent => append_block(&target.bytes, payload),
            FileState::OutOfDate => match &target.block {
                Block::Complete(span) => replace_block(&target.bytes, span, payload),
                // `read_targets` refuses both of these before the payload is compiled.
                Block::None | Block::Misconfigured => continue,
            },
            // `read_targets` read every file, so neither state can occur here.
            FileState::Misconfigured | FileState::Missing => continue,
        };
        crate::write::replace_file(&target.path, &new_bytes).map_err(|e| Error::io(&target.path, e))?;
        written.push(target.path.clone());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness(name: &str) -> Harness {
        Harness {
            name: name.to_string(),
            directory: PathBuf::from("/tmp").join(name),
            files: vec!["AGENTS.md".to_string()],
        }
    }

    #[test]
    fn select_matches_case_insensitively_and_drops_duplicates() {
        let harnesses = vec![harness("claude"), harness("codex")];
        let selected = select(
            &harnesses,
            &["Codex".to_string(), "CLAUDE".to_string(), "codex".to_string()],
        )
        .unwrap();
        assert_eq!(
            selected.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["codex", "claude"]
        );
    }

    #[test]
    fn an_unknown_harness_lists_the_ones_that_exist() {
        let harnesses = vec![harness("claude"), harness("codex")];
        let err = select(&harnesses, &["nope".to_string()]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("no harness named \"nope\""), "{text}");
        assert!(text.contains("claude, codex"), "{text}");
    }

    #[test]
    fn an_unknown_harness_with_an_empty_config_says_so() {
        let err = select(&[], &["nope".to_string()]).unwrap_err();
        assert!(err.to_string().contains("declares no harnesses"), "{err}");
    }
}

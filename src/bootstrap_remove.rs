// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor bootstrap remove` (doc 04 §5.3, doc 07 §12.7): strip the bootstrap block from the instruction files of
//! installed harnesses.
//!
//! The checks are the ones `bootstrap write` makes: every named file is read before the first edit, and a missing file
//! or a block with one marker line stops the command with no file changed. A file with no block is left alone. The
//! payload is never compiled, so this command does not open the vault.

use crate::bootstrap_write::Target;
use crate::error::{Error, Result};
use crate::harness::{Block, remove_block};

/// Removes the complete block from each target that has one and returns the targets that changed, in input order.
pub fn remove(targets: &[Target]) -> Result<Vec<&Target>> {
    let mut removed = Vec::new();
    for target in targets {
        let Block::Complete(span) = &target.block else {
            continue;
        };
        let bytes = remove_block(&target.bytes, span);
        crate::write::replace_file(&target.path, &bytes).map_err(|e| Error::io(&target.path, e))?;
        removed.push(target);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{block, render_block};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("recognitor-bootstrap-remove-unit-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn target(harness: &str, path: PathBuf, bytes: Vec<u8>) -> Target {
        let block = block(&bytes);
        Target {
            harness: harness.to_string(),
            path,
            bytes,
            block,
        }
    }

    #[test]
    fn a_complete_block_is_removed_and_a_file_without_one_is_skipped() {
        let dir = scratch();
        let with_block = dir.join("with.md");
        let without_block = dir.join("without.md");
        let content = [b"before\n".as_slice(), &render_block(b"body\n"), b"after\n".as_slice()].concat();
        fs::write(&with_block, &content).unwrap();
        fs::write(&without_block, b"mine\n").unwrap();

        let targets = vec![
            target("with", with_block.clone(), content),
            target("without", without_block.clone(), b"mine\n".to_vec()),
        ];
        let removed = remove(&targets).unwrap();
        assert_eq!(removed.iter().map(|t| t.harness.as_str()).collect::<Vec<_>>(), ["with"]);
        assert_eq!(fs::read(&with_block).unwrap(), b"before\nafter\n");
        assert_eq!(fs::read(&without_block).unwrap(), b"mine\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_misconfigured_target_is_not_touched() {
        let dir = scratch();
        let path = dir.join("broken.md");
        let bytes = b"<!-- recognitor bootstrap end -->\n".to_vec();
        fs::write(&path, &bytes).unwrap();
        let targets = vec![target("broken", path.clone(), bytes)];
        assert!(remove(&targets).unwrap().is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"<!-- recognitor bootstrap end -->\n");
        let _ = fs::remove_dir_all(&dir);
    }
}

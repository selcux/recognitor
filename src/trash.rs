// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! `recognitor trash empty` (doc 07 §10, §12.4): removes `.recognitor/trash`.
//!
//! The folder is hidden, so the index never contains it and this function does not open the database. Success prints
//! nothing, so the returned text is empty.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use crate::{Error, Result};

/// Removes `.recognitor/trash` under `vault`.
///
/// A missing trash directory is already clear: that is a normal result. The directory itself goes with its contents
/// (`remove_dir_all`), which on Windows opens the directory without following a symlink or junction. A trash path that
/// is itself a link, or that does not canonicalize to `<vault>/.recognitor/trash`, is left in place.
pub fn empty(vault: &Path) -> Result<String> {
    clear(vault)?;
    Ok(String::new())
}

fn clear(vault: &Path) -> Result<()> {
    let trash = vault.join(".recognitor").join("trash");
    let meta = match fs::symlink_metadata(&trash) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(&trash, e)),
    };
    if meta.file_type().is_symlink() {
        return Err(refuse(&trash));
    }
    if !meta.is_dir() {
        return Err(Error::io(
            &trash,
            io::Error::new(io::ErrorKind::InvalidInput, "not a directory"),
        ));
    }
    let canon = fs::canonicalize(&trash).map_err(|e| Error::io(&trash, e))?;
    let vault_canon = fs::canonicalize(vault).map_err(|e| Error::io(vault, e))?;
    let mut expected = vault_canon;
    expected.push(".recognitor");
    expected.push("trash");
    if !same_dir(&canon, &expected) {
        return Err(refuse(&trash));
    }
    // `remove_dir_all` opens the directory with `FILE_FLAG_OPEN_REPARSE_POINT` on Windows and does not follow a
    // symlink or junction. Doc 07 §5.1's write lock is not taken: nothing in the docs says this command holds it.
    fs::remove_dir_all(&trash).map_err(|e| Error::io(&trash, e))
}

fn refuse(trash: &Path) -> Error {
    Error::io(
        trash,
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to follow a link out of .recognitor/trash",
        ),
    )
}

/// `canonicalize` on Windows keeps the drive letter's casing from the volume, so the comparison ignores ASCII case
/// there. Elsewhere the path is the bytes it has.
fn same_dir(a: &Path, b: &Path) -> bool {
    let mut a = a.components();
    let mut b = b.components();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if component_eq(x.as_os_str(), y.as_os_str()) => {}
            _ => return false,
        }
    }
}

fn component_eq(a: &OsStr, b: &OsStr) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

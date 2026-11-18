//! The file half of the write path (doc 07 §5.1 step 4): new bytes go to a temporary file next to the note, which is
//! then renamed over it. A reader sees the old file or the new one, never half of one.

use std::fs;
use std::io;
use std::path::Path;

/// Replaces the file at `path` with `bytes`. The temporary file is hidden (a leading `.`) and does not end in `.md`, so
/// neither the sweep nor an editor picks it up; it is removed when the rename fails.
pub(crate) fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.recognitor-tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

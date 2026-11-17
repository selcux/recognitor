//! Note ids, namespaces, vault-relative path arithmetic, and the database location (doc 03 §4, doc 05 §5).
//!
//! Ids and stored paths always use `/` as the separator, on every platform.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::hash::sha256_prefix;

/// Reserved namespace: loaded at session start, never searched (doc 04 §2).
pub const BOOTSTRAP_NAMESPACE: &str = "bootstrap";
/// Reserved namespace: full-text only, graph-silent (doc 04 §4).
pub const ARCHIVE_NAMESPACE: &str = "archive";
/// Environment variable that overrides the database path (doc 05 §5).
pub const DB_ENV: &str = "RECOGNITOR_DB";

/// Note id from a vault-relative path: the path without `.md`, in any case (doc 03 §4).
pub fn note_id(rel_path: &str) -> &str {
    strip_md_ignore_case(rel_path).unwrap_or(rel_path)
}

/// Note key from a vault-relative path: the id in Rust Unicode lowercase (doc 03 §4). Names are ASCII, so for notes
/// the lowercasing is the same on every platform.
pub fn note_key(rel_path: &str) -> String {
    note_id(rel_path).to_lowercase()
}

/// Namespace of a note key: its top-level folder. `None` for a key without a folder: files directly in the vault
/// root are not notes (doc 03 §4).
pub fn namespace_of_key(key: &str) -> Option<&str> {
    key.split_once('/').map(|(ns, _)| ns)
}

/// Filename without `.md`: the last segment of an id or key.
pub fn file_stem(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Folder of a note id or key, `""` for the vault root.
pub fn parent_dir(id: &str) -> &str {
    match id.rfind('/') {
        Some(i) => &id[..i],
        None => "",
    }
}

/// Joins `rel` onto the vault folder `base_dir` and resolves `.` and `..`.
/// Returns `None` when the result leaves the vault root.
pub fn join_normalized(base_dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Strips a trailing `.md`, ignoring case. `None` when the path has no `.md` suffix.
pub fn strip_md_ignore_case(path: &str) -> Option<&str> {
    let n = path.len();
    if n >= 3 && path.is_char_boundary(n - 3) && path[n - 3..].eq_ignore_ascii_case(".md") {
        Some(&path[..n - 3])
    } else {
        None
    }
}

/// Decodes `%XX` escapes. Invalid escapes stay as written; invalid UTF-8 is replaced.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Canonical absolute vault root, symlinks resolved. Fails when it is not a directory.
pub fn canonical_vault_root(path: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(path).map_err(|e| Error::io(path, e))?;
    if !canonical.is_dir() {
        return Err(Error::Vault(format!("{} is not a directory", path.display())));
    }
    Ok(canonical)
}

/// The canonical path as text, without the Windows `\\?\` prefix that `canonicalize` adds.
pub fn display_path(canonical: &Path) -> String {
    let s = canonical.to_string_lossy();
    // `\\?\UNC\server\share` is the verbatim form of `\\server\share`.
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s.into_owned(),
    }
}

/// True when two vault root paths from [`display_path`] name the same folder. On Windows paths compare without regard
/// to case, as in the vault id.
pub fn same_vault_root(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// `<vault-id>`: first 16 hex characters of the SHA-256 of the canonical vault root path. On Windows the path is
/// lowercased and has no `\\?\` prefix (doc 05 §5). `display` must come from [`display_path`].
pub fn vault_id(display: &str, windows: bool) -> String {
    if windows {
        sha256_prefix(display.to_lowercase().as_bytes(), 16)
    } else {
        sha256_prefix(display.as_bytes(), 16)
    }
}

/// Database path for a vault (doc 05 §5): `RECOGNITOR_DB` when set and non-empty, else
/// `<per-user cache dir>/recognitor/.../<vault-id>.db`.
pub fn database_path(canonical_root: &Path, env_override: Option<OsString>) -> Result<PathBuf> {
    if let Some(p) = env_override.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let id = vault_id(&display_path(canonical_root), cfg!(windows));
    let dirs = directories::ProjectDirs::from_path(PathBuf::from("recognitor")).ok_or(Error::NoCacheDir)?;
    Ok(dirs.cache_dir().join(format!("{id}.db")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_key_namespace_stem() {
        let rel = "Common/working-style/ADHD profile.md";
        let id = note_id(rel);
        assert_eq!(id, "Common/working-style/ADHD profile");
        let key = note_key(rel);
        assert_eq!(key, "common/working-style/adhd profile");
        assert_eq!(namespace_of_key(&key), Some("common"));
        assert_eq!(file_stem(&key), "adhd profile");
        assert_eq!(parent_dir(&key), "common/working-style");
        // The extension matches in any case (doc 03 §4).
        assert_eq!(note_key("a/Note.MD"), "a/note");
        // A file in the vault root has no namespace.
        assert_eq!(namespace_of_key("readme"), None);
        assert_eq!(parent_dir("readme"), "");
    }

    #[test]
    fn join_resolves_dots_and_refuses_escape() {
        assert_eq!(
            join_normalized("backend", "../ops/Deploy Checklist.md").as_deref(),
            Some("ops/Deploy Checklist.md")
        );
        assert_eq!(join_normalized("a/b", "./c.md").as_deref(), Some("a/b/c.md"));
        assert_eq!(join_normalized("", "x.md").as_deref(), Some("x.md"));
        assert_eq!(join_normalized("a", "../../x.md"), None);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("Deploy%20Checklist.md"), "Deploy Checklist.md");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        assert_eq!(percent_decode("%C3%A7"), "ç");
    }

    #[test]
    fn md_suffix() {
        assert_eq!(strip_md_ignore_case("a/b.md"), Some("a/b"));
        assert_eq!(strip_md_ignore_case("a/b.MD"), Some("a/b"));
        assert_eq!(strip_md_ignore_case("a/b.png"), None);
    }

    #[test]
    fn vault_id_is_16_hex_and_lowercased_on_windows() {
        let a = vault_id(r"C:\Users\Me\Vault", true);
        let b = vault_id(r"c:\users\me\vault", true);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(vault_id("/home/Me/Vault", false), vault_id("/home/me/vault", false));
    }

    #[test]
    fn display_path_strips_verbatim_prefix() {
        assert_eq!(display_path(Path::new(r"\\?\C:\Vault")), r"C:\Vault");
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share\vault")),
            r"\\server\share\vault"
        );
        assert_eq!(display_path(Path::new("/home/me/vault")), "/home/me/vault");
    }

    #[test]
    fn env_override_wins() {
        let p = database_path(Path::new("/x"), Some(OsString::from("/tmp/custom.db"))).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/custom.db"));
    }

    #[test]
    fn default_db_path_ends_with_vault_id() {
        let p = database_path(Path::new(r"C:\Vault"), None).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name.len(), 16 + 3);
        assert!(name.ends_with(".db"));
        if cfg!(windows) {
            let s = p.to_string_lossy();
            assert!(s.ends_with(&format!(r"recognitor\cache\{name}")), "{s}");
        }
    }
}

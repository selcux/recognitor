//! The config file and the vault and database settings that come from it (doc 07 §12 "Which vault" and "Config
//! file", doc 05 §5).
//!
//! The file is `config.toml` in the per-user local config folder. Keys follow the `section.name` style and are
//! stored as `[section]` tables with a string value. Only two keys exist so far: [`KEYS`].

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::error::{Error, Result};

/// Environment variable with the vault root (doc 07 §12).
pub const VAULT_ENV: &str = "RECOGNITOR_VAULT";
/// Environment variable that replaces the config file path. Not in the docs: a seam so that tests, and anyone who
/// needs a second setup, never touch the per-user file.
pub const CONFIG_ENV: &str = "RECOGNITOR_CONFIG";

pub const VAULT_PATH_KEY: &str = "vault.path";
pub const DB_PATH_KEY: &str = "db.path";
/// The config keys that exist so far.
pub const KEYS: [&str; 2] = [VAULT_PATH_KEY, DB_PATH_KEY];

/// Where the config file lives: [`CONFIG_ENV`] when set and non-empty, else `config.toml` in
/// `ProjectDirs::config_local_dir()` (doc 07 §12).
pub fn config_path() -> Result<PathBuf> {
    config_path_from(std::env::var_os(CONFIG_ENV))
}

/// [`config_path`] with the environment value passed in.
pub fn config_path_from(env_override: Option<OsString>) -> Result<PathBuf> {
    if let Some(p) = env_override.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let dirs = directories::ProjectDirs::from_path(PathBuf::from("recognitor")).ok_or(Error::NoConfigDir)?;
    Ok(dirs.config_local_dir().join("config.toml"))
}

fn known(key: &str) -> Result<(&str, &str)> {
    if !KEYS.contains(&key) {
        return Err(Error::UnknownConfigKey(key.to_string()));
    }
    // Every known key is `section.name`.
    key.split_once('.')
        .ok_or_else(|| Error::UnknownConfigKey(key.to_string()))
}

/// The file as an editable document; a missing file is an empty document. A file that does not parse is an error and
/// is never overwritten. The document keeps comments and formatting, so a `set` changes only its own value.
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

/// The value of a known key, `None` when the file does not set it.
pub fn get(path: &Path, key: &str) -> Result<Option<String>> {
    let (section, name) = known(key)?;
    let doc = read_document(path)?;
    match doc.get(section).and_then(Item::as_table_like).and_then(|s| s.get(name)) {
        None => Ok(None),
        Some(item) => match item.as_str() {
            Some(s) => Ok(Some(s.to_string())),
            None => Err(Error::Config {
                path: path.to_path_buf(),
                message: format!("{key} must be a string"),
            }),
        },
    }
}

/// Sets a known key, keeping everything else in the file as it is: other keys, comments and formatting. Creates the
/// file and its folder when they are missing. `vault.path` must name an existing folder; it is stored as given.
pub fn set(path: &Path, key: &str, value: &str) -> Result<()> {
    let (section, name) = known(key)?;
    if key == VAULT_PATH_KEY && !Path::new(value).is_dir() {
        return Err(Error::InvalidConfigValue {
            key: key.to_string(),
            reason: format!("{value} does not exist or is not a folder"),
        });
    }
    let bad_file = |message: String| Error::Config {
        path: path.to_path_buf(),
        message,
    };
    let mut doc = read_document(path)?;
    if !doc.contains_key(section) {
        doc.insert(section, toml_edit::table());
    }
    let Some(section_table) = doc.get_mut(section).and_then(Item::as_table_like_mut) else {
        return Err(bad_file(format!("[{section}] must be a table")));
    };
    let mut new_value = toml_edit::Value::from(value);
    // Keep the spacing and the comment around the old value.
    if let Some(old) = section_table.get(name).and_then(Item::as_value) {
        *new_value.decor_mut() = old.decor().clone();
    }
    // An existing key is changed in place, so its own spacing stays as well; `insert` would write a fresh key.
    match section_table.get_mut(name) {
        Some(item) => *item = Item::Value(new_value),
        None => {
            section_table.insert(name, Item::Value(new_value));
        }
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    fs::write(path, doc.to_string()).map_err(|e| Error::io(path, e))
}

/// The vault root (doc 07 §12 "Which vault"): the flag, else [`VAULT_ENV`], else `vault.path` in the config file.
/// With none of them set it fails with an error that names all three. The current folder is never used.
pub fn resolve_vault(flag: Option<PathBuf>, env: Option<OsString>, config_file: &Path) -> Result<PathBuf> {
    if let Some(p) = flag {
        return Ok(p);
    }
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    match get(config_file, VAULT_PATH_KEY)?.filter(|p| !p.is_empty()) {
        Some(p) => Ok(PathBuf::from(p)),
        None => Err(Error::NoVault {
            config: config_file.to_path_buf(),
        }),
    }
}

/// The explicit database path (doc 05 §5): `RECOGNITOR_DB`, else `db.path` in the config file, else `None` for the
/// default place in the cache folder.
pub fn resolve_db(env: Option<OsString>, config_file: &Path) -> Result<Option<PathBuf>> {
    if let Some(p) = env.filter(|p| !p.is_empty()) {
        return Ok(Some(PathBuf::from(p)));
    }
    Ok(get(config_file, DB_PATH_KEY)?
        .filter(|p| !p.is_empty())
        .map(PathBuf::from))
}

//! One error type for the library. The CLI prints it with `Display`.

use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    /// A filesystem call failed on a known path.
    Io { path: PathBuf, source: io::Error },
    /// SQLite returned an error.
    Sql(rusqlite::Error),
    /// The vault root is missing or is not a directory.
    Vault(String),
    /// The OS gave no per-user config directory (doc 07 §12), and `RECOGNITOR_CONFIG` is not set.
    NoConfigDir,
    /// No vault from the flag, `RECOGNITOR_VAULT` or the config file (doc 07 §12 "Which vault").
    NoVault { config: PathBuf },
    /// `config get` or `config set` with a key that does not exist.
    UnknownConfigKey(String),
    /// `config set` with a value the key cannot take.
    InvalidConfigValue { key: String, reason: String },
    /// `config get` of a key the file does not set.
    ConfigNotSet(String),
    /// The config file cannot be read as TOML or holds the wrong kind of value. It is never overwritten.
    Config { path: PathBuf, message: String },
    /// A search query with no words in it (doc 07 §4, "Query text").
    EmptyQuery,
    /// The OS gave no per-user cache directory (doc 05 §5), and `RECOGNITOR_DB` is not set.
    NoCacheDir,
    /// The database file is not a Recognitor database. Nothing was changed.
    ForeignDatabase(PathBuf),
    /// The database belongs to another vault (doc 05 §5: one database per vault). Nothing was changed.
    VaultMismatch {
        db: PathBuf,
        stored: String,
        current: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// A mistake in how the command was called, as opposed to a failure while running it: the CLI exits with 2.
    pub fn is_usage(&self) -> bool {
        matches!(
            self,
            Error::NoVault { .. } | Error::UnknownConfigKey(_) | Error::InvalidConfigValue { .. }
        )
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Sql(e) => write!(f, "database: {e}"),
            Error::Vault(msg) => write!(f, "vault: {msg}"),
            Error::NoConfigDir => write!(f, "no per-user config directory found; set RECOGNITOR_CONFIG"),
            Error::NoVault { config } => write!(
                f,
                "no vault given: pass --vault <path>, set RECOGNITOR_VAULT, or set vault.path in the config file \
                 ({}; `recognitor config set vault.path <path>`)",
                config.display()
            ),
            Error::UnknownConfigKey(key) => {
                write!(
                    f,
                    "unknown config key {key:?}; the keys are: {}",
                    crate::config::KEYS.join(", ")
                )
            }
            Error::InvalidConfigValue { key, reason } => write!(f, "{key}: {reason}"),
            Error::ConfigNotSet(key) => write!(f, "{key} is not set"),
            Error::Config { path, message } => write!(f, "{}: {message}", path.display()),
            // PLACEHOLDER wording: doc 07 §4 says only "an error that says so".
            Error::EmptyQuery => write!(f, "the search query is empty: give at least one word to search for"),
            Error::NoCacheDir => write!(f, "no per-user cache directory found; set RECOGNITOR_DB"),
            Error::ForeignDatabase(db) => write!(
                f,
                "{} is not a Recognitor database; refusing to change it (delete it or set RECOGNITOR_DB elsewhere)",
                db.display()
            ),
            Error::VaultMismatch { db, stored, current } => write!(
                f,
                "{} belongs to the vault {stored}, not {current}; refusing to change it (one database per vault)",
                db.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Sql(e) => Some(e),
            Error::Vault(_)
            | Error::EmptyQuery
            | Error::NoCacheDir
            | Error::NoConfigDir
            | Error::NoVault { .. }
            | Error::UnknownConfigKey(_)
            | Error::InvalidConfigValue { .. }
            | Error::ConfigNotSet(_)
            | Error::Config { .. }
            | Error::ForeignDatabase(_)
            | Error::VaultMismatch { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sql(e)
    }
}

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
            Error::Vault(_) | Error::NoCacheDir | Error::ForeignDatabase(_) | Error::VaultMismatch { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sql(e)
    }
}

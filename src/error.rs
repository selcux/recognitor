// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

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
    /// A note named in a tool call matches no note (doc 07 §6.3). Holds the name as given.
    NoteNotFound(String),
    /// `relink`'s `old` is not a note id with its folder (doc 07 §6.3: `old` is the id the note had). Holds it as
    /// given.
    OldNotAnId(String),
    /// `read_note` with a `section` that names no heading of the note (doc 07 §7.3, "No match"). `note` is the id as
    /// doc 07 §4.2 shows it; `sections` are the note's top-level sections.
    NoSection {
        note: String,
        name: String,
        sections: Vec<String>,
    },
    /// `load_bootstrap` found no `bootstrap/index.md` in the index: no folder, no file, or a case twin that was left
    /// out (doc 04 §2.2, "No entry point"). Its text is the one error line of the doc, without the `error: ` prefix
    /// that the CLI adds.
    NoBootstrap,
    /// The OS gave no per-user cache directory (doc 05 §5), and `RECOGNITOR_DB` is not set.
    NoCacheDir,
    /// The database file is not a Recognitor database. Nothing was changed.
    ForeignDatabase(PathBuf),
    /// `load_namespace("bootstrap")` (doc 07 §1.1): the bootstrap loads with `load_bootstrap`.
    BootstrapNotANamespace,
    /// `load_namespace` of a namespace with no note in the index (doc 07 §3, "No such namespace"). Holds the name as
    /// given and the namespaces that exist, as their folders are spelled, sorted by name ignoring case.
    NoNamespace { name: String, namespaces: Vec<String> },
    /// A write was refused and nothing changed (doc 07 §5.3). Holds the text of the refusal, which names the reason and
    /// returns the current data; the CLI prints it as the error, as an MCP tool returns it as one.
    Refused(String),
    /// The MCP server could not start or lost its transport. Holds the SDK's message.
    Mcp(String),
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
            // Doc 07 §4 "Query text": a query with no words left is refused with an error that says so.
            Error::EmptyQuery => write!(f, "the search query is empty: give at least one word to search for"),
            Error::NoBootstrap => write!(f, "bootstrap/index.md not found. Run `recognitor lint`."),
            // Doc 07 §6.3: `not found`. The name is quoted as in the "matches N notes" line.
            Error::NoteNotFound(name) => write!(f, "\"{name}\" not found"),
            Error::OldNotAnId(old) => write!(
                f,
                "\"{old}\" is not a note id: old needs the id the note had before the move, with its folder"
            ),
            Error::NoSection { note, name, sections } => {
                let listed: Vec<String> = sections.iter().map(|s| format!("\"{s}\"")).collect();
                if listed.is_empty() {
                    // Not in doc 07 §7.3: a note with no sections has none to list.
                    write!(f, "No section \"{name}\" in {note}. The note has no sections.")
                } else {
                    write!(f, "No section \"{name}\" in {note}. Sections: {}.", listed.join(", "))
                }
            }
            // Doc 07 §3, "`bootstrap` is refused".
            Error::BootstrapNotANamespace => write!(
                f,
                "bootstrap is not loaded with load_namespace: call load_bootstrap for the standing instructions"
            ),
            Error::NoNamespace { name, namespaces } => {
                write!(f, "No namespace \"{name}\".")?;
                if !namespaces.is_empty() {
                    let listed: Vec<String> = namespaces.iter().map(|n| format!("\"{n}\"")).collect();
                    write!(f, " Namespaces: {}.", listed.join(", "))?;
                }
                Ok(())
            }
            Error::Refused(text) => write!(f, "{text}"),
            Error::Mcp(msg) => write!(f, "mcp: {msg}"),
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
            | Error::NoteNotFound(_)
            | Error::OldNotAnId(_)
            | Error::NoSection { .. }
            | Error::NoBootstrap
            | Error::Refused(_)
            | Error::Mcp(_)
            | Error::BootstrapNotANamespace
            | Error::NoNamespace { .. }
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

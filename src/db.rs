//! SQLite schema and connection setup (doc 05 §2, §4). Vectors (doc 05 §2.5) are not part of shadow mode.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};

/// `PRAGMA user_version` of this binary's schema (doc 05 §4.3). A database with another version is dropped and
/// rebuilt.
pub const SCHEMA_VERSION: i64 = 4;

/// DDL from doc 05 §2.1-2.4 and §2.6 (comments left out). §2.5 (`notes_vec`) is deferred.
pub const SCHEMA: &str = r"
CREATE TABLE notes (
    pk INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    namespace TEXT NOT NULL,
    type TEXT NOT NULL DEFAULT 'note',
    title TEXT NOT NULL,
    title_key TEXT NOT NULL,
    name_key TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    summary TEXT,
    checksum TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL,
    content TEXT NOT NULL
);

CREATE INDEX idx_notes_namespace ON notes(namespace);
CREATE INDEX idx_notes_type ON notes(type);
CREATE INDEX idx_notes_mtime ON notes(mtime);
CREATE INDEX idx_notes_title ON notes(title_key);
CREATE INDEX idx_notes_name ON notes(name_key);
CREATE INDEX idx_notes_sweep ON notes(key, path, title, checksum, mtime, size);

CREATE TABLE edges (
    source_key TEXT NOT NULL REFERENCES notes(key) ON DELETE CASCADE,
    predicate TEXT NOT NULL,
    target_ref TEXT NOT NULL,
    ref_key TEXT NOT NULL,
    target_key TEXT,
    is_inline INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL,
    context TEXT,
    PRIMARY KEY (source_key, predicate, ref_key)
) WITHOUT ROWID;

CREATE INDEX idx_edges_target ON edges(target_key, predicate);
CREATE INDEX idx_edges_ref ON edges(ref_key);

CREATE TRIGGER notes_edges_ad AFTER DELETE ON notes BEGIN
    UPDATE edges SET target_key = NULL WHERE target_key = old.key;
END;

CREATE TABLE observations (
    pk INTEGER PRIMARY KEY,
    note_key TEXT NOT NULL REFERENCES notes(key) ON DELETE CASCADE,
    category TEXT NOT NULL,
    content TEXT NOT NULL,
    qualifiers TEXT,
    tags TEXT,
    line_number INTEGER NOT NULL
);

CREATE INDEX idx_obs_note ON observations(note_key);

CREATE VIRTUAL TABLE observations_fts USING fts5(
    category,
    content,
    tags,
    content='observations',
    content_rowid='pk',
    tokenize='porter unicode61'
);

CREATE TRIGGER obs_ai AFTER INSERT ON observations BEGIN
    INSERT INTO observations_fts(rowid, category, content, tags)
    VALUES (new.pk, new.category, new.content, new.tags);
END;

CREATE TRIGGER obs_ad AFTER DELETE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, category, content, tags)
    VALUES('delete', old.pk, old.category, old.content, old.tags);
END;

CREATE TRIGGER obs_au AFTER UPDATE OF category, content, tags ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, category, content, tags)
    VALUES('delete', old.pk, old.category, old.content, old.tags);
    INSERT INTO observations_fts(rowid, category, content, tags)
    VALUES (new.pk, new.category, new.content, new.tags);
END;

CREATE VIRTUAL TABLE notes_fts USING fts5(
    title,
    summary,
    content,
    content='notes',
    content_rowid='pk',
    tokenize='porter unicode61'
);

CREATE TRIGGER notes_ai AFTER INSERT ON notes
WHEN new.namespace != 'bootstrap' BEGIN
    INSERT INTO notes_fts(rowid, title, summary, content)
    VALUES (new.pk, new.title, new.summary, new.content);
END;

CREATE TRIGGER notes_ad AFTER DELETE ON notes
WHEN old.namespace != 'bootstrap' BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, summary, content)
    VALUES('delete', old.pk, old.title, old.summary, old.content);
END;

CREATE TRIGGER notes_au AFTER UPDATE OF namespace, title, summary, content ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, summary, content)
    SELECT 'delete', old.pk, old.title, old.summary, old.content
    WHERE old.namespace != 'bootstrap';
    INSERT INTO notes_fts(rowid, title, summary, content)
    SELECT new.pk, new.title, new.summary, new.content
    WHERE new.namespace != 'bootstrap';
END;

CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
";

/// `meta` keys (doc 05 §2.6).
pub mod meta_keys {
    pub const VAULT_ROOT: &str = "vault_root";
    pub const LAST_SWEEP_STARTED_MS: &str = "last_sweep_started_ms";
    pub const LAST_SWEEP_FINISHED_MS: &str = "last_sweep_finished_ms";
}

/// Opens (or creates) the database and applies the per-connection PRAGMAs (doc 05 §4.1).
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaState {
    /// No tables yet.
    Fresh,
    /// A Recognitor database that matches [`SCHEMA_VERSION`].
    Current,
    /// A Recognitor database with another version: start over with a new file (doc 05 §4.3).
    Outdated(i64),
    /// Tables of another application, or a Recognitor database without `meta.vault_root`. Never touched.
    Foreign,
}

/// Reads the state of an open database. A database counts as Recognitor's when its `meta` table holds `vault_root`
/// (doc 05 §2.6); every committed sweep writes it, whatever the version.
pub fn schema_state(conn: &Connection) -> Result<SchemaState> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if version == 0 && tables == 0 {
        return Ok(SchemaState::Fresh);
    }
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
        [],
        |r| r.get(0),
    )?;
    let is_ours = has_meta && {
        let columns_ok = conn
            .prepare("SELECT key, value FROM meta LIMIT 0")
            .map(|_| true)
            .unwrap_or(false);
        columns_ok && meta_get(conn, meta_keys::VAULT_ROOT)?.is_some()
    };
    Ok(match (is_ours, version) {
        (false, _) => SchemaState::Foreign,
        (true, v) if v == SCHEMA_VERSION => SchemaState::Current,
        (true, v) => SchemaState::Outdated(v),
    })
}

/// Creates the schema and stamps the version.
pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

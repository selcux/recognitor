# Recognitor Database Schema Specification

This document defines the physical SQLite database schema for Recognitor. The database functions as a fast, disposable, rebuildable index over canonical local Markdown files.

---

## 1. Schema Design Principles

1. **Unified Relational Graph:** Notes, edges, and observations coexist in a single SQLite database across all namespaces, eliminating siloed databases and enabling cross-domain graph traversal.
2. **Soft Foreign Keys for Ghost Links:** `edges.source_key` strictly references `notes.key`. `edges.target_ref` keeps the link exactly as written, `edges.ref_key` holds its lowercase key for matching, and `edges.target_key` holds the resolved note key, or `NULL` while the target does not exist. References to uncreated notes are preserved as ghost links without violating database constraints, and heal automatically when the target note appears.
3. **Location Decides Treatment:** the reserved namespaces `bootstrap` and `archive` decide how notes are treated ([04-information-stores-and-lifecycle.md](04-information-stores-and-lifecycle.md)). Bootstrap notes share the same tables but never get full-text or vector rows, so search cannot return them. There is no tier column; `type` is free metadata. What each tool may do in each store is defined in [07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access).
4. **Surgical Granularity:** The `observations` table indexes individual bracketed bullets (`- [category] ...`) with an open text category, allowing agents to retrieve atomic facts without pulling whole files into prompt context.
5. **Disposable Projection:** The entire database can be deleted at any time and reconstructed deterministically via `recognitor rebuild`. Schema changes never need migrations (see §4.3).
6. **Native Vector Search (Optional):** Embedded `sqlite-vec` virtual tables store dense embeddings directly alongside relational data, enabling hybrid semantic search with zero external vector databases or daemons.
7. **Stable Row Keys:** Every table that a virtual table points at declares an explicit `INTEGER PRIMARY KEY`. Implicit `rowid` values are never used as join keys, because `VACUUM` may renumber them.

---

## 2. Core Tables DDL

### 2.1 The `notes` Table

Stores note metadata, extracted summaries, and body content.

```sql
CREATE TABLE notes (
    pk INTEGER PRIMARY KEY,                    -- Stable rowid alias; notes_fts and notes_vec rows key on this
    key TEXT NOT NULL UNIQUE,                  -- The note's id lowercased (e.g. "common/working-style/adhd profile"); edges and observations reference it; never read from frontmatter (doc 03 §4, doc 07 §6)
    namespace TEXT NOT NULL,                   -- Top-level folder, lowercased (e.g. "common", "compass"); reserved: "bootstrap", "archive"; files in the vault root are not notes
    type TEXT NOT NULL DEFAULT 'note',         -- Free frontmatter metadata (e.g. "profile", "decision", "spec", "note"); never decides loading; selects the type schema
    title TEXT NOT NULL,                       -- Human-readable title from H1 or frontmatter, as written
    title_key TEXT NOT NULL,                   -- Title lowercased; link resolution by title
    name_key TEXT NOT NULL,                    -- Filename without ".md", lowercased; link resolution by filename
    path TEXT NOT NULL UNIQUE,                 -- Path as written, relative to the vault root (e.g. "common/working-style/ADHD profile.md"); opens the file; output shows it without ".md" as the note's id
    summary TEXT,                              -- First paragraph synopsis for token-efficient graph traversal
    content TEXT NOT NULL,                     -- Raw Markdown content
    checksum TEXT NOT NULL,                    -- SHA-256 hash of file content for change detection
    mtime INTEGER NOT NULL,                    -- File modification timestamp (Unix epoch in milliseconds)
    size INTEGER NOT NULL                      -- File size in bytes; (mtime, size) is the change signal
);

CREATE INDEX idx_notes_namespace ON notes(namespace);
CREATE INDEX idx_notes_type ON notes(type);
CREATE INDEX idx_notes_mtime ON notes(mtime);
CREATE INDEX idx_notes_title ON notes(title_key);  -- Link resolution by title, in one namespace or the whole vault
CREATE INDEX idx_notes_name ON notes(name_key);    -- Link resolution by filename
```

- **Names are keys:** `key`, `namespace`, `title_key` and `name_key` hold lowercase keys made in Rust with Unicode lowercasing ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)). Query parameters are lowercased the same way before they reach SQL. `COLLATE NOCASE` is never used for names, because it folds only A to Z.
- **Spelling lives in `path` and `title`:** output, and opening the file, use them. A case-only rename keeps `key` and updates `path` (§4.2), so edges and observations are untouched.
- **Case twins and non-ASCII names never reach the database:** files that share a key, or whose path has a non-ASCII character, are left out, and nothing records them. So `key`, `namespace` and `name_key` are always ASCII. The sweep finds them again in the file list each time and reports them until one is renamed ([07-interface-contracts.md §6](07-interface-contracts.md#6-note-identity-and-moves)).

---

### 2.2 The `edges` Table

Stores directional relations between notes.

```sql
CREATE TABLE edges (
    source_key TEXT NOT NULL REFERENCES notes(key) ON DELETE CASCADE,
    predicate TEXT NOT NULL,                   -- e.g. "part_of", "has_part", "supersedes", "relates_to", "links_to"
    target_ref TEXT NOT NULL,                  -- Link exactly as written: "Security Architecture", "cognition::index", "../ops/Deploy%20Checklist.md"; output shows it for ghosts
    ref_key TEXT NOT NULL,                     -- The link's key for matching (doc 03 §4): "security architecture", "cognition::index", "backend/ops/deploy checklist"
    target_key TEXT,                           -- Resolved note key; NULL = ghost note
    is_inline INTEGER NOT NULL DEFAULT 0,      -- 0: explicit relation from ## Relations, 1: inline link
    position INTEGER NOT NULL,                 -- Source line of the link; keeps the written order for load_bootstrap()
    context TEXT,                              -- Trailing "(context)" of a ## Relations line, or surrounding snippet if inline
    PRIMARY KEY (source_key, predicate, ref_key)
) WITHOUT ROWID;

-- The primary key already serves outgoing lookups (source_key, then predicate)
CREATE INDEX idx_edges_target ON edges(target_key, predicate);   -- Backlinks, ghost discovery, and the delete trigger
CREATE INDEX idx_edges_ref ON edges(ref_key);                    -- Re-resolution after a note is added, removed or renamed

-- A deleted note turns every link pointing at it back into a ghost
CREATE TRIGGER notes_edges_ad AFTER DELETE ON notes BEGIN
    UPDATE edges SET target_key = NULL WHERE target_key = old.key;
END;
```

Resolution rules for `ref_key` → `target_key` are defined in [03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution). Re-resolution is targeted: after a note is added, removed, renamed or retitled, the engine re-resolves only the edges whose `ref_key` matches one of that note's names, old or new.

- **One row per link key:** a note that links the same target twice with the same predicate gets one row, even when the spelling differs (`[[Caching]]`, `[[caching]]`). Explicit relations are inserted before inline links with `ON CONFLICT DO NOTHING`, so the explicit row wins, and the first spelling written is kept.
- **Both directions are kept:** `A has_part B` and `B part_of A` are two rows, because `load_bootstrap()` needs links exactly as written. Graph queries treat such an inverse pair as one fact.

---

### 2.3 The `observations` Table

Stores atomic facts, rules, and decisions extracted from bracketed bullet lists. Bootstrap and archive notes store no observations: bootstrap notes are loaded whole, and archive notes are graph-silent.

```sql
CREATE TABLE observations (
    pk INTEGER PRIMARY KEY,                    -- Stable rowid alias; observations_fts rows key on this
    note_key TEXT NOT NULL REFERENCES notes(key) ON DELETE CASCADE,
    category TEXT NOT NULL,                    -- Open vocabulary, stored lowercase: "fact", "rule", "avoid", "decision", "goal", etc.
    content TEXT NOT NULL,                     -- Observation text without the category bracket and qualifier
    qualifiers TEXT,                           -- Raw qualifier text: "@effective:2026-11-11", "@valid[2026-01-01,2026-07-01)"; parsed when date-aware queries are built
    tags TEXT,                                 -- Trailing #tags, space-separated, without "#"
    line_number INTEGER NOT NULL               -- Source line number in Markdown file
);

CREATE INDEX idx_obs_note ON observations(note_key);
```

#### Observation Full-Text Search (`observations_fts`)

Searches individual observations by their text, so agents can retrieve one fact without the whole note.

```sql
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
```

---

### 2.4 The Full-Text Search Table (`notes_fts`)

SQLite FTS5 virtual table for keyword search across titles, summaries, and content using an external content table to minimize database footprint. Bootstrap notes are never indexed. Archive notes are indexed, and the search query decides when they match (§3.7).

```sql
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title,
    summary,
    content,
    content='notes',
    content_rowid='pk',
    tokenize='porter unicode61'
);

-- Triggers keep FTS5 synchronized with the notes table, skipping bootstrap notes
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

-- Fires only when indexed text or the namespace changes; mtime/size/checksum-only updates skip it.
-- Each half checks its own side, because an FTS5 'delete' must match a row that was actually indexed.
CREATE TRIGGER notes_au AFTER UPDATE OF namespace, title, summary, content ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, summary, content)
    SELECT 'delete', old.pk, old.title, old.summary, old.content
    WHERE old.namespace != 'bootstrap';
    INSERT INTO notes_fts(rowid, title, summary, content)
    SELECT new.pk, new.title, new.summary, new.content
    WHERE new.namespace != 'bootstrap';
END;
```

---

### 2.5 The Vector Search Table (`notes_vec`)

SQLite virtual table using `sqlite-vec` (`vec0`, ≥ 0.1.6) for dense embedding storage and cosine similarity search. Rows key on `notes.pk`. Bootstrap notes never get vectors.

`namespace` is declared as a `vec0` **metadata column**, so the scope filter runs inside the nearest-neighbor search. Filtering in a `JOIN` after the search would cut the top `k` results down, possibly to zero rows. The indexer writes the column whenever it writes a note's vector, and rewrites it when the note's namespace changes.

> **Optional, deferred (DR1-04):** this table exists only when semantic search is enabled. The embedding model, runtime, schedule, and chunking are decided when the feature is built. `float[384]` is a placeholder; the dimension follows the chosen model.

```sql
CREATE VIRTUAL TABLE notes_vec USING vec0(
    note_pk integer primary key,               -- = notes.pk
    embedding float[384] distance_metric=cosine,
    namespace text                             -- Metadata column; excludes archive inside the KNN search
);

-- Trigger to clean up vector embeddings when a note is deleted
CREATE TRIGGER notes_vec_ad AFTER DELETE ON notes BEGIN
    DELETE FROM notes_vec WHERE note_pk = old.pk;
END;
```

---

### 2.6 The `meta` Table

Small key-value table for engine state that must be shared between processes (CLI invocations and concurrent MCP servers).

```sql
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- Keys:
--   vault_root               Canonical absolute vault root path
--   last_sweep_started_ms    Unix ms when the last sync sweep started (racy-file detection)
--   last_sweep_finished_ms   Unix ms when the last sync sweep committed (TTL gate)
```
---

## 3. Core Query Patterns

### 3.1 Bootstrap Walk Step (`load_bootstrap()`)

The walk itself runs in Rust with a visited set, depth-first from `bootstrap/index.md` ([04-information-stores-and-lifecycle.md §2.2](04-information-stores-and-lifecycle.md)). A recursive SQL query cannot keep one visited set across branches. Each step asks for a note's outgoing links in written order:

```sql
SELECT e.target_key
FROM edges e
JOIN notes n ON n.key = e.target_key
WHERE e.source_key = ?1
  AND e.is_inline = 0                          -- ## Relations links only
  AND e.predicate != 'supersedes'              -- never load a retired version
  AND n.namespace = 'bootstrap'                -- never leave the bootstrap; ghosts drop out of the JOIN
ORDER BY e.position;
```

### 3.2 Namespace Entry (`load_namespace(ns)`)

```sql
SELECT title, content
FROM notes
WHERE key = ?1 || '/index';                     -- ?1 is the namespace key; matches index.md in any case
```

### 3.3 Two-Stage Graph Discovery
Returns outgoing edges without loading full note content. Links into the bootstrap are not returned:

```sql
SELECT
    e.predicate,
    e.target_ref,
    n.path,                                    -- The id as written; NULL for a ghost, which shows target_ref
    n.namespace,
    n.title,
    n.summary,
    CASE WHEN e.target_key IS NULL THEN 1 ELSE 0 END AS is_ghost
FROM edges e
LEFT JOIN notes n ON e.target_key = n.key
WHERE e.source_key = ?1
  AND (n.namespace IS NULL OR n.namespace != 'bootstrap')
ORDER BY e.position;                           -- Written order (doc 07 §8)
```

### 3.4 Backlink Discovery (Incoming References)
Finds all notes referencing a target note across knowledge namespaces:

```sql
SELECT
    e.predicate,
    n.path,                                    -- The id as written
    n.namespace,
    n.title,
    n.summary
FROM edges e
JOIN notes n ON e.source_key = n.key
WHERE e.target_key = ?1
  AND n.namespace != 'bootstrap'
ORDER BY n.mtime DESC;                         -- Most recently changed first (doc 07 §8)
```

### 3.5 Ghost Notes Discovery (Unwritten Concept Backlog)
Surfaces unwritten notes ranked by inbound reference count:

```sql
SELECT
    MIN(e.target_ref) AS name,                 -- One spelling, for display
    COUNT(*) AS mention_count,
    GROUP_CONCAT(DISTINCT e.predicate) AS predicates
FROM edges e
WHERE e.target_key IS NULL
GROUP BY e.ref_key                             -- [[Gall's law]] and [[Gall's Law]] are one ghost
ORDER BY mention_count DESC;
```

The "maybe moved to" hint ([07-interface-contracts.md §6.2](07-interface-contracts.md#62-moves-outside-recognitor)) looks up the last part of each ghost's `ref_key` (after the last `/` or `::`) in `notes.name_key`, and shows a hint only when exactly one note matches.

### 3.6 Hybrid Semantic and Metadata Search
Finds semantically similar notes across the whole vault (optional feature). The namespace is a ranking perspective, not a filter ([07-interface-contracts.md §4.1](07-interface-contracts.md#41-ranking)), so the only filter is the archive rule ([07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access)). It sits on a `vec0` metadata column, so it applies during the search, not after it (DR1-05). When `ns` is `archive`, the query runs without the archive line; a plain `!=` keeps the filter inside the nearest-neighbor search. The query fetches more candidates than a result page holds, because the `ns` boost reorders them afterwards:

```sql
SELECT
    n.path,
    n.namespace,
    n.title,
    n.summary,
    v.distance
FROM notes_vec v
JOIN notes n ON n.pk = v.note_pk
WHERE v.embedding MATCH ?1
  AND k = 50                                   -- candidates; the ns boost and paging pick the final 10
  AND v.namespace != 'archive'                 -- dropped when ns is archive
ORDER BY v.distance ASC;
```

### 3.7 Full-Text Search (`search`)
Ranks notes by BM25. Like §3.6, it fetches candidates, and the `ns` boost and paging pick the final page ([07-interface-contracts.md §4.1](07-interface-contracts.md#41-ranking)). The archive condition applies the store rules ([07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access)). Bootstrap notes have no full-text rows (§2.4), so they need no condition:

```sql
SELECT
    n.path,
    n.namespace,
    n.title,
    n.summary,
    bm25(notes_fts) AS rank
FROM notes_fts
JOIN notes n ON n.pk = notes_fts.rowid
WHERE notes_fts MATCH ?1
  AND (n.namespace != 'archive' OR ?2 = 'archive')  -- ?2 is the ns key; NULL without ns
ORDER BY rank                                        -- bm25(): lower is better
LIMIT 50;                                            -- candidates; the ns boost and paging pick the final 10
```

The observation lines under each hit come from `observations_fts`. Bootstrap and archive notes have no observations (§2.3), so that query needs no store condition.

---

## 4. Connection, Write & Versioning Invariants

### 4.1 Per-Connection PRAGMAs

Every connection, CLI or MCP server, applies these on open:

```sql
PRAGMA foreign_keys = ON;       -- Off by default in SQLite; ON DELETE CASCADE does nothing without it
PRAGMA journal_mode = WAL;      -- Readers never block the single writer
PRAGMA busy_timeout = 5000;     -- Wait up to 5 s for another process's write lock instead of failing
PRAGMA synchronous = NORMAL;    -- Safe with WAL; the database is rebuildable anyway
```

### 4.2 Write Rules

- **Upsert only:** notes are written with `INSERT ... ON CONFLICT(key) DO UPDATE`, and the update sets every column of §2.1 except `pk` and `key`, so a retitle refreshes `title_key` too. This is the only place the write is specified. A case-only rename hits the same `key` and updates `path`. `INSERT OR REPLACE` and `REPLACE` are forbidden: a replace deletes the old row without firing delete triggers (unless `recursive_triggers` is on), which leaves stale `notes_fts` rows and orphaned `notes_vec` rows.
- **Stable keys:** `notes_fts` and `notes_vec` reference `notes.pk`, and `observations_fts` references `observations.pk`, never an implicit `rowid`.
- **Edges:** inserted with `ON CONFLICT DO NOTHING`, explicit relations before inline links (§2.2).
- **One writer per sweep:** a sync sweep runs inside `BEGIN IMMEDIATE`, so concurrent processes queue instead of indexing the same files twice.

### 4.3 Schema Versioning

`PRAGMA user_version` stores the engine's schema version. When a binary opens a database with a different version, it drops all tables and runs `rebuild`. Because the database is a projection of the Markdown files, schema changes need no migrations.

---

## 5. Database File Location

The database never lives inside the vault. Cloud sync tools and git would copy its WAL files half-written, which corrupts SQLite. Instead, each user gets one database per vault, in the operating system's per-user cache directory. That directory is the platform's slot for data that can be rebuilt, and it sits on local disk (WAL mode does not work on network shares).

| Platform | Database path |
| :--- | :--- |
| **Windows** | `%LOCALAPPDATA%\recognitor\cache\<vault-id>.db` (Local, not Roaming: Roaming copies the profile between machines) |
| **Linux** | `$XDG_CACHE_HOME/recognitor/<vault-id>.db`, falling back to `~/.cache/recognitor/<vault-id>.db` |
| **macOS** | `~/Library/Caches/recognitor/<vault-id>.db` |

- **`<vault-id>`:** first 16 hex characters of the SHA-256 of the canonical absolute vault root path (symlinks resolved; on Windows, lowercased and without the `\\?\` prefix). Two vaults never share a database, and two users never share a directory.
- **Override:** the `RECOGNITOR_DB` environment variable or `db.path` in the config file sets an explicit path. Config keys use one style: `section.name` (`db.path`, `read.max_tokens`, `bootstrap.max_tokens`).
- **Implementation hint:** the `directories` crate (`ProjectDirs::cache_dir()`) resolves these paths per platform.

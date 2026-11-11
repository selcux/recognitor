# Recognitor Database Schema Specification

This document defines the physical SQLite database schema for Recognitor. The database functions as a fast, disposable, rebuildable index over canonical local Markdown files.

---

## 1. Schema Design Principles

1. **Unified Relational Graph:** Notes, edges, and observations coexist in a single SQLite database across all namespaces, eliminating siloed databases and enabling cross-domain graph traversal.
2. **Soft Foreign Keys for Ghost Links:** `edges.source_id` strictly references `notes.id`, but `edges.target_id` is a soft reference. References to uncreated notes are preserved as ghost links without violating database constraints.
3. **Pacing Tiers as First-Class Citizens:** Notes store their information tier (1 to 5) directly, defaulting to Tier 4 (Domain Knowledge PKM) for unclassified notes.
4. **Surgical Granularity:** The `observations` table indexes individual bracketed bullets (`- [category] ...`) with an open text category, allowing agents to retrieve atomic facts without pulling whole files into prompt context.
5. **Disposable Projection:** The entire database can be deleted at any time and reconstructed deterministically via `recognitor rebuild`.

---

## 2. Core Tables DDL

### 2.1 The `notes` Table

Stores note metadata, extracted summaries, and body content.

```sql
CREATE TABLE notes (
    id TEXT PRIMARY KEY,                       -- Canonical slug: "<namespace>/<slug>" (e.g. "backend/auth-flow")
    namespace TEXT NOT NULL,                   -- Logical domain namespace (e.g. "backend", "cognition", "personal/health")
    type TEXT NOT NULL DEFAULT 'note',         -- Frontmatter type (e.g. "profile", "decision", "spec", "note")
    tier INTEGER NOT NULL DEFAULT 4,           -- Information pacing tier (1: Profile, 2: Frame, 3: Operational, 4: PKM, 5: Archive)
    title TEXT NOT NULL,                       -- Human-readable title from H1 or frontmatter
    path TEXT NOT NULL UNIQUE,                 -- Relative filesystem path from root (e.g. "backend/auth-flow.md")
    summary TEXT,                              -- First paragraph synopsis for token-efficient graph traversal
    content TEXT NOT NULL,                     -- Raw Markdown content
    checksum TEXT NOT NULL,                    -- SHA-256 hash of file content for change detection
    mtime INTEGER NOT NULL                     -- File modification timestamp (Unix epoch in milliseconds)
);

CREATE INDEX idx_notes_namespace ON notes(namespace);
CREATE INDEX idx_notes_type ON notes(type);
CREATE INDEX idx_notes_tier ON notes(tier);
CREATE INDEX idx_notes_mtime ON notes(mtime);
```

---

### 2.2 The `edges` Table

Stores directional relations between notes.

```sql
CREATE TABLE edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    target_id TEXT NOT NULL,                   -- Soft reference to allow unwritten ghost notes
    predicate TEXT NOT NULL,                   -- e.g. "part_of", "has_part", "supersedes", "relates_to", "links_to"
    is_inline INTEGER NOT NULL DEFAULT 0,      -- 0: explicit relation from ## Relations, 1: inline [[wikilink]]
    context TEXT,                              -- Optional surrounding snippet if inline
    UNIQUE(source_id, target_id, predicate)
);

CREATE INDEX idx_edges_source ON edges(source_id);
CREATE INDEX idx_edges_target ON edges(target_id);
CREATE INDEX idx_edges_predicate ON edges(predicate);
CREATE INDEX idx_edges_lookup ON edges(source_id, predicate);
```

---

### 2.3 The `observations` Table

Stores atomic facts, rules, and decisions extracted from bracketed bullet lists.

```sql
CREATE TABLE observations (
    id TEXT PRIMARY KEY,                       -- Format: "<note_id>:<line_number>"
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    category TEXT NOT NULL,                    -- Open vocabulary: "fact", "rule", "avoid", "decision", "goal", etc.
    content TEXT NOT NULL,                     -- Observation text without the category bracket
    qualifiers TEXT,                           -- Optional qualifiers: "@effective:2026-11-11", "@valid:...", etc.
    line_number INTEGER NOT NULL               -- Source line number in Markdown file
);

CREATE INDEX idx_obs_note ON observations(note_id);
CREATE INDEX idx_obs_category ON observations(category);
```

---

### 2.4 The Full-Text Search Table (`notes_fts`)

SQLite FTS5 virtual table for keyword search across titles, summaries, and content using an external content table to minimize database footprint.

```sql
CREATE VIRTUAL TABLE notes_fts USING fts5(
    title,
    summary,
    content,
    content='notes',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

-- Triggers to keep FTS5 synchronized with the notes table
CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, title, summary, content) 
    VALUES (new.rowid, new.title, new.summary, new.content);
END;

CREATE TRIGGER notes_ad AFTER DELETE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, summary, content) 
    VALUES('delete', old.rowid, old.title, old.summary, old.content);
END;

CREATE TRIGGER notes_au AFTER UPDATE ON notes BEGIN
    INSERT INTO notes_fts(notes_fts, rowid, title, summary, content) 
    VALUES('delete', old.rowid, old.title, old.summary, old.content);
    INSERT INTO notes_fts(rowid, title, summary, content) 
    VALUES (new.rowid, new.title, new.summary, new.content);
END;
```

---

## 3. Core Query Patterns

### 3.1 One-Shot Bootstrap Retrieval
Hydrates Tier 1 and Tier 2 context for the `recognitor.get_bootstrap()` tool:

```sql
SELECT id, type, title, content 
FROM notes 
WHERE tier IN (1, 2) 
  AND namespace = 'common'
ORDER BY tier ASC;
```

### 3.2 Two-Stage Graph Discovery
Returns outgoing edges without loading full note content:

```sql
SELECT 
    e.predicate,
    e.target_id,
    n.namespace,
    n.title,
    n.summary,
    CASE WHEN n.id IS NULL THEN 1 ELSE 0 END AS is_ghost
FROM edges e
LEFT JOIN notes n ON e.target_id = n.id
WHERE e.source_id = ?1;
```

### 3.3 Backlink Discovery (Incoming References)
Finds all notes referencing a target note across any namespace:

```sql
SELECT 
    e.predicate,
    e.source_id,
    n.namespace,
    n.title,
    n.summary
FROM edges e
JOIN notes n ON e.source_id = n.id
WHERE e.target_id = ?1;
```

### 3.4 Ghost Notes Discovery (Unwritten Concept Backlog)
Surfaces unwritten notes ranked by inbound reference count:

```sql
SELECT 
    e.target_id,
    COUNT(*) AS mention_count,
    GROUP_CONCAT(DISTINCT e.predicate) AS predicates
FROM edges e
LEFT JOIN notes n ON e.target_id = n.id
WHERE n.id IS NULL
GROUP BY e.target_id
ORDER BY mention_count DESC;
```

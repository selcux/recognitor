# Recognitor Architecture Overview

## 1. System Vision & Purpose

Recognitor is a local-first Personal Knowledge Management (PKM) memory management system designed specifically for Large Language Models (LLMs) and autonomous agents.

While tools like Basic Memory demonstrated the value of combining local Markdown files with an MCP interface and graph traversal, they introduce structural bottlenecks when scaling across multiple domains. Recognitor addresses these bottlenecks by redesigning the storage model, edge semantics, and language runtime from the ground up while preserving human-readable plain text notes.

---

## 2. Core Differences from Basic Memory

| Capability | Basic Memory | Recognitor |
| :--- | :--- | :--- |
| **Storage Architecture** | Physically separate SQLite database per project. | Single unified SQLite engine with domain namespaces. |
| **Cross-Domain Traversal** | Broken at the database level. `[[project::note]]` is an unindexed reference. | Native. Relational edges connect nodes across namespaces in a single database. |
| **Domain Isolation** | Hard silo on disk. | Logical namespaces (`work`, `personal`, `research`) filtered by default at query time. |
| **Graph Edge Semantics** | Section-based `## Relations` with list items (`- type [[target]]`). | First-class edge relations with typed predicates, weights, and context. |
| **Graph Algorithms** | None. Manual BFS in Python application code. | Recursive SQL CTEs for hierarchies, and in-memory `petgraph` algorithms (PageRank, Shortest Paths) in Rust. |
| **Vector Search** | Basic/separate. | Built-in vector search via `sqlite-vec` or Rust-native HNSW indices. |
| **Runtime & Latency** | Python MCP server (subject to interpreter cold start). | Pure Rust embedded engine (`rusqlite`) with sub-millisecond startup. |

---

## 3. Storage Architecture: Unified Graph with Namespaces

### The Silo Bottleneck in Existing Systems
In Basic Memory, isolating domains requires creating distinct projects. Because each project is a separate SQLite file on disk, joins, referential integrity, and graph queries cannot cross project boundaries.

### The Recognitor Model
Recognitor adopts a **Single Unified Graph with Logical Namespaces** powered by an embedded SQLite engine paired with Rust graph algorithms:

```
+-------------------------------------------------------------+
|                SQLite Relational-Graph-Vector               |
|                                                             |
|  [Namespace: coding]               [Namespace: personal]    |
|   +---------------+                 +------------------+    |
|   | Note: Auth    |-------edge----->| Note: Sleep      |    |
|   +---------------+ (affects_focus) +------------------+    |
|           |                                 |               |
|         edge                              edge              |
|           v                                 v               |
|   +---------------+                 +------------------+    |
|   | Note: Tokens  |                 | Note: Routine    |    |
|   +---------------+                 +------------------+    |
+-------------------------------------------------------------+
```

1. **Nodes Table:** Contains all notes across all domains. Each node has a `namespace`, disk `path`, checksum, title, permalink, mtime, and frontmatter.
2. **Edges Table:** Explicit relational edges (`source_id`, `target_id`, `predicate`, `weight`, `context`). Foreign keys reference `nodes(id)`. Edges connect nodes across namespaces with zero boundary friction.
3. **Query Scoping:**
   - **Default Scope:** When an agent works inside a specific context, queries filter `namespace = 'coding'`.
   - **Traversable Scope:** Relational queries can walk edges across namespace boundaries using recursive CTEs or via in-memory graph expansion in Rust, with configurable depth limits or hop-decay weights.

---

## 4. Source of Truth: File-First Projection

Recognitor uses a **file-first** architecture:

1. **Disk is Canonical:** Plain Markdown files on the local filesystem are the ultimate source of truth.
2. **Database is a Fast Index:** SQLite is a disposable, rebuildable projection of the Markdown files. If the database file is deleted, `recognitor rebuild` reconstructs the entire graph, full-text index, and vector embeddings directly from the files.
3. **Bidirectional Safety:** Edits made by human hands in editors like Obsidian are detected via file modification timestamps and hashes, triggering incremental re-indexing.

---

## 5. Technology Stack Selection

### Database Layer: SQLite + petgraph + sqlite-vec (Embedded in Rust)
- **Why SQLite (`rusqlite`):**
  - Universal, zero-maintenance embedded engine with sub-millisecond execution.
  - Zero external daemons, zero network stack, and native cross-platform support across Windows, macOS, and Linux.
  - ACID transactional consistency for batch index updates.
- **Why `petgraph` in Rust:**
  - Complex graph algorithms (PageRank, personalized walks, shortest paths, centrality) run directly in Rust memory in microseconds over SQLite-backed edge lists.
  - Replaces external graph engine dependencies with a battle-tested pure Rust data structure library.
- **Why `sqlite-vec`:**
  - Modern, fast vector search extension written in C with SIMD acceleration, embedding directly into SQLite.
  - Keeps vectors, metadata, and graph edges inside the same SQLite file.

### Language & Runtime Layer: Rust
- **Why Rust:**
  - Sub-millisecond cold start for CLI commands and MCP daemon operations.
  - Distribution as a single static binary without Python virtual environments, runtime dependencies, or Docker containers.
  - High-throughput Markdown parsers (e.g. `pulldown-cmark`) feeding directly into `rusqlite`.

---

## 6. SQLite Schema Definition

```sql
-- Notes table
CREATE TABLE nodes (
    id TEXT PRIMARY KEY,
    namespace TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    permalink TEXT NOT NULL,
    checksum TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    raw_frontmatter TEXT
);

CREATE INDEX idx_nodes_namespace ON nodes(namespace);
CREATE INDEX idx_nodes_permalink ON nodes(permalink);

-- Relational edges table
CREATE TABLE edges (
    source_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    predicate TEXT NOT NULL,
    weight REAL DEFAULT 1.0,
    context TEXT,
    PRIMARY KEY (source_id, target_id, predicate)
);

CREATE INDEX idx_edges_source ON edges(source_id);
CREATE INDEX idx_edges_target ON edges(target_id);
CREATE INDEX idx_edges_predicate ON edges(predicate);

-- Full-Text Search index (FTS5)
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    title,
    content,
    content='nodes',
    content_rowid='rowid'
);

-- Vector Search index (sqlite-vec)
CREATE VIRTUAL TABLE vec_nodes USING vec0(
    id TEXT PRIMARY KEY,
    embedding float[384] distance_metric=cosine
);
```

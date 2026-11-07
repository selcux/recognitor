# Recognitor Architecture Overview

## 1. System Vision & Purpose

Recognitor is a local-first Personal Knowledge Management (PKM) memory management system designed specifically for Large Language Models (LLMs) and autonomous agents.

While tools like Basic Memory demonstrated the value of combining local Markdown files with an MCP interface and graph traversal, they introduce structural bottlenecks when scaling across multiple domains. Recognitor addresses these bottlenecks by redesigning the storage model, edge semantics, and language runtime from the ground up while preserving human-readable plain text notes.

---

## 2. Core Differences from Basic Memory

| Capability | Basic Memory | Recognitor |
| :--- | :--- | :--- |
| **Storage Architecture** | Physically separate SQLite database per project. | Single unified CozoDB engine with domain namespaces. |
| **Cross-Domain Traversal** | Broken at the database level. `[[project::note]]` is an unindexed reference. | Native. Relational edges connect nodes across namespaces and are traversable in single Datalog queries. |
| **Domain Isolation** | Hard silo on disk. | Logical namespaces (`work`, `personal`, `research`) filtered by default at query time. |
| **Graph Edge Semantics** | Section-based `## Relations` with list items (`- type [[target]]`). | First-class edge relations with typed predicates, weights, and context. |
| **Graph Algorithms** | None. Manual BFS in Python application code. | Built-in PageRank, Shortest Paths, and recursive Datalog traversals. |
| **Vector Search** | Basic/separate. | Built-in HNSW vector index integrated with graph queries. |
| **Runtime & Latency** | Python MCP server (subject to interpreter cold start). | Pure Rust embedded engine (`cozo` crate) with sub-millisecond startup. |

---

## 3. Storage Architecture: Unified Graph with Namespaces

### The Silo Bottleneck in Existing Systems
In Basic Memory, isolating domains requires creating distinct projects. Because each project is a separate SQLite file on disk, joins, referential integrity, and graph queries cannot cross project boundaries.

### The Recognitor Model
Recognitor adopts a **Single Unified Graph with Logical Namespaces** powered by CozoDB:

```
+-------------------------------------------------------------+
|                CozoDB Relational-Graph-Vector               |
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

1. **Nodes Relation:** Contains all notes across all domains. Each node has a `namespace`, disk `path`, checksum, frontmatter, and dense vector embedding.
2. **Edges Relation:** Explicit relational edges (`source_id`, `target_id`, `predicate`, `weight`, `context`). Edges connect nodes across namespaces with zero boundary friction.
3. **Query Scoping:**
   - **Default Scope:** When an agent works inside a specific context, queries filter `namespace == 'coding'`.
   - **Traversable Scope:** When traversing relations, Datalog rules walk edges outward regardless of namespace boundaries, with optional hop-decay weights.

---

## 4. Source of Truth: File-First Projection

Recognitor uses a **file-first** architecture:

1. **Disk is Canonical:** Plain Markdown files on the local filesystem are the ultimate source of truth.
2. **Database is a Fast Index:** CozoDB is a disposable, rebuildable projection of the Markdown files. If the database file is deleted, `recognitor rebuild` reconstructs the entire graph, full-text index, and vector embeddings directly from the files.
3. **Bidirectional Safety:** Edits made by human hands in editors like Obsidian are detected via file modification timestamps and hashes, triggering incremental re-indexing.

---

## 5. Technology Stack Selection

### Database Layer: CozoDB (Rust Native)
- **Why CozoDB:**
  - Advertised as the "Hippocampus for AI", CozoDB unifies relational storage, graph algorithms, and vector search in a single embedded engine.
  - Built-in **PageRank** and **Shortest Path** algorithms solve the fan-out problem deterministically.
  - Native **HNSW** vector index allows vector similarity search and graph edge expansion inside the same query.
  - Written in pure Rust, running in-process with zero network latency.
  - Storage durability backed by RocksDB or SQLite under the hood.

### Language & Runtime Layer: Rust
- **Why Rust:**
  - Sub-millisecond cold start for CLI commands and MCP daemon operations.
  - Distribution as a single static binary without Python virtual environments or runtime dependencies.
  - High-throughput Markdown parsers (e.g. `pulldown-cmark`) feeding directly into the embedded `cozo` crate.

---

## 6. CozoDB Schema Definition (CozoScript)

```datalog
% Stored relation for notes / nodes
:create nodes {
    id: String =>
    namespace: String,
    path: String,
    title: String,
    permalink: String,
    checksum: String,
    mtime: Int,
    raw_frontmatter: Json,
    embedding: <F32; 384>
}

% Stored relation for typed graph edges
:create edges {
    source_id: String,
    target_id: String,
    predicate: String =>
    weight: Float,
    context: String
}

% HNSW vector index for semantic retrieval
::hnsw create nodes:vec_idx {
    dim: 384,
    dtype: F32,
    fields: [embedding],
    distance: Cosine,
    m: 32,
    ef_construction: 64
}

% Full-Text Search index
::fts create nodes:fts_idx {
    fields: [title]
}
```

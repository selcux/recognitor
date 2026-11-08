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

1. **Nodes:** Represents notes across domains, identified by unique IDs/paths and scoped by logical namespace.
2. **Edges:** Typed relational edges connecting nodes across namespaces with optional predicates and weights.
3. **Query Scoping:**
   - **Default Scope:** When an agent works inside a specific context, queries filter by active namespace.
   - **Traversable Scope:** Relational queries can traverse edges across namespace boundaries when following connections.

---

## 4. Source of Truth: File-First Projection

Recognitor uses a **file-first** architecture:

1. **Disk is Canonical:** Plain Markdown files on the local filesystem are the ultimate source of truth.
2. **Database is a Fast Index:** SQLite is a disposable, rebuildable projection of the Markdown files. If the database file is deleted, `recognitor rebuild` reconstructs the entire graph, full-text index, and vector embeddings directly from the files.
3. **Bidirectional Safety:** Edits made by human hands in editors like Obsidian are detected via file modification timestamps and hashes, triggering incremental re-indexing.

---

## 5. Technology Stack Selection

### Database Layer: SQLite + petgraph (Embedded in Rust)
- **SQLite (`rusqlite`):** Zero-maintenance, in-process embedded storage engine with sub-millisecond execution, zero network overhead, and universal platform support.
- **`petgraph`:** In-memory graph processing in pure Rust for complex graph traversals and ranking algorithms over edge lists loaded from SQLite.

### Language & Runtime Layer: Rust
- Sub-millisecond startup for CLI invocations and MCP operations.
- Single static binary distribution without runtime or daemon dependencies.

---

## 6. Storage Model & Schema Status

The concrete database schema is currently **to be defined** following alignment on:
1. Markdown edge notation syntax.
2. Metadata and frontmatter requirements.
3. Vector indexing integration strategy.

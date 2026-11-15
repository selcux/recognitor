# Recognitor Architecture Overview

## 1. System Vision & Purpose

Recognitor is a local-first Personal Knowledge Management (PKM) memory management system designed specifically for Large Language Models (LLMs) and autonomous agents.

Tools like Basic Memory demonstrated the value of combining local Markdown files with an MCP interface and graph traversal. Basic Memory serves as a reference, not a template: Recognitor keeps what works, and changes the format and mechanics where its own goals differ. Those goals are links that resolve across domains, a deterministic session start, and a native runtime, all while preserving human-readable plain-text notes.

---

## 2. Core Differences from Basic Memory

Basic Memory facts below were checked against v0.24 (October 2026) and a real local vault on 2026-11-15.

| Capability | Basic Memory (v0.24) | Recognitor |
| :--- | :--- | :--- |
| **Storage** | One SQLite database for all projects; notes scoped by `project_id`. | One SQLite database; notes scoped by namespace (top-level folder). |
| **Cross-Domain Links** | Search spans projects in one query, but links resolve only inside a project: `[[cognition::index]]` from another project stays unresolved. | Links resolve across namespaces, and graph walks follow them freely. A namespace is an entry point and a perspective, never a boundary. |
| **Session Start** | The agent follows multi-step reading instructions. | `load_bootstrap()` returns the whole bootstrap link tree in one call, or a session-start hook injects it. |
| **Edge Notation** | `## Relations` with typed predicates and context; inline links become `links_to`. | Same notation, used as a reference. Edge weights are computed at query time, never authored. |
| **Graph Algorithms** | Context building by depth-limited traversal (depth ≤ 5). | Recursive SQL walks; Personalized PageRank with hub dampening (resolved in principle). |
| **Vector Search** | Always on: FastEmbed embeddings in `sqlite-vec`, hybrid with full-text search. | Optional feature, added later (DR1-04); `sqlite-vec` storage. |
| **Sync** | File watcher. | On-demand sweep with a 2-second gate; no background process. |
| **Runtime** | Python MCP server and CLI. | Native Rust binary; no interpreter start-up. |

---

## 3. Storage Architecture: Unified Graph with Namespaces

### The Silo Bottleneck in Existing Systems
In Basic Memory, isolating domains requires creating distinct projects. All projects share one SQLite database, but a link resolves only inside its own project. In a real 8-project vault, the link `relates_to [[cognition::index]]` written in the `recognitor` project is stored with no target, so graph context stops at the project boundary.

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

1. **Nodes:** Represents notes across domains, identified by their path and grouped by logical namespace.
2. **Edges:** Typed relational edges connecting nodes across namespaces. Weights are computed at query time, never authored ([02-open-questions-and-tradeoffs.md §6](02-open-questions-and-tradeoffs.md)).
3. **Namespaces as Perspective:**
   - **Entry Point:** An agent enters the knowledge base through a namespace's `index.md` and passes that namespace to searches, which rank its notes higher without filtering out the rest.
   - **No Boundaries:** Searches and graph walks span the whole vault; the LLM decides when to follow links into other namespaces ([07-interface-contracts.md §1](07-interface-contracts.md#1-principles)).

---

## 4. Source of Truth: File-First Projection

Recognitor uses a **file-first** architecture:

1. **Disk is Canonical:** Plain Markdown files on the local filesystem are the ultimate source of truth.
2. **Database is a Fast Index:** SQLite is a disposable, rebuildable projection of the Markdown files. If the database file is deleted, `recognitor rebuild` reconstructs the entire graph, full-text index, and vector embeddings directly from the files.
3. **Bidirectional Safety:** Edits made by human hands in editors like Obsidian are detected via file modification timestamps and hashes, triggering incremental re-indexing.

---

## 5. Technology Stack Selection

### Database Layer: SQLite (Embedded in Rust)
- **SQLite (`rusqlite`):** Zero-maintenance, in-process embedded storage engine with no network round trip and universal platform support.
- **Graph algorithms:** plain walks run as SQL queries or short Rust loops. Ranking algorithms run in Rust over links loaded from the `edges` table; the library choice (e.g. `petgraph` or hand-written) is an implementation detail, made when such an algorithm is built ([02 §6](02-open-questions-and-tradeoffs.md)).

### Language & Runtime Layer: Rust
- No interpreter start-up: the CLI runs as a native process, and the MCP server stays resident for the whole session. Latency targets are set and measured by the benchmark (DR1-16).
- Single static binary distribution without runtime or daemon dependencies.

---

## 6. Information Architecture: Three Stores

Recognitor separates information by location, not by frontmatter `type`. Full specification in [04-information-stores-and-lifecycle.md](04-information-stores-and-lifecycle.md).

1. **Bootstrap (`bootstrap/`):** the user's profile, behavior, and global rules. `load_bootstrap()` loads it at session start as a link tree from `bootstrap/index.md`. Only linked notes load, so linking selects the active persona.
2. **Knowledge namespaces (every other top-level folder):** on-demand PKM. `load_namespace(ns)` returns the namespace's `index.md` and keeps no state; the agent passes `ns` to later searches as a ranking perspective. Everything else is reached through search and graph walks.
3. **Archive (`archive/`):** raw session records and verbatim transcripts, excluded from standard searches.

The two loading functions replace fragile multi-hop procedural file reads at session start.

---

## 7. Storage Model & Schema Status

- **Markdown format & edge syntax:** Resolved (see [03-markdown-format-and-edge-syntax.md](03-markdown-format-and-edge-syntax.md)).
- **Information stores & observation granularity:** Resolved (see [04-information-stores-and-lifecycle.md](04-information-stores-and-lifecycle.md)).
- **Database schema, full-text and vector indexing:** Resolved (see [05-database-schema-specification.md](05-database-schema-specification.md)). Vector search is an optional feature, added later.
- **Design reviews:** findings and their status are tracked in [06-design-reviews.md](06-design-reviews.md).
- **Interface contracts:** MCP tools and CLI commands, with inputs, outputs, budgets and conflict rules: [07-interface-contracts.md](07-interface-contracts.md).

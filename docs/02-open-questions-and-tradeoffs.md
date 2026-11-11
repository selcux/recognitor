# Recognitor Open Questions & Tradeoffs

This document tracks architectural decisions, resolved choices, and design tradeoffs.

---

## 1. Resolved Decision: Embedded Storage Engine

Recognitor uses an embedded SQLite engine via Rust (`rusqlite`) paired with in-memory `petgraph` algorithms for graph processing.
- Runs entirely in-process inside the CLI / MCP server binary with zero daemon or network overhead.
- Universal native cross-platform support (Windows, macOS, Linux).
- Serves as a disposable, rebuildable index projection over local canonical Markdown files.

---

## 2. Database Schema & Index Design (Resolved)

Resolved with a unified SQLite relational schema, soft-key edges, observation tables, and external-content FTS5 search. Full specification defined in [05-database-schema-specification.md](05-database-schema-specification.md).

- **Unified Table & Namespace Mapping (Resolved):** Single `notes` table with indexed `namespace TEXT` and `tier INTEGER` columns. Zero-DDL namespace creation with support for hierarchical paths (`work/backend`).
- **Soft-Key Edges & Ghost Notes (Resolved):** Dedicated `edges` table with `(source_id, target_id, predicate, is_inline)`. `target_id` uses soft references, enabling ghost notes to surface as an unwritten concept backlog without constraint violations.
- **Observation Granularity (Resolved):** Dedicated `observations` table parsing bracketed bullets (`- [category] ...`) with open string `category TEXT` for surgical fact retrieval.
- **Full-Text Indexing (Resolved):** SQLite FTS5 external content virtual table (`notes_fts`) kept in sync via database triggers.
- **Vector Indexing (Open):** Choice between `sqlite-vec` embedded C extension versus pure Rust in-memory HNSW index crate.

---

## 3. Edge Notation Syntax in Markdown Notes (Resolved)

Resolved in favor of a two-tier model: dedicated `## Relations` section for typed semantic edges, and generic inline wikilinks for unweighted mentions. Full specification defined in [03-markdown-format-and-edge-syntax.md](03-markdown-format-and-edge-syntax.md).

- **Typed Relations:** Stored in bullet list format (`- <predicate> [[Target]]`) under a reserved `## Relations` section at the end of the note.
- **Inline Mentions:** Inline `[[wikilinks]]` in body prose are indexed automatically as generic `links_to` edges.
- **Summary Extraction:** The immediate first paragraph following the H1 heading is indexed as a 1-sentence note summary for token-efficient lazy graph traversal.
- **Unidirectional Authoring:** Backlinks and bidirectional traversals are computed in SQLite and petgraph, never authored manually in Markdown files.

---

## 4. Invalidation & Sync Architecture (Open)

How does the SQLite index stay in sync with local Markdown files?

1. **Filesystem Watcher (Notify crate in Rust):**
   - Continuously monitors the directory tree in the background while the MCP server runs.
   - Updates changed files incrementally in near real-time.
2. **On-Demand Stale Check:**
   - Checks file modification times (`mtime`) and checksums at query time or tool invocation.
   - Avoids running a continuous background watcher thread.
3. **Explicit Sync / Rebuild:**
   - Requires explicit CLI or tool triggers (`recognitor sync` / `recognitor rebuild`).

---

## 5. Cross-Domain Agent Scoping & Privacy (Resolved)

Resolved in favor of permissive cross-domain PKM graph traversal with active namespace query filtering.
- **Default Query Scoping:** Initial vector and text searches filter by the active namespace unless configured to search globally.
- **Cross-Domain Traversal:** Graph walks and relational queries follow typed edges across namespace boundaries freely. A note in `coding` can link to a concept in `cognition` or a rule in `personal`.
- **Purpose:** Restores true Personal Knowledge Management (PKM) semantics, eliminating the physical database silos of Basic Memory while preserving clean default views.

---

## 6. Graph Fan-Out & Traversal Strategy (Resolved in Principle)

Instead of hard depth truncation (e.g. blind BFS cutoff at depth 2), Recognitor will leverage recursive SQL CTEs combined with in-memory graph algorithms via `petgraph` in Rust:
- **Personalized PageRank / Topic Walk:** Biases the walk toward seed nodes matching the prompt, pruning off-topic branches mathematically regardless of depth.
- **Hub Dampening:** High-degree hub nodes have their edge weights dampened by inverse degree to prevent generic index pages from overwhelming specific thematic threads.

---

## 7. Information Substitution & Templates (Open)

How should templates and dynamic substitutions be handled for standardized note types?
- **Template System:** Using reusable scaffolds for structured notes (e.g. decisions, session records, specifications).
- **Variable Substitution:** How dynamic variables (dates, timestamps, active namespace, author metadata) are resolved during note creation.

---

## 8. Schema Validation Strategy (Open)

How should note schema conformity be validated?
- **Basic Memory Reference:** Analyze Basic Memory's lightweight Picoschema approach (`bm schema validate --strict`), where schemas are defined as Markdown notes (`type: schema`) with simple field types (`string`, `number`, `boolean`, `string[]`).
- **Validation Timing:** Pre-commit hooks (`.githooks`) vs on-ingestion parsing warnings in SQLite.

---

## 9. CLI and MCP Interface Design (Deferred)

Inventory and ergonomics of developer-facing CLI commands and LLM-facing MCP tools.
- CLI commands (`rebuild`, `sync`, `validate`, `query`).
- MCP tool definitions (`get_bootstrap`, `search`, `context`, `write_note`).
- To be finalized after remaining conceptual mechanisms are locked down.

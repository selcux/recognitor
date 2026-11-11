# Recognitor Open Questions & Tradeoffs

This document tracks architectural decisions, resolved choices, and design tradeoffs.

---

## 1. Resolved Decision: Embedded Storage Engine

Recognitor uses an embedded SQLite engine via Rust (`rusqlite`) paired with in-memory `petgraph` algorithms for graph processing.
- Runs entirely in-process inside the CLI / MCP server binary with zero daemon or network overhead.
- Universal native cross-platform support (Windows, macOS, Linux).
- Serves as a disposable, rebuildable index projection over local canonical Markdown files.

---

## 2. Database Schema & Index Design (Partially Resolved)

How should notes, metadata, edges, observations, and vectors be mapped into SQLite tables?
- **Node & Metadata Attributes (Resolved):** Dedicated columns for `id`, `namespace`, `path`, `title`, `type`, `summary`, and `mtime`. Notes without `type` default to Tier 4 (Domain Knowledge PKM). Full specification in [04-information-tiers-and-lifecycle.md](04-information-tiers-and-lifecycle.md).
- **Observation Granularity (Resolved):** Dedicated `observations` table parsing bracketed bullets (`- [category] ...`) with open string `category TEXT` to enable surgical fact retrieval without context bloat.
- **Edge Representation (Resolved):** Dedicated `edges` table with `(source_id, target_id, predicate, is_inline)`.
- **Vector Indexing (Open):** Whether to use an embedded SQLite extension (`sqlite-vec`) or maintain vector indices in pure Rust.
- **Full-Text Indexing (Open):** FTS5 table configuration and indexing scope.

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

## 5. Cross-Domain Agent Scoping & Privacy (Open)

When an agent is queried within a specific domain, how should cross-namespace edges be exposed?

1. **Strictly Permissive Traversal:**
   - The initial query filters on the active namespace, but any hop across an edge to another namespace is followed automatically.
2. **Explicit Hop Permissions:**
   - Agent config dictates whether cross-namespace traversal is allowed (e.g. `allow_cross_domain: true` or `allowed_domains: ["work", "public"]`).
3. **Depth-Decayed Weighting:**
   - Edges crossing a namespace boundary incur a weight penalty or depth reduction so agents only see immediate boundary notes.

---

## 6. Graph Fan-Out & Traversal Strategy (Resolved in Principle)

Instead of hard depth truncation (e.g. blind BFS cutoff at depth 2), Recognitor will leverage recursive SQL CTEs combined with in-memory graph algorithms via `petgraph` in Rust:
- **Personalized PageRank / Topic Walk:** Biases the walk toward seed nodes matching the prompt, pruning off-topic branches mathematically regardless of depth.
- **Hub Dampening:** High-degree hub nodes have their edge weights dampened by inverse degree to prevent generic index pages from overwhelming specific thematic threads.

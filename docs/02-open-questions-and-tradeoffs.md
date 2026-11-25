# Recognitor Open Questions & Tradeoffs

This document tracks architectural decisions, resolved choices, and design tradeoffs.

---

## 1. Resolved Decision: Embedded Storage Engine

Recognitor uses an embedded SQLite engine via Rust (`rusqlite`), with graph algorithms running in Rust over links loaded from SQLite (§6).
- Runs entirely in-process inside the CLI / MCP server binary with zero daemon or network overhead.
- Universal native cross-platform support (Windows, macOS, Linux).
- Serves as a disposable, rebuildable index projection over local canonical Markdown files.

---

## 2. Database Schema & Index Design (Resolved)

Resolved with a unified SQLite relational schema, soft-key edges, observation tables, external-content FTS5 search, and native vector search. Full specification defined in [05-database-schema-specification.md](05-database-schema-specification.md).

- **Unified Table & Namespace Mapping (Resolved):** Single `notes` table with an indexed `namespace TEXT` column. The reserved namespaces `bootstrap` and `archive` decide how notes are treated; there is no tier column (§10). Zero-DDL namespace creation. Notes are identified by their path; the frontmatter `permalink` is not indexed ([07-interface-contracts.md §6](07-interface-contracts.md#6-note-identity-and-moves)). The namespace is the note's top-level folder under the vault root; deeper folders are just folders, and moving a file into another top-level folder moves it into that namespace (DR1-02).
- **Soft-Key Edges & Ghost Notes (Resolved):** Dedicated `edges` table with `(source_key, target_ref, ref_key, target_key, predicate, is_inline)`. `target_ref` keeps the link as written and `ref_key` its lowercase key for matching; `target_key` is the resolved note key or `NULL`, enabling ghost notes to surface as an unwritten concept backlog without constraint violations, and to heal once the target is written. Resolution rules in [03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution).
- **Observation Granularity (Resolved):** Dedicated `observations` table parsing bracketed bullets (`- [category] ...`) with open string `category TEXT` for surgical fact retrieval.
- **Full-Text Indexing (Resolved):** SQLite FTS5 external content virtual table (`notes_fts`) kept in sync via database triggers.
- **Vector Indexing (Resolved, Optional, Deferred):** Semantic search is an optional feature added after the core (full-text search + graph). The core never depends on it. Embedding model, runtime, schedule, and chunking are chosen when the feature is built (DR1-04). Storage design: embedded `sqlite-vec` virtual table (`notes_vec`) via the official `sqlite-vec` Rust crate. Provides static in-process compilation with zero daemons, unified SQL joins between vector similarity and relational metadata, and zero-copy byte passing with `zerocopy`.

---

## 3. Edge Notation Syntax in Markdown Notes (Resolved)

Resolved in favor of a two-tier model: dedicated `## Relations` section for typed semantic edges, and generic inline wikilinks for unweighted mentions. Full specification defined in [03-markdown-format-and-edge-syntax.md](03-markdown-format-and-edge-syntax.md).

- **Typed Relations:** Stored in bullet list format (`- <predicate> [[Target]]`) under a reserved `## Relations` section at the end of the note.
- **Inline Mentions:** Inline `[[wikilinks]]` in body prose are indexed automatically as generic `links_to` edges.
- **Summary Extraction:** The first paragraph after the H1 heading is the note's summary, for token-efficient graph traversal. Extraction rules: [03-markdown-format-and-edge-syntax.md §2](03-markdown-format-and-edge-syntax.md#2-note-anatomy--layout).
- **Unidirectional Authoring:** Backlinks and bidirectional traversals are computed in SQLite and Rust, never authored manually in Markdown files.

---

## 4. Invalidation & Sync Architecture: On-Demand Sweep with TTL Gate (Resolved)

Resolved in favor of **deterministic on-demand reconciliation with a 2-second TTL cooldown gate**, rejecting continuous background daemon watchers (`notify`).

- **Deterministic Correctness:** Pull-based reconciliation on query or tool invocation guarantees index consistency across machine sleep/wake cycles, git branch switches, and cloud sync without relying on stateful background threads.
- **2-Second TTL Cooldown Window:** Rapid multi-step tool calls within the same agent turn (e.g. `load_bootstrap`, `search`, `context`) execute against hot in-memory SQLite without repeating filesystem stat sweeps. If `(now - last_sweep_time) < 2000ms`, the sweep is bypassed entirely.
- **Stat-Only Sweep:** A directory walk compares each file's `(mtime, size)` against `(notes.mtime, notes.size)` in SQLite using cached file metadata. Only files whose metadata or existence changed are read. Deleted files cascade automatically (requires `PRAGMA foreign_keys = ON`).
- **Developer & Agent Ergonomics:**
  - `recognitor sync`: Explicit CLI command to force an mtime sweep and report dirty files.
  - `recognitor rebuild`: Nuclear command to delete the database file and re-index the entire vault deterministically from canonical Markdown.

### Sync Correctness Rules (DR1-06)

| Rule | Mechanism | Why |
| :--- | :--- | :--- |
| **Inequality, not ordering** | A file is dirty when `(mtime, size) != (notes.mtime, notes.size)`. Never `disk.mtime > notes.mtime`. | Cloud sync and backup restores can deliver files with older timestamps. |
| **Racy files** | A file whose `mtime` is not older than `last_sweep_started_ms` of the sweep that indexed it is re-hashed on the next sweep. | An edit inside the same timestamp tick as the indexing pass leaves `(mtime, size)` unchanged. Git handles its index the same way. |
| **Names, not only stats** | Each sweep also checks the file list itself: a new spelling of a known key (a case-only rename) is stored even when `(mtime, size)` is unchanged, and non-ASCII names and case twins are left out ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)). | A rename can keep mtime and size, and renaming one twin makes the other indexable without that file changing. |
| **Checksum gate** | When metadata changed, hash first; re-parse only when `checksum` differs. | `git checkout` and sync tools rewrite timestamps without changing content. |
| **Shared TTL gate** | The 2-second gate reads `last_sweep_finished_ms` from the `meta` table, not from process memory. | CLI invocations and several MCP servers (one per agent session) share one gate. |
| **Single writer** | The sweep runs inside `BEGIN IMMEDIATE` with WAL mode and `busy_timeout`. | Concurrent processes queue instead of indexing the same files twice. |
| **Write-through** | MCP write tools update the file and the index in the same call. | A search inside the 2-second window still sees the agent's own write. |
| **Optimistic concurrency** | Each edit checks the part it overwrites and, on a mismatch, is refused with the current version of that part. The check and the write run under SQLite's write lock. Full rules in [07-interface-contracts.md §5](07-interface-contracts.md#5-writes-and-conflicts). | Neither another agent nor a human editing in Obsidian loses work silently. |
| **Atomic file writes** | Write to a temporary file in the same directory, then rename over the target; creates and moves use a rename that cannot replace ([07-interface-contracts.md §5.1](07-interface-contracts.md#51-the-write-lock)). | Editors and sync tools never see a half-written note. |
| **Surgical edits** | Edits patch byte ranges taken from the parser's source offsets (`pulldown-cmark` offset iterator). Markdown is never regenerated from the parse tree. | Re-serializing a parse tree rewrites formatting the human chose. |
| **Database location** | Per-user OS cache directory, one database per vault, never inside the vault. | See [05-database-schema-specification.md §5](05-database-schema-specification.md#5-database-file-location). |

---

## 5. Cross-Domain Agent Scoping (Resolved)

Resolved in favor of an open graph: a namespace is an entry point and a perspective, never a boundary.
- **Namespace as Perspective:** `search(query, ns?, page?)` ranks notes in `ns`, and notes linked from it, higher; every knowledge note in the vault can still match. The agent learns `ns` from `load_namespace(ns)` and passes it itself, because the server keeps no state ([07-interface-contracts.md §3-4](07-interface-contracts.md#3-load_namespacens)). Bootstrap notes never appear in searches or graph walks; archive notes appear only on explicit request ([07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access)).
- **Cross-Domain Traversal:** Graph walks and relational queries follow typed edges across namespace boundaries freely. A note in `coding` can link to a concept in `cognition` or a rule in `personal`. The LLM decides when to jump, based on the conversation and the ranking.
- **Purpose:** Restores true Personal Knowledge Management (PKM) semantics. Basic Memory keeps projects in one database but resolves links only inside a project; Recognitor resolves and follows them across namespaces.
- **No Isolation in Early Versions:** the per-session allowlist and sealed namespaces proposed in DR1-07 are dropped to keep the first version simple. They can return if real use shows that unrelated material (for example health notes in a coding session) surfaces too often.

---

## 6. Graph Fan-Out & Traversal Strategy (Resolved in Principle)

Instead of hard depth truncation (e.g. blind BFS cutoff at depth 2), Recognitor will leverage recursive SQL CTEs combined with in-memory graph algorithms in Rust, run over links loaded from the `edges` table:
- **Build order (DR1-14):** plain link-following (recursive SQL and short Rust loops) ships first. Personalized PageRank is added only when the retrieval benchmark (DR1-16) shows that plain link-following ranks results poorly. The graph library (e.g. `petgraph` or about 30 hand-written lines of power iteration) is chosen then; it is an implementation detail, not an architectural dependency.
- **Personalized PageRank / Topic Walk:** Biases the walk toward seed nodes matching the prompt, pruning off-topic branches mathematically regardless of depth.
- **Hub Dampening:** High-degree hub nodes have their edge weights dampened by inverse degree to prevent generic index pages from overwhelming specific thematic threads.
- **Computed Weights (DR1-09):** Edge weights are computed at query time and never authored in Markdown. An explicit `## Relations` edge weighs more than an inline mention, and hub dampening scales the result. The exact values are tuning parameters, set with the retrieval benchmark (DR1-16).

---

## 7. Information Substitution & Templates (Open)

How should templates and dynamic substitutions be handled for standardized note types?
- **Template System:** Using reusable scaffolds for structured notes (e.g. decisions, session records, specifications).
- **Variable Substitution:** How dynamic variables (dates, timestamps, the note's namespace, author metadata) are resolved during note creation.

---

## 8. Schema Validation Strategy: Structural Linting + Vault-Wide Type Schemas (Resolved)

Resolved in favor of lightweight **Structural Linting** plus optional **vault-wide type schemas in TOML**, rather than a user-facing schema DSL (such as Basic Memory's Picoschema) or schema notes.

- **Resilient SQLite Fallbacks:** Recognitor's embedded relational engine does not require schema conformity to function. Missing frontmatter defaults to `type: 'note'` (location, not `type`, decides how a note is treated), titles fall back to the H1 heading, and namespaces are inferred from filesystem paths.
- **Structural Invariants Only:** The engine validates the mechanical invariants necessary for graph construction:
  1. Frontmatter is valid, syntactically parsable YAML.
  2. Lines under `## Relations` match `- <predicate> [[Target]] (context)`; other lines index their links as `links_to` with a warning.
  3. Observation qualifiers parse as ISO dates; a qualifier that does not parse stays text with a warning.
  4. Links resolve unambiguously; an ambiguous link stays a ghost with a warning.
  5. Summary paragraphs fit within 300 characters.
  6. A frontmatter `status: superseded` agrees with the `supersedes` edges.
  7. Every note and folder name is ASCII, and no two paths share a lowercase key (case twins). Both are errors, and the files are not indexed.
  8. Spelling: an entry point not spelled `index.md`, and a Markdown link whose case differs from the file's, get a warning.
  9. Every namespace has an `index.md`; a namespace without one gets a warning.

  Hint, never an error, even with `--strict`: the vault has no `bootstrap/` folder. That is a valid setup, but `load_bootstrap` then returns an error.

  Syntax rules: [03-markdown-format-and-edge-syntax.md](03-markdown-format-and-edge-syntax.md).
- **Ingestion Tolerance:** Invalid files trigger non-fatal diagnostic warnings during SQLite indexing rather than halting parsing or dropping notes from search. The exceptions are names: a non-ASCII name is not indexed, and neither are case twins, because indexing one would guess which file the user meant and indexing both would break link resolution. Both stay out until renamed ([07-interface-contracts.md §6](07-interface-contracts.md#6-note-identity-and-moves)). A file the sweep listed but can no longer read does not fail the sweep either: if it is gone (deleted, or replaced by an editor's save-and-rename), it counts as deleted; if it is locked or unreadable, it is skipped with a warning, and its indexed copy stays until a later sweep reads it.
- **Type Schemas (DR1-12):** one file per vault, `<vault>/.recognitor/schemas.toml`, holds one block per note `type`. A block applies to every note with that `type`, in every namespace except the archive, bootstrap included:

  ```toml
  [type.concept]
  required_fields = ["status", "grade"]
  allowed = { status = ["studied", "draft"], grade = ["green", "yellow", "orange"] }
  required_categories = ["fact", "use-when", "limit"]
  required_relations = ["part_of"]
  ```

  | Key | Checks in the note | Fails when |
  | :--- | :--- | :--- |
  | `required_fields` | These frontmatter keys exist and have a value | A listed key is missing or empty |
  | `allowed` | Frontmatter values are on the list | `grade: purple` |
  | `required_categories` | At least one `- [category]` observation per listed category, anywhere in the body | No `[limit]` line |
  | `required_relations` | At least one `- predicate [[X]]` line per listed predicate under `## Relations` | No `part_of` line |

  - Notes with no `type`, or with a type no block lists, are not checked.
  - **Not the archive:** archive notes are frozen history. A schema added later would fail `lint --strict` on every old record, and the only fix would be editing them. The structural checks still run on archive notes.
  - **Empty:** a null value, a blank string or an empty list. An empty required field gets only its `required_fields` finding, not an `allowed` one too. An empty mapping (`status: {}`) is not empty: `allowed` reports it as not a plain value.
  - **Matching:** type names and `allowed` values match exactly, trimmed, with case counting, because the list sets the spelling. A list value passes only when every item is allowed. Category and predicate names ignore case, as the parser does.
  - **The file itself:** a missing `schemas.toml` means no schemas and no finding. A file that cannot be read or is not TOML, an unknown key, or a value of the wrong kind is a lint error, because a gate that silently does not run is worse than one that fails. A bad key is skipped, and the rest of the file still applies.
  - **One type, one definition:** a type that needs different rules gets a different name (e.g. `health-record` instead of `record`). There are no per-namespace overrides; `type` is free metadata, so renaming costs nothing.
  - **Rejected: per-namespace schemas.** In the Basic Memory vault they produced an identical duplicate (`plan` defined in two projects) and unchecked notes (`decision` and `record` used in projects that have no schema for them).
  - **Rejected: schema notes in Markdown.** They turn notes into configuration.
  - **Rejected: Picoschema.** A schema DSL is exactly what this section avoids.
- **Lint exit code:** `recognitor lint` exits non-zero when it finds an error (check 7: non-ASCII names, case twins). `recognitor lint --strict` also turns structural and schema warnings into errors, for pre-commit hooks and CI workflows. Output, messages and exit codes: [07-interface-contracts.md §12.2](07-interface-contracts.md#122-lint---strict).

---

## 9. CLI and MCP Interface Design (Resolved)

Resolved in [07-interface-contracts.md](07-interface-contracts.md): a stateless server, MCP tools for every-session agent work, and a CLI limited to what a human cannot do by hand.
- **MCP:** `load_bootstrap`, `load_namespace`, `search`, `context`, `read_note`, `write_note`, `edit_note`, `move_note`, `relink`, `delete_note`.
- **CLI:** `bootstrap [--list]`, `bootstrap write <harness>...`, `bootstrap remove <harness>...`, `search`, `context`, `read`, `move`, `relink`, `ghosts [<link>]`, `lint [--strict]`, `doctor`, `trash empty`, `sync`, `rebuild`, `config get/set`. No write or edit commands for notes: humans edit the files directly. `doctor` only reads harness instruction files; `bootstrap write` and `bootstrap remove` are the commands that edit them ([07-interface-contracts.md §12.6](07-interface-contracts.md#126-bootstrap-write-harness), [§12.7](07-interface-contracts.md#127-bootstrap-remove-harness)).
- **Rejected:** a raw-SQL `query` command, a `--json` output flag, and `ghosts` as an MCP tool.
- **Order (DR1-15):** every schema change must now be justified by a contract in doc 07.

---

## 10. Bootstrap and Session Loading (Resolved)

Resolved in favor of **three stores separated by location** (bootstrap, knowledge namespaces, archive), replacing the 5-tier model in which frontmatter `type` decided loading. Full specification in [04-information-stores-and-lifecycle.md](04-information-stores-and-lifecycle.md).

- **Bootstrap as a tree:** `load_bootstrap()` walks `## Relations` links from `bootstrap/index.md`, depth-first in written order, never following inline mentions, `supersedes`, reversed edges, or links that leave the bootstrap. Notes no link reaches do not load, so linking selects the active persona (DR1-01).
- **Fixed location:** the bootstrap is always `<vault>/bootstrap/`; this version has no custom bootstrap directory (DR2-11). Its notes belong to the reserved namespace `bootstrap`.
- **Global rules only:** everything in the bootstrap applies in every project. Project rules stay in the repository's own instruction files (`AGENTS.md`, `CLAUDE.md`, Cursor rules), so each rule has one home.
- **Namespace entry:** every namespace has an `index.md`; `load_namespace(ns)` returns it and keeps no state ([07-interface-contracts.md §3](07-interface-contracts.md#3-load_namespacens)).
- **Same tables:** bootstrap notes share the schema but get no full-text or vector rows, so search cannot return them.
- **Session start:** the written block or an instruction in the harness file, or a session-start hook running `recognitor bootstrap` where the harness supports one. `recognitor doctor` checks that each installed harness file still holds that payload, and `recognitor bootstrap write <harness>...` puts it there ([04-information-stores-and-lifecycle.md §5.3](04-information-stores-and-lifecycle.md#53-the-bootstrap-block)).
- **Rejected: a separate database table for the bootstrap.** Same Markdown means the same parser, link resolver, and lint; leaving bootstrap notes out of the search indexes gives the same isolation.
- **Rejected: per-namespace `rules/` folders.** Project rules already live in the project repository's instruction files.

---

## 11. Target Scale & Benchmark (Resolved)

Resolved in favor of explicit targets checked by a benchmark, replacing unmeasured performance claims (DR1-16).

### Target Scale

The reference vault on 2026-11-15 holds 108 notes, 1,102 observations, 202 links, and 269 KB of Markdown. The target is **100× that size**:

| Notes | Observations | Links | Markdown |
| :--- | :--- | :--- | :--- |
| 10,000 | 100,000 | 20,000 | ~25 MB |

The benchmark vault is generated by scaling up the real vault, so its shape (note sizes, link density, observation counts) matches real use.

### Latency Targets

Targets, not measurements. Percentiles are p95; Windows is measured first because it has the slowest filesystem metadata calls of the three platforms.

| Operation | p95 target | Rationale |
| :--- | :--- | :--- |
| `load_bootstrap()`, `load_namespace()`, full-text search | < 50 ms | Disappears inside an agent turn that takes seconds |
| Sync sweep, no changes, 10,000 files | < 100 ms | Runs before queries once the 2-second gate expires |
| `recognitor bootstrap`, cold process start | < 100 ms | A session-start hook adds this to every session |
| Full `rebuild`, without vectors | < 10 s | Runs after every schema version change |

### Retrieval Benchmark

- **Question set:** 20–30 real questions taken from agent sessions, each with the notes that should be retrieved.
- **Measures:** how many expected notes appear in the top 5 results (recall@5), and how many tokens an agent spends to reach the answer.
- **Baseline:** Basic Memory on the same vault.
- **Decides:** the computed edge weights (§6) and whether Personalized PageRank is built (DR1-14). The bootstrap payload size is measured on every run.

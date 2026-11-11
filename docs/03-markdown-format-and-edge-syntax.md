# Recognitor Markdown Format & Edge Syntax Specification

This document defines the canonical format for Markdown notes in Recognitor, specifying how document structure, metadata, summaries, and graph relations are parsed and indexed by the Rust engine into SQLite.

---

## 1. Design Principles

1. **Human-Readable & Editor-Native:** Files remain clean plain-text Markdown compatible with Obsidian, VS Code, and standard tools.
2. **Zero Context Pollution:** Notes support lightweight, two-stage lazy loading. The engine extracts a compact title and summary so autonomous agents can traverse graph topology without pulling complete note bodies into LLM prompt context.
3. **Unidirectional Authoring, Bidirectional Querying:** Links are authored in one direction only. Bidirectional backlinks and inverse graph traversals are handled purely in the SQLite index and Rust graph layer (`petgraph`).
4. **Deterministic Streaming Parsing:** Document anatomy is strictly predictable, allowing fast streaming extraction via Rust AST parsers (`pulldown-cmark`) without LLM preprocessing.

---

## 2. Note Anatomy & Layout

Every Recognitor note adheres to the following structural layout:

```markdown
---
title: Auth Flow
namespace: backend
type: specification
tags: [security, oauth]
---

# Auth Flow

Handles OAuth2 token refresh and session rotation across backend services.

## Observations
- [fact] Access tokens expire after 15 minutes.
- [rule] Refresh tokens must be rotated on every exchange.

## Relations
- part_of [[Security Architecture]]
- supersedes [[Legacy Cookie Auth]]
- relates_to [[User Session Store]]
```

### Component Breakdown

| Section | Role | Parsing & Engine Handling |
| :--- | :--- | :--- |
| **Frontmatter** | Document metadata | Parsed with `serde_yaml`. Contains file attributes (`title`, `type`, `tags`). Undeclared `type` defaults automatically to Tier 4 (Domain Knowledge PKM). **Never contains edge definitions.** |
| **H1 Heading (`# Title`)** | Primary note title | Fallback for title if omitted in frontmatter. |
| **Summary Paragraph** | One-sentence synopsis | The first non-empty paragraph immediately following the H1 heading. Extracted into the SQLite `notes.summary` column for compact graph traversal. |
| **Body / Observations** | Primary content | Free-form Markdown. Individual categorized observation bullets (`- [category] ...`) are parsed into the `observations` table with open `category TEXT`. |
| **`## Relations`** | Typed graph edges | Dedicated section reserved at the bottom of the note for explicit, typed relational edges. |

---

## 3. Graph Edge Syntax

Recognitor implements a **two-tier linking model**:

### Tier 1: Typed Semantic Edges (`## Relations`)

Typed edges describe exact relationships between notes and must reside under the `## Relations` heading as bullet list items:

```markdown
## Relations
- <predicate> [[Target Note]]
- <predicate> [[namespace::Target Note]]
```

- **Predicate:** The single token immediately preceding the wikilink target (e.g. `part_of`, `supersedes`, `depends_on`).
- **Target:** A standard wikilink containing the target note title or permalink. Optional cross-namespace references use the `namespace::note` prefix.

#### Engine Predicate Classification

Recognitor supports an **open vocabulary** for predicates in SQLite (`predicate TEXT`), but the engine recognizes three functional behavioral classes during graph traversal:

| Class | Predicates | Engine Behavior |
| :--- | :--- | :--- |
| **Hierarchical** | `part_of`, `has_part`, `indexes` | Handled via recursive SQL CTEs to collapse or expand subtrees, modules, and namespaces. Inverse direction automatically flips (`part_of` <-> `has_part`). |
| **Lifecycle & Invalidation** | `supersedes` | Traversal algorithms recognize superseded notes as historical, automatically deprioritizing or excluding them from prompt retrieval. |
| **Lateral & Associative** | `relates_to`, `depends_on`, `implements`, etc. | Standard weighted relational edges used in `petgraph` graph walks (PageRank, shortest paths). |

### Tier 2: Generic Inline Mentions

Casual wikilinks scattered inside the note body or prose (`...as discussed in [[Rate Limiting]]...`) are automatically detected by the parser.
- **Engine Handling:** Inline wikilinks are indexed in SQLite with the default predicate `links_to`.
- **Purpose:** Preserves serendipitous connections across notes without forcing the user to assign semantic weight or pollute prose with metadata keys.

---

## 4. On-Demand Traversal & Ingestion Mechanics

To prevent LLM context bloat, Recognitor separates graph discovery from note reading:

1. **Ingestion Pipeline (Rust):**
   - The streaming parser extracts frontmatter, H1 title, summary paragraph, and relation list in a single pass.
   - Populates SQLite `notes` and `edges` tables:
     ```sql
     -- Notes projection
     INSERT INTO notes (id, namespace, path, title, summary, content, mtime)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);

     -- Edges projection
     INSERT INTO edges (source_id, target_id, predicate, is_inline)
     VALUES (?1, ?2, ?3, ?4);
     ```

2. **Compact Graph Walk (Two-Stage Retrieval):**
   - **Stage 1 (Discovery):** Agent graph traversal tools return compact skeletons containing only `(id, namespace, title, summary, predicate)`. An agent can inspect 20 related nodes using under 300 prompt tokens.
   - **Stage 2 (Hydration):** The agent only fetches the complete Markdown body via `read_note(id)` for the specific nodes deemed necessary.

3. **Bidirectional Graph Queries:**
   - The SQLite index supports immediate incoming and outgoing queries:
     ```sql
     -- Outgoing edges
     SELECT predicate, target_id FROM edges WHERE source_id = ?;

     -- Incoming backlinks
     SELECT predicate, source_id FROM edges WHERE target_id = ?;
     ```
   - Neither the user nor the agent ever writes reciprocal backlinks into source Markdown files.

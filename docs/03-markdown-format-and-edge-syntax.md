# Recognitor Markdown Format & Edge Syntax Specification

This document defines the canonical format for Markdown notes in Recognitor, specifying how document structure, metadata, summaries, observations, and graph relations are parsed and indexed by the Rust engine into SQLite.

---

## 1. Design Principles

1. **Human-Readable & Editor-Native:** Files remain clean plain-text Markdown compatible with Obsidian, VS Code, and standard tools.
2. **Zero Context Pollution:** Notes support lightweight, two-stage lazy loading. The engine extracts a compact title and summary so autonomous agents can traverse graph topology without pulling complete note bodies into LLM prompt context.
3. **Unidirectional Authoring, Bidirectional Querying:** Links are authored in one direction only. Bidirectional backlinks and inverse graph traversals are handled purely in the SQLite index and the Rust graph layer.
4. **Deterministic Streaming Parsing:** Document anatomy is strictly predictable, allowing fast streaming extraction via Rust AST parsers (`pulldown-cmark`) without LLM preprocessing. Rules are applied to the parse tree, not to raw lines, so links, task boxes, and code are recognized for what they are.

---

## 2. Note Anatomy & Layout

Every Recognitor note adheres to the following structural layout. This example lives at `backend/Auth Flow.md`, so its namespace is `backend`:

```markdown
---
title: Auth Flow
type: spec
tags: [security, oauth]
---

# Auth Flow

Handles OAuth2 token refresh and session rotation across backend services.

## Observations
- [fact] Access tokens expire after 15 minutes. #tokens
- [decision] @effective:2026-06-10 Refresh tokens are rotated on every exchange.

## Relations
- part_of [[Security Architecture]]
- supersedes [[Legacy Cookie Auth]] (replaced after the 2026 audit)
- relates_to [[User Session Store]]
```

### Component Breakdown

| Section | Role | Parsing & Engine Handling |
| :--- | :--- | :--- |
| **Frontmatter** | Document metadata | Parsed with `serde_yaml`. Contains file attributes (`title`, `type`, `tags`). The note's id is its path, never a frontmatter field (see §4); a `permalink` field written by Basic Memory or Obsidian Publish is left untouched and ignored. The namespace is not a frontmatter field either: it is the note's top-level folder (see §4). `type` is free metadata: location, not `type`, decides how a note is loaded ([04-information-stores-and-lifecycle.md](04-information-stores-and-lifecycle.md)). `type` selects the note's schema, if one exists ([02 §8](02-open-questions-and-tradeoffs.md)). **Never contains edge definitions.** |
| **H1 Heading (`# Title`)** | Primary note title | Fallback for title if omitted in frontmatter. |
| **Summary Paragraph** | Short synopsis | The first non-empty paragraph immediately following the H1 heading, cut at a sentence boundary within 300 characters. Lint warns when the paragraph is longer. Extracted into the SQLite `notes.summary` column for compact graph traversal. |
| **Body / Observations** | Primary content | Free-form Markdown. Observation bullets are parsed anywhere in the body into the `observations` table (see Observation Syntax below). `## Observations` is a convention, not a requirement. |
| **`## Relations`** | Typed graph edges | Dedicated section for explicit, typed relational edges (see §3). |

### Observation Syntax

```markdown
- [category] @qualifier Observation text. #tag1 #tag2
```

- **Line shape:** a list item whose text starts with `[category]` followed by a space.
- **Category:** starts with a letter, then letters, digits, `_` or `-`, and is at least 2 characters long. Stored in lowercase, so `[Fact]` and `[fact]` group together. The vocabulary is open.
- **What never counts:** the category rule alone excludes Markdown task boxes (`[ ]`, `[x]`, `[X]`), Obsidian task statuses (`[-]`, `[/]`, `[>]`, `[?]`, `[!]`, `[b]`, …), and transcript timestamps (`[24:33.098]`). A list item that starts with a Markdown link (`- [Title](file.md)`) is a link, not an observation.
- **Location:** anywhere in the body, outside code. Notes in the `bootstrap` and `archive` namespaces store no observations: bootstrap notes are loaded whole, and archive notes are graph-silent (§3).
- **Qualifier (optional):** placed right after the category, it says when the fact holds:
  - `@kind:value` or `@kind[start,end)`; a bare `@value` means kind `valid`.
  - Kinds: `effective`, `valid`, `occurred`, `due`, `mentioned`.
  - Values are ISO 8601 only: `2026`, `2026-06`, `2026-06-10`, or a timestamp. Ranges use `[` / `]` for inclusive and `(` / `)` for exclusive ends, and an empty side is open (`@effective[2026-06-10,)`).
  - No natural-language, slash-formatted, or relative dates. A qualifier that does not parse stays ordinary text, and lint warns.
  - Stored as raw text in `observations.qualifiers`; date-aware queries come later.
- **Tags (optional):** trailing `#tags` stay in the text and are also stored in `observations.tags`.

---

## 3. Graph Edge Syntax

Recognitor implements a **two-tier linking model**:

### Tier 1: Typed Semantic Edges (`## Relations`)

Typed edges describe exact relationships between notes and exist only under the `## Relations` heading, as bullet list items:

```markdown
## Relations
- <predicate> [[Target Note]]
- <predicate> [[namespace::Target Note]] (optional context)
```

- **Predicate:** a single token immediately preceding the wikilink target (e.g. `part_of`, `supersedes`, `depends_on`): letters, digits, and `_`. Multi-word predicates are not supported.
- **Target:** a wikilink containing the target note's title, filename, or path. Optional cross-namespace references use the `namespace::note` prefix. Resolution rules are defined in §4.
- **Context:** an optional trailing `(text)` is stored in `edges.context`.
- **Lines of other shapes:** a line under `## Relations` that does not match `- <predicate> [[Target]] (context)`, such as `- see also [[X]]` or `- [[X]]`, still indexes its links as generic `links_to` edges, and lint warns.
- **Outside `## Relations`:** a bullet like `- Mother [[Alice]]` in the body is an inline mention (Tier 2). Because typed edges live only in their section, prose never needs an escape syntax to avoid becoming a typed edge.

#### Engine Predicate Classification

Recognitor supports an **open vocabulary** for predicates in SQLite (`predicate TEXT`), but the engine recognizes three functional behavioral classes during graph traversal:

| Class | Predicates | Engine Behavior |
| :--- | :--- | :--- |
| **Hierarchical** | `part_of`, `has_part`, `indexes` | Handled via recursive SQL CTEs to collapse or expand subtrees, modules, and namespaces. Inverse direction automatically flips (`part_of` <-> `has_part`) in graph queries; bootstrap loading follows the written direction only. Both directions are stored as written, and graph queries treat `A has_part B` plus `B part_of A` as one fact. |
| **Lifecycle & Invalidation** | `supersedes` | Traversal algorithms recognize superseded notes as historical, automatically deprioritizing or excluding them from prompt retrieval. `load_bootstrap()` never follows them. The `supersedes` edge is the only source of truth for "superseded": lint flags a frontmatter `status: superseded` that no edge confirms, and an edge whose target says otherwise. |
| **Lateral & Associative** | `relates_to`, `depends_on`, `implements`, etc. | Relational edges with weights computed at query time ([02 §6](02-open-questions-and-tradeoffs.md)), used in graph walks (Personalized PageRank, shortest paths). |

### Tier 2: Generic Inline Mentions

Casual links scattered inside the note body or prose (`...as discussed in [[Rate Limiting]]...`) are automatically detected by the parser.
- **Engine Handling:** Inline links are indexed in SQLite with the default predicate `links_to`.
- **Purpose:** Preserves serendipitous connections across notes without forcing the user to assign semantic weight or pollute prose with metadata keys.

#### Wikilink Forms

| Form | Target | Notes |
| :--- | :--- | :--- |
| `[[A]]` | `A` | |
| `[[A\|alias]]` | `A` | The alias is display text only. |
| `[[A#Heading]]` | `A` | The heading is ignored for resolution. |
| `![[A]]` | `A` | An embedded note counts as an inline link. Embeds of non-Markdown files (images, PDFs) are ignored. |

#### Markdown Links

Standard Markdown links to notes in the vault also create `links_to` edges, so notes written in other editors join the graph without conversion:

```markdown
Follow [the deploy checklist](../ops/Deploy%20Checklist.md) before release.
```

- Paths resolve relative to the linking note. A leading `/` means the **vault root**, so a Markdown link can cross namespaces.
- Percent-encoding is decoded; `#fragments` and query strings do not change the target.
- Exact files only: no title lookup and no added `.md`. A link to a file that does not exist yet stays a ghost until it does.
- Ignored: external URLs, `mailto:` and `file:` links, images, fragment-only links (`#section`), and paths that leave the vault.
- Markdown links are always `links_to`; typed relations use wikilinks under `## Relations`.

#### Code Is Never Parsed

Wikilinks, Markdown links, and observation-shaped lines inside inline code or code blocks are text. They create no edges and no observations.

#### Graph-Silent Archive

Notes in the `archive` namespace are full-text searchable, but the parser creates no observations and no edges from them. Raw transcripts would otherwise flood the graph with false observations and ghost links ([04-information-stores-and-lifecycle.md §4](04-information-stores-and-lifecycle.md)).

---

## 4. Note Identity and Link Resolution

### Identity: the path

Recognitor follows the Obsidian model: a note's address is its path ([07-interface-contracts.md §6](07-interface-contracts.md#6-note-identity-and-moves)).

- **Note id:** the path from the vault root, exactly as the file is named, without `.md` (`notes.id`), e.g. `common/working-style/ADHD profile`. There is no stored id and no slug conversion, so nothing can drift.
- **`permalink` is ignored:** the frontmatter field means a web address to Obsidian Publish and an id to Basic Memory; Recognitor leaves it untouched and never indexes it.
- **Case:** matching ignores case, as Windows and macOS file systems do. Two files in the same folder whose names differ only in case are an error ([07-interface-contracts.md §6](07-interface-contracts.md#6-note-identity-and-moves)).
- **Namespace:** the top-level folder under the vault root (`common/working-style/ADHD profile.md` → `common`). Deeper folders (`working-style`, `personas`) are just folders. Notes stored directly in the vault root belong to the `default` namespace. The namespace is never declared in frontmatter, so it has exactly one source. Hidden folders (`.git/`, `.obsidian/`, `.recognitor/`) are not namespaces and are never indexed.
- **Reserved namespaces:** `bootstrap` (loaded at session start; its directory can be relocated) and `archive`. See [04-information-stores-and-lifecycle.md](04-information-stores-and-lifecycle.md).
- **Entry point:** every namespace has an `index.md` at its top level.
- **Moves:** moving a file changes its id. Moving it into another top-level folder also moves the note into that namespace. Path-style links to it break; `move_note` reports them and `relink` fixes the ones the user picks ([07-interface-contracts.md §6.1](07-interface-contracts.md#61-moving-a-note)).

### Links: resolved to a note id

Wikilinks name a note by title (`[[ADHD profile]]`), by filename, by path, or with a namespace prefix (`[[cognition::index]]`). The engine resolves each one to a note id in this order. Matching is case-insensitive, and a "name" is a note's title or its filename without `.md`:

1. **Exact path:** `[[common/working-style/ADHD profile]]` matches a note id, with or without `.md`. Explicit relative paths (`[[./same.md]]`, `[[../guides/Setup.md]]`) resolve relative to the linking note, to that exact file only.
2. **Prefixed name:** `[[ns::Name]]` matches a name inside namespace `ns`.
3. **Same-namespace name:** `[[Name]]` matches a name inside the source note's namespace.
4. **Unique name:** `[[Name]]` matches a name that exists in exactly one namespace.
5. **Ghost:** no match, or more than one match at the first step that finds any. The link is stored with `target_id = NULL`. An ambiguous link also raises a lint warning, because guessing would create a silently wrong edge. A link that hits a case twin is an error naming both files.

Filename matching matters for Obsidian, which links by filename even when the frontmatter title differs.

### Re-resolution and ghosts

- **Raw text is kept:** `edges.target_ref` stores the link exactly as written, so it can be resolved again later.
- **Ghosts heal:** after every sync that adds, removes, or retitles a note, all links are re-resolved. A link written before its target existed resolves once the target is created, wherever it lands (`backend/specs/security-architecture`, not only a guessed `backend/security-architecture`).
- **Deletes revert to ghosts:** links to a deleted note return to `target_id = NULL` and show up in the ghost backlog.
- **Title renames:** links that use the old title become ghosts and show up in the ghost backlog, unless the filename still matches.
- **File moves:** a move breaks path-style links (`[[ideas/caching]]`) to the moved note. A name link keeps resolving through steps 3 and 4, unless the move makes the name ambiguous. A move made outside Recognitor shows up in the sweep as a deleted and a created file with the same checksum, and is reported as a probable move ([07-interface-contracts.md §6.2](07-interface-contracts.md#62-moves-outside-recognitor)).
- **Obsidian:** Obsidian does not understand `[[ns::Title]]` and shows it as an unresolved link. The syntax stays because existing notes use it.

---

## 5. On-Demand Traversal & Ingestion Mechanics

To prevent LLM context bloat, Recognitor separates graph discovery from note reading:

1. **Ingestion Pipeline (Rust):**
   - The streaming parser extracts frontmatter, H1 title, summary paragraph, observations, typed relations, and inline links in a single pass.
   - Populates SQLite `notes`, `observations`, and `edges` tables:
     ```sql
     -- Notes projection (upsert keeps notes.pk stable)
     INSERT INTO notes (id, namespace, type, title, path, summary, content, checksum, mtime, size)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
     ON CONFLICT(id) DO UPDATE SET
         namespace = excluded.namespace, type = excluded.type, title = excluded.title,
         path = excluded.path, summary = excluded.summary, content = excluded.content,
         checksum = excluded.checksum, mtime = excluded.mtime, size = excluded.size;

     -- Edges projection: explicit relations first, then inline links
     INSERT INTO edges (source_id, predicate, target_ref, is_inline, position, context)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
     ON CONFLICT DO NOTHING;
     -- target_id is filled by the resolution pass (§4)
     ```

2. **Compact Graph Walk (Two-Stage Retrieval):**
   - **Stage 1 (Discovery):** Agent graph traversal tools return compact skeletons containing only `(id, namespace, title, summary, predicate)`. Estimated cost: about 15 tokens per node without the summary and about 40 with it, so 20 nodes cost roughly 800 tokens. Real numbers come from the benchmark (DR1-16).
   - **Stage 2 (Hydration):** The agent only fetches the complete Markdown body via `read_note(id)` for the specific nodes deemed necessary.

3. **Bidirectional Graph Queries:**
   - The SQLite index supports immediate incoming and outgoing queries:
     ```sql
     -- Outgoing edges
     SELECT predicate, target_ref, target_id FROM edges WHERE source_id = ?;

     -- Incoming backlinks
     SELECT predicate, source_id FROM edges WHERE target_id = ?;
     ```
   - Neither the user nor the agent ever writes reciprocal backlinks into source Markdown files.

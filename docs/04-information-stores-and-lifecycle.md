# Recognitor Information Architecture & Lifecycle Specification

This specification defines where information lives in Recognitor and how the engine treats it: what loads at session start, what is retrieved on demand, and what stays frozen. Every store uses the same Markdown format ([03-markdown-format-and-edge-syntax.md](03-markdown-format-and-edge-syntax.md)). Only the engine's treatment differs.

---

## 1. Three Stores

Information in an agent environment changes at different speeds and is read in different ways. Grounding facts and global rules must be present before the first reply; domain knowledge is pulled in only when a task needs it; raw session records are kept but rarely read. Recognitor separates these by **location**, not by frontmatter `type`:

```
+--------------------------------------------------------------------------+
| bootstrap/      profile · behavior · rules     load_bootstrap()          | <- every session, whole link tree
+--------------------------------------------------------------------------+
| <namespace>/    index.md                       load_namespace(ns)        | <- when a session enters a namespace
|                 all other notes                search, graph walks       | <- on demand
+--------------------------------------------------------------------------+
| archive/        session records, transcripts   explicit request only     | <- frozen
+--------------------------------------------------------------------------+
```

| | Bootstrap | Knowledge namespaces | Archive |
| :--- | :--- | :--- | :--- |
| **Location** | `bootstrap/` | Every other top-level folder | `archive/` |
| **Read as** | Instructions and grounding | Data | Data |
| **Loaded** | Whole link tree, once per session | `index.md` on entry; everything else on demand | Only on explicit request |
| **Engine treatment** | Tree walk from `index.md` | Full-text search, vector search (optional), graph walks | Full-text search on request |
| **In search results** | Never | Yes, ranked higher when the search names their namespace | Only when the search names `archive` |
| **Main writer** | The user | Agents and the user | Agents, append only |

What each tool may do in each store: [07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access).

### Default Store Rule

> **Rule:** A note's location decides its store. Every other top-level folder is knowledge. Frontmatter `type` is free metadata and never decides how a note is loaded or searched.

- Casual Markdown notes without frontmatter land in knowledge and are never injected into startup context.
- A typo in `type:` can neither drop a note from the bootstrap nor promote one into it. Routing by location fails closed.

---

## 2. Bootstrap Store

The bootstrap holds everything an agent needs before its first reply: who the user is, how to talk to the user, and the global rules that apply in every project.

### 2.1 Layout

```
bootstrap/
├── index.md                 entry point
├── profile/                 who the user is
├── behavior/                how to talk to the user
│   └── personas/            folder names are free; only links decide what loads
└── rules/                   global rules
```

- **Convention, not semantics:** `profile/`, `behavior/` and `rules/` organize the files for humans. The engine reserves no folder names inside the bootstrap.
- **Global rules only:** everything the bootstrap loads applies in every project and namespace. Recognitor serves no per-namespace rules; project rules live in the project repository's own instruction files (§5.2).
- **Fixed location:** the bootstrap is always `<vault>/bootstrap/`, and `bootstrap` is a reserved namespace name. This version has no custom bootstrap directory (DR2-11).

### 2.2 Loading: `load_bootstrap()`

The bootstrap is loaded as a tree, starting from `index.md` and following links in one direction:

1. Load `bootstrap/index.md`.
2. Follow its `## Relations` links depth-first, in the order they are written (`edges.position`).
3. Load every note reached, then follow its links the same way.
4. Never follow:
   - inline `[[mentions]]` in prose, and lines under `## Relations` without a relation word (`- [[Mentor]]`), which count as plain links ([03-markdown-format-and-edge-syntax.md §3](03-markdown-format-and-edge-syntax.md#3-graph-edge-syntax)). The payload does not mention them; `sync` warns and `lint` reports them;
   - `supersedes` links, which point at a retired version;
   - links in the reverse direction: a `part_of` written in the target note is not an edge out of the source;
   - links that leave the bootstrap;
   - ghost links, and notes already visited.
5. Return one Markdown payload: each note's content as written, with frontmatter and `## Relations` stripped, and a blank line between notes. When a note's content does not start with a `# ` heading line, `# <title>` is added above it ([03-markdown-format-and-edge-syntax.md §2](03-markdown-format-and-edge-syntax.md#2-note-anatomy--layout)), so every note starts with a heading. Nothing else is added or changed.

**Selection is linking.** Bootstrap notes that no link reaches are not loaded. With ten personas in `behavior/personas/`, only the one that a loaded note links to is loaded. Switching persona means editing one link.

- **Budget:** the payload size is measured on every load, as characters ÷ 4 of the payload with its ghost line. Above the configured `bootstrap.max_tokens` (default 4,000), the payload ends with one more line: `warning: the bootstrap is 7071 tokens (estimate), above bootstrap.max_tokens (4000)`. There are no fixed per-section token targets.
- **CLI twin:** `recognitor bootstrap` prints the same payload, for harness hooks (§5.1). `recognitor bootstrap --list` prints which notes load, in order, without their content: one id per line (with its title when it differs, as in [07-interface-contracts.md §4.2](07-interface-contracts.md#42-output)), then the ghost line. It carries no budget warning.
- **`## Relations` stripped:** from its heading to the next level-1 or level-2 heading, or to the end of the file, the same section the parser reads relations from.
- **No entry point:** `load_bootstrap` reads only the index. When the index has no `bootstrap/index.md` (no `bootstrap/` folder, no `index.md`, or a case twin), the tool returns one error line instead of the payload: ``error: bootstrap/index.md not found. Run `recognitor lint`.`` `recognitor bootstrap` prints the same line on stderr, with nothing on stdout, and exits with a non-zero code. A vault without a bootstrap is valid: the user decides whether the session-start hook calls it. `lint` names the cause ([02-open-questions-and-tradeoffs.md §8](02-open-questions-and-tradeoffs.md)).
- **Ghost links in the tree:** the payload ends with one line listing them: `not loaded: behavior/personas/Mentor (not written yet)`. Several ghosts share the line, in load order, each once, as their links wrote them: `not loaded: Mentor (not written yet), Tone (ambiguous)`. A link that matches more than one note says `(ambiguous)`. Without ghosts, the line is left out.

### 2.3 Writes

Bootstrap notes are read as instructions in every future session, so writes to them carry more weight than knowledge writes (DR1-08):

- When an MCP write or edit touches a note in the bootstrap, the tool result states that the note loads as instructions at session start ([07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access)).
- A new bootstrap note loads only once a loaded note links to it, so an agent cannot add instructions without editing a note that already loads. That edit shows in the vault's git history.

---

## 3. Knowledge Namespaces

Every top-level folder other than `bootstrap/` and `archive/` is a knowledge namespace ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)).

### 3.1 Entry: `load_namespace(ns)`

- Returns the body of `<ns>/index.md`, the namespace's entry point, plus one tool-generated line telling the agent to pass `ns` to `search`.
- Keeps no state. The agent passes `ns` to later searches, which rank the namespace's notes higher without filtering out the rest ([07-interface-contracts.md §3-4](07-interface-contracts.md#3-load_namespacens)).
- A namespace without `index.md` returns its note list instead, and lint reports it.
- `bootstrap` cannot be loaded this way; `load_bootstrap()` loads it. `archive` can be loaded explicitly ([07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access)).

### 3.2 On-Demand Retrieval

All other knowledge notes are cold storage, queried through full-text search, vector search (optional feature), and graph walks. Results return compact skeletons first and full notes only on request ([03-markdown-format-and-edge-syntax.md §5](03-markdown-format-and-edge-syntax.md)). Bootstrap notes never appear in these results, and graph walks never enter the bootstrap.

---

## 4. Archive Store

### Archive Path Rule (DR1-08)

> **Rule:** Any note in the `archive` namespace (top-level folder `archive/`) is archive material, whatever its `type`.

- Routing by `type` would fail open: a misspelled `type: sesion_record` would land a raw transcript in knowledge and expose it to every standard search. The path rule fails closed.
- Archive notes are searched only on explicit request: a search that names `archive` as its namespace. `context` lists them marked `(archive)`, and agents only create them and append to them ([07-interface-contracts.md §1.1](07-interface-contracts.md#11-store-access)).
- **Graph-silent:** archive notes are full-text searchable, but the parser creates no observations and no edges from them. Raw transcripts would otherwise flood the graph with false observations and ghost links ([03-markdown-format-and-edge-syntax.md §3](03-markdown-format-and-edge-syntax.md)).

---

## 5. Harness Integration

### 5.1 Session Start

Two ways load the same content:

- **Instruction:** the harness instruction file says: "Session start: call `load_bootstrap()`, then `load_namespace('<project>')`."
- **Hook (preferred where supported):** a session-start hook runs `recognitor bootstrap`, and the harness adds its output to the context. Loading then does not depend on the model following an instruction. Claude Code supports such hooks.

### 5.2 Harness Instruction Files

Harnesses load their own instruction files (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, Cursor rules) and combine them with whatever Recognitor returns. Recognitor cannot change that order or precedence; its output usually carries less authority than the harness's own files. To avoid conflicts, each rule has exactly one home:

| Lives in harness instruction files | Lives in the Recognitor bootstrap |
| :--- | :--- |
| Repository facts shared with other contributors: build and test commands, code conventions, project rules | Who the user is and how to talk to the user |
| The pointer to Recognitor (§5.1) | Global rules that apply in every project |
| Harness-only mechanics: permissions, hooks, tool settings | |

A rule stored in both places drifts, and the two copies eventually conflict.

---

## 6. Observation Granularity & Open Categories

Recognitor treats the **Note** as the canonical file on disk, but treats individual **Observations** as indexed atomic units in SQLite:

```markdown
## Observations
- [fact] Access tokens expire after 15 minutes.
- [rule] Refresh tokens must be rotated on every exchange.
- [avoid] Storing refresh tokens in browser localStorage.
```

- **Open Vocabulary:** Users and models may author arbitrary brackets (`[fact]`, `[rule]`, `[avoid]`, `[decision]`, `[hypothesis]`, `[measurement]`).
- **Surgical Retrieval:** Agents can query specific rules or facts without loading the complete parent note into prompt context.
- **Storage:** the `observations` table in [05-database-schema-specification.md §2.3](05-database-schema-specification.md).

# Recognitor Interface Contracts

This document defines every MCP tool and CLI command: its inputs, its output, its token budget, and the queries it runs. After this document exists, every schema change must be justified by a contract here (DR1-15).

**Status:** complete for the first version (2026-11-15). Later changes are recorded here, and design reviews go to [06-design-reviews.md](06-design-reviews.md).

---

## 1. Principles

- **Stateless server:** the MCP server and the CLI keep no session state. Each call carries everything it needs. Parallel agents working from different namespaces share the database, never server state.
- **Transport:** `recognitor mcp` serves MCP over stdio. The client starts one server per session as a child process; there is no HTTP server and no daemon.
- **Namespace = entry point and perspective:** a namespace is where an agent enters the knowledge base and the angle it reads it from. It is not a boundary. Searches and graph walks span every knowledge namespace, and bootstrap and archive follow §1.1. The LLM decides when to follow links into other namespaces.
- **No isolation in early versions:** the per-session allowlist and sealed namespaces from DR1-07 are dropped. They can return later if real use shows a need.
- **MCP for every-session work, CLI for maintenance:** every MCP tool's schema costs context tokens in every session, so tools agents rarely need are CLI-only.
- **Plain text output:** tools return Markdown text. There is no JSON output mode.
- **Tokens are a cost, not a limit:** shorter text wins only while agents still use the tools correctly. When the benchmark or real use shows an agent misusing a tool because the text left something out, the text grows. A token count is a measurement, not a budget to meet. Where output is cut (search pages, long notes), the cut says how to get the rest.

### 1.1 Store Access

A note's store comes from its namespace: `bootstrap`, `archive`, or any other name for knowledge ([04-information-stores-and-lifecycle.md §1](04-information-stores-and-lifecycle.md#1-three-stores)). This table is the only place that says what each tool may do in each store; the other docs link here.

| Tool | bootstrap | knowledge | archive |
| :--- | :--- | :--- | :--- |
| `search` | Never | Yes | Only when `ns` is `archive` |
| `context` | Never listed | Yes | Listed, marked `(archive)` |
| `load_namespace` | Refused; the error points to `load_bootstrap` | `index.md` | `index.md` |
| `read_note` | Yes | Yes | Yes |
| Write tools | Allowed, with the bootstrap notice | Yes | Create and `append` only |

- **Write tools:** `write_note`, `edit_note`, `move_note`, `relink`, `delete_note`.
- **Bootstrap notice:** the tool result states that the note loads as instructions at session start ([04-information-stores-and-lifecycle.md §2.3](04-information-stores-and-lifecycle.md#23-writes)); after `delete_note`, that it no longer does (§10).
- **Archive is append only:** `write_note` without a checksum creates an archive note, and `edit_note` with `append` adds to one. Every other write to an archive note is refused. `relink` never reaches one, because archive notes have no edges ([04-information-stores-and-lifecycle.md §4](04-information-stores-and-lifecycle.md#4-archive-store)).
- **A move checks both stores:** the one the note leaves and the one it enters. A move out of the archive is refused, because it changes an archive note. A move into the archive counts as a create, so a knowledge note can be archived by moving it. A move into or out of the bootstrap is allowed and carries the bootstrap notice.
- **Explicit request:** doc 04 shows archive notes "only on explicit request". Passing `ns: "archive"` to `search` is one; reading a note that `context` marked `(archive)` is another, because a curated note linked to it.

---

## 2. Inventory

| Surface | Tools / commands |
| :--- | :--- |
| **MCP** | `load_bootstrap`, `load_namespace`, `search`, `context`, `read_note`, `write_note`, `edit_note`, `move_note`, `relink`, `delete_note` |
| **CLI** | `bootstrap [--list]`, `search`, `context`, `read`, `move`, `relink`, `ghosts [<link>]`, `trash empty`, `sync`, `rebuild`, `lint [--strict]`, `doctor`, `config get/set` |

- **CLI-only:** `ghosts` (backlog gardening is a human job, not an agent job).
- **Rejected: a `query` command for raw SQL.** The schema is a disposable projection that changes without migrations; exposing raw SQL would turn it into an API people depend on.
- **Rejected: a `--json` flag.**

### 2.1 What Agents See

Agents read this text in every session. How the tools fit together is said once, in the server's `instructions` field, which MCP sends when a session connects. Each tool then gets one or two lines. The server instructions do not repeat the session-start steps: those live in the harness file or the hook ([04-information-stores-and-lifecycle.md §5.1](04-information-stores-and-lifecycle.md#51-session-start)), and each rule has one home (§5.2 there).

**Server instructions:**

```
Notes are Markdown files; a note's id is its path without ".md". Use search to
find notes, context to see what links to a note, and read_note for its text.
Search and context show summaries only: read a note when you need its body.
A line ending in "page N" means more results exist.
```

**Tools:**

| Tool | Description | Arguments |
| :--- | :--- | :--- |
| `load_bootstrap` | Load the user's standing instructions: who they are, how to answer, rules for every project. Call once at session start, unless a hook already added them. | None |
| `load_namespace` | Load a namespace's entry note. Call once when work enters a namespace, then pass the same `ns` to `search`. | `ns`: the namespace name |
| `search` | Full-text search. Returns ids, summaries and matching facts, 5 per page. Notes with every word come first; when fewer than 5 do, notes with only some of the words fill the first page. No operators. `ns` ranks that namespace first without filtering; `ns: "archive"` also searches session records. | `query`: plain words. `ns`: from `load_namespace`. `page`: the number from the "page N" line |
| `context` | Show a note's links out and in, each with its summary, without reading the neighbors. Use it to pick what to read next. | `note`: an id, path or name. `page`: the number from the "page N" line |
| `read_note` | Return a note's full text with its checksum. `section` returns one section. A note over the token limit is cut to its best sections; the closing line says how to read the rest. | `note`: an id, path or name. `section`: heading text as written. `query`: words that choose which sections to keep when the note is cut |
| `write_note` | Create a note, or replace a whole note. Without `expected_checksum` it only creates; to replace a note, pass the checksum from `read_note`. To change part of a note, use `edit_note`. | `path`: from the vault root, `.md` optional. `content`: the whole file, frontmatter included. `expected_checksum`: the checksum of the version it replaces |
| `edit_note` | Change part of a note without rewriting it. `append` adds text at the end of a section, `add_observation` and `add_relation` add one line, `find_replace` replaces text that occurs exactly once, `replace_section` replaces a section and needs its checksum from `read_note`. | `note`: an id, path or name. `op`: one of the five. `text`: the text to add, the line, the replacement or the new section. `section`: heading text, for `append` and `replace_section`. `find`: the exact text to replace, for `find_replace`. `expected_checksum`: the section's checksum, for `replace_section` |
| `move_note` | Move or rename a note. Pass its checksum from `read_note`. Returns the links the move broke; show them to the user, who picks the notes `relink` fixes. Links inside the moved note are fixed with `edit_note`. | `note`: an id, path or name. `new_path`: from the vault root, `.md` optional. `expected_checksum`: the checksum of the version it moves |
| `relink` | Rewrite the links that pointed to a moved note, in the notes the user chose. A link that is already gone is skipped with a warning. | `old`: the note's id before the move. `new`: an id, path or name. `notes`: the linking notes, each an id, path or name |
| `delete_note` | Move a note to the vault trash. Pass its checksum from `read_note`. Returns the links that now point nowhere; show them to the user, who decides whether to fix them. | `note`: an id, path or name. `expected_checksum`: the checksum of the version it deletes |

- **Word for word:** the server sends this text exactly as written here. The retrieval benchmark ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)) can change it, through this section.
- **Write tools** get their lines here when they are built, under the same rules.
- **Read tools** (`load_bootstrap`, `load_namespace`, `search`, `context`, `read_note`) send `annotations.readOnlyHint: true`. The write tools send no annotations. A read may still refresh the index; it does not change a note.

**Errors:** this document calls each outcome either an error or a normal result. On the wire:

| Outcome | MCP | CLI |
| :--- | :--- | :--- |
| Error: `"x" not found`, `load_namespace("bootstrap")`, a query with no words, no such section, a refused write (§5.3) | A tool result with `isError: true`; its text is the message | `error: <message>` on stderr, exit 1 |
| Normal result: no hits, several matches (§6.3), a page past the end, no links, a move to where the note already is (§11) | A tool result with the text | The text on stdout, exit 0 |
| Bad arguments: `query` missing, `page` not a number, an argument the tool does not take | As an error | `error: <message>` and the usage text on stderr, exit 2 |
| Unknown tool: a name not in `tools/list` | A JSON-RPC error (`-32602`), as the MCP spec prescribes. Clients offer only listed tools, so agents do not reach it | No CLI twin: an unknown command is a bad argument |

- **Why a tool result:** each error message names the fix (call `load_bootstrap`, the list of sections), so it comes back as text the model reads and acts on. The flag tells the client the call failed, so a refusal never passes for a success.
- **No prefix over MCP:** the text is the CLI's message without the `error:` prefix; the flag already says it.
- **Argument messages:** they match the CLI's texts, without its dashes. A missing argument reads `<tool> needs <what>`: `search needs a query`, `read_note needs a note`, `load_namespace needs ns`. An argument of the wrong type reads `<argument> needs text`, or `page needs a number from 1`. New tools follow the same two patterns.
- **Two exceptions, `lint` and `doctor`:** their findings are a normal result on stdout even when something is wrong, and that verdict sets exit 1 (§12.2, §12.5). The findings are what the user asked for; the exit code is what a hook or CI reads.
- **Unknown arguments are errors:** `unexpected argument: <name>`, as the CLI says. Ignoring one would turn a typo such as `pages: 2` into page 1, a wrong result that looks right. The schemas do not mark extra arguments as forbidden: that would cost tokens in every session, and the error already teaches the agent.

---

## 3. `load_namespace(ns)`

Loads a namespace's entry point into the agent's context once. Replaces `enter_namespace`, whose name implied server-side state.

- **Input:** `ns`, the namespace name.
- **Output:** `<ns>/index.md`, found whatever its case (§6), as written with its frontmatter removed, then a blank line and one line generated by the tool: `Pass ns: "<ns>" to search.` When the content does not start with a level-1 heading line, `# <title>` is added above it, as in the bootstrap payload ([04-information-stores-and-lifecycle.md §2.2](04-information-stores-and-lifecycle.md#22-loading-load_bootstrap), step 5).
- **Spelling in the line:** `<ns>` is the namespace as its folder is spelled on disk, whatever casing the agent passed, so it matches the ids in later results (`Compass/Index`).
- **`## Relations` stays:** the bootstrap strips it because its walk loads the linked notes. `load_namespace` loads no other note, so the links are the agent's map of the namespace.
- **Clean data:** the hint line comes from the tool and is never stored in `index.md`.
- **State:** none. The LLM keeps the namespace in its context and passes it to later calls.
- **`bootstrap` is refused:** the error says to call `load_bootstrap` (§1.1): `bootstrap is not loaded with load_namespace: call load_bootstrap for the standing instructions`.
- **No such namespace:** when the index holds no note in `<ns>`, the tool returns an error that lists the namespaces that exist, so the agent can retry without another call (§5.3): `No namespace "nope". Namespaces: "archive", "baseline", "Compass".` Every namespace but `bootstrap` is listed, sorted by name ignoring case, each spelled as its folder is on disk. With nothing to list, the message ends after its first sentence.
- **No nested namespaces:** a namespace is a top-level folder, so a name with a `/` (`backend/guides`) gets the same "No namespace" error. `read_note("backend/guides/index")` still reads that note.
- **No `index.md`:** a namespace can exist without one, for example after `write_note` creates the first note in a new top-level folder. Storing is unaffected; only the entry point is missing, so only retrieval reports it:
  - `load_namespace` returns a normal result, not an error: the line `No index.md in "<ns>". Its notes:` first, so the agent knows why a list follows, then at most 10 notes, the size of a search page (§4.2), sorted by id ignoring case. Each note is `- <id>`, with its title added as in §4.2, and its summary on the next line, indented two spaces, when it has one. When more notes exist, one closing line points to search, since the tool has no `page` argument: `12 more. Search with ns: "Shelf" to find them.` `<ns>` is spelled as the folder is on disk, as in the hint line.

    ```
    No index.md in "Shelf". Its notes:
    - Shelf/Alpha
      Summary of Alpha.
    - Shelf/Beta
      Summary of Beta.
    12 more. Search with ns: "Shelf" to find them.
    ```

  - `lint` reports every namespace without an `index.md`, and warns when an entry point is not spelled `index.md`.
  - `write_note` stays silent: nothing is wrong at write time.
- **Query:** [05-database-schema-specification.md §3.2](05-database-schema-specification.md#32-namespace-entry-load_namespacens).

---

## 4. `search(query, ns?, page?)`

- **`ns` is a perspective, not a filter:** every knowledge note in the vault can match. Notes in `ns`, and notes linked from it, rank higher. Without `ns`, ranking is plain.
- **Stores (§1.1):** bootstrap notes never match. Archive notes match only when `ns` is `archive`.
- **Query:** [05-database-schema-specification.md §3.7](05-database-schema-specification.md#37-full-text-search-search).
- **Query text:** the agent's text is never passed to FTS5 as query syntax, so no input can cause a syntax error. Each word is wrapped in double quotes, with a `"` inside a word doubled. Text the agent put in double quotes stays one phrase, and an unmatched `"` is ignored. The words are joined with AND; for the fill in §4.2 ("Some of the words") they are joined with OR. So `cost-of-change` matches the phrase "cost of change", and `c++` matches `c`. There are no operators (`OR`, `NOT`, `*`). A word with no letter or digit is dropped, because it has nothing to match. A query with no words left is refused with an error that says so.
- **Responsibility:** the LLM passes the `ns` it got from `load_namespace`.
- **Page:** `page` starts at 1 and defaults to 1. It is the number the "more" line names (§4.2), so the agent copies it as is. The CLI takes it as `--page <n>`.
- **Boost values:** tuning parameters, set with the retrieval benchmark ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)).

### 4.1 Ranking

Most relevant first. The score combines:

| Part | Measures |
| :--- | :--- |
| Text match | SQLite FTS5 BM25 rank over the note's title, summary and text (`notes_fts`); observations count because they are part of the text |
| `ns` boost | Notes in `ns`, and notes it links to, rank higher |
| Vector similarity | Added once embeddings exist |

The score is not shown: it means nothing to the LLM and costs tokens.

### 4.2 Output

One block per hit:

```
1. recognitor/Decisions
   Durable decisions for the engine, newest first.
   - [decision] Writes take the SQLite write lock before touching a file
2. compass/concepts/Feedback loops
   What you build reacts to its own results.
3 more: page 2
```

- **Line 1:** the id. The title is added only when it differs from the filename by key, so `Index.md` titled `Index` shows no title. It reads `baseline/Tools and sourcing, titled "Tools & sourcing"`. Every tool that shows a note's id adds a title this way (§8 too), so parentheses stay free for markers such as `(archive)`.
- **Line 2:** the summary (at most 300 characters, [03-markdown-format-and-edge-syntax.md §2](03-markdown-format-and-edge-syntax.md#2-note-anatomy--layout)).
- **Below:** up to 3 observation lines that matched the query, best match first. Lines that contain every query word are shown; when none of the note's lines do, lines that contain any query word are shown instead. A line reads `- [category] @qualifier text`: the qualifier appears only when the fact has one, and tags stay in the text ([03-markdown-format-and-edge-syntax.md §2](03-markdown-format-and-edge-syntax.md#2-note-anatomy--layout)).
- **Paging:** 5 hits per page by default; the last line says how many more exist. Hit numbers continue across pages, so page 2 starts at `6.`. The last page has no "more" line. A page past the end prints one line, `Page 4 is past the end: 13 results on 3 pages.`, as a normal result, not an error. In a benchmark every expected note ranked within the top 5; the page size is a tuning parameter.
- **Kept matches:** the query keeps the best 50 matches ([05-database-schema-specification.md §3.7](05-database-schema-specification.md#37-full-text-search-search)). When more notes match, every page ends by saying so, after its "more" line if it has one: `45 more: page 2. Only the best 50 of 3120 matches are kept; narrow the query to reach the rest.` The total is counted only when the query returns 50 rows.
- **Some of the words:** when fewer than 5 notes match every word and the query has more than one word, notes that match any word fill page 1. They rank the same way among themselves and always below the notes with every word. The line `Notes with only some of the words:` stands before the first of them; when no note matches every word, it is the page's first line. The fill only completes page 1, so it never adds a "more" line or a page 2. Their fact lines are the any-word lines (Below), since none of their lines holds every word.
- **No hits:** one line, `No notes match "<query>".`, when no note matches even one word. It is a normal result, not an error.
- **Budget:** a page stays under 1,000 tokens (estimated as characters ÷ 4). Each fact line is cut at 200 characters, at a word boundary, and ends with `…`. Every hit on the page keeps its id and summary, and so do the closing lines. Fact lines are then added in rank order, the best hit's lines first, while the page stays under budget; once a line does not fit, the page gets no more fact lines. Both numbers are tuning parameters, set with the retrieval benchmark ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)).

---

## 5. Writes and Conflicts

Several agents and the CLI can write to the same vault at the same time. These rules prevent lost updates.

### 5.1 The Write Lock

SQLite protects its own data, but the notes are plain files outside it. Every write tool therefore uses SQLite's write lock as a lock between processes, explicitly:

1. `BEGIN IMMEDIATE`: take the write lock now. Other writers wait, up to `busy_timeout`.
2. Read the target file **from disk** and hash it. Never trust the index: a human may have edited the file inside the 2-second sync window. A create or a move instead reads each folder on the target path from disk and compares every name in it by key (§6). Any match on the last name refuses, even a file the index does not know yet, unless it is the note's own key (a case-only rename, §11). A folder that matches is reused with its spelling; twin folders refuse (§6).
3. Check the edit's guard (§5.2). On a mismatch: `ROLLBACK` and return the rejection (§5.3).
4. Apply the edit to the disk bytes, write a temporary file, rename it over the note. A create or a move uses a rename that fails when the target exists, so a file a human saves after step 2 is never overwritten.
5. Update the index rows, `COMMIT`.

- **Readers never wait:** in WAL mode ([05-database-schema-specification.md §4.1](05-database-schema-specification.md#41-per-connection-pragmas)), reads run on a snapshot while a write is in progress. Only writers queue behind each other. A snapshot may be a few milliseconds old; the guard catches that if the reader later writes.
- **Crash safety:** the rename cannot be rolled back. If the process dies after step 4, the file is already correct, and the next sweep repairs the index.

### 5.2 Guards: Each Edit Checks What It Overwrites

| Edit | Rejected when |
| :--- | :--- |
| `append`, `add_observation`, `add_relation` | Never: applied to the newest version |
| `relink` | Never: a link is rewritten only while it is still there, and skipped otherwise (§6.1) |
| `find_replace` (text X → Y) | X no longer occurs exactly once |
| `replace_section` | The section's checksum differs from `expected_checksum` |
| Create (`write_note` without `expected_checksum`) | A file or folder with the same key exists on disk (§5.1 step 2) |
| Overwrite (`write_note` with `expected_checksum`), `move_note`, `delete_note` | The note's checksum differs from `expected_checksum` |

- **No blind overwrites:** `write_note` without a checksum only creates. Overwriting always needs the checksum of the version being replaced.

Two agents editing different parts of the same note both succeed, with no re-read.

### 5.3 Rejections Return the Current Data

A rejection carries a short reason plus the current version of the part that clashed, so the agent can retry without a separate read:

| Rejected edit | Returns |
| :--- | :--- |
| `replace_section` | The section as it is now, with its checksum |
| `find_replace` | The whole note as it is now, with its checksum (X may have moved anywhere), cut at the read limit |
| Create or move onto an existing note, overwrite, `move_note`, `delete_note` | The whole note as it is now (for a move onto a note, the note already there), with its checksum, cut at the read limit |
| Create or move onto case twins | The path of every twin, without content |

The rejection never repeats the agent's own request: the agent already has it in context.

- **Cut like a read:** the current note is shown exactly as `read_note` shows it (§7), header line included. A note over the read limit is cut by §7.2; its header line still carries the whole-note checksum, which `move_note` and `delete_note` accept and `write_note` never uses to overwrite such a note (§11).
- **Archive and long-note refusals return nothing:** no version of the note would let the agent retry the same call (§1.1, §11), so the message names the fix instead.
- **Texts:** each refusal is a reason, then the current data:

| Refusal | Text |
| :--- | :--- |
| Create on an existing note | `The note already exists, so nothing was written. To replace it, call again with its checksum as expected_checksum. Current version:`, a line break, the note |
| Create on an existing note over the read limit | `The note already exists and is too long to replace whole, so nothing was written. Change it with edit_note. Current version:`, a line break, the note |
| Overwrite with a stale checksum | `The note changed since that checksum, so nothing was written. Current version:`, a line break, the note |
| Create onto case twins | `Names that differ only in case cannot exist together, so nothing was written. On disk:`, then one path per line, indented two spaces, a folder ending in `/` |
| Move onto case twins | `Names that differ only in case cannot exist together, so nothing was moved. On disk:`, then the paths as for a create |
| Move onto an existing note | `A note already exists at that path, so nothing was moved. Current version:`, a line break, the note already there |
| `move_note` on an archive note | `Archive notes are append only: move_note never moves one. Only the user can move the file.` |
| `move_note` with a stale checksum | `The note changed since that checksum, so nothing was moved. Current version:`, a line break, the note |
| `write_note` with a checksum on an archive note | `Archive notes are append only: write_note never overwrites one. To add to it, use edit_note with append.` |
| `write_note` with a checksum on a note over the read limit (§11) | `The note is too long to read whole, so write_note cannot replace it and nothing was written. Change it with edit_note, a section at a time.` |
| `delete_note` on an archive note | `Archive notes are append only: delete_note never removes one. Only the user can delete the file.` |
| `delete_note` with a stale checksum | `The note changed since that checksum, so nothing was deleted. Current version:`, a line break, the note |
| `find_replace`, `find` not in the note | `The text to replace was not found, so nothing was written. Current version:`, a line break, the note |
| `find_replace`, `find` more than once (overlaps count) | `The text to replace occurs N times, so nothing was written; it must occur exactly once. Current version:`, a line break, the note |
| `replace_section` with a stale checksum | `The section changed since that checksum, so nothing was written. Current version:`, a line break, the section as `read_note` shows it (§7.3) |
| `replace_section` text that does not start with a heading line | `The new section must start with its heading line, so nothing was written. Current heading: `, then the section's heading line as on disk |
| `add_observation` with a line that is not an observation | `That is not an observation line, so nothing was written. Give one line without its leading "- ", as [category] text, for example: [fact] Tokens expire after 15 minutes.` |
| `add_relation` with a line that is not a typed relation | `That is not a typed relation line, so nothing was written. Give one line without its leading "- ", as predicate [[Target]], for example: part_of [[Security]].` |
| An edit to a note that is not valid UTF-8 | `<id> is not valid UTF-8, so nothing was written.` |

### 5.4 Lint Lines in Write Results

A write that leaves a problem in the note says so right away, while the agent can still fix it. `lint` (§12.2) reports the same problems, but only when the user runs it, after the agent has moved on.

```
design/Caching  checksum: 3f9a2c41b7e0
lint: 2 warnings; the note is saved
- schema "concept": no [limit] observation
- line 8: summary paragraph is 366 characters; the limit is 300
```

| Write | Findings shown |
| :--- | :--- |
| `write_note`, create | Every finding on the new note |
| `write_note`, overwrite; `edit_note` | Only new findings |
| `move_note` | Only new findings, such as a link by name that now reaches another namespace without `ns::` |
| `relink`, `delete_note` | None: `relink` writes links it already resolved, and a delete leaves no note |

- **New:** the note had no finding with the same message before the write; line numbers are ignored. A message with a number in it, such as a summary's length, counts as new when the number changes, because the write touched that part. Findings with the same message count as one: if the note had it before, every copy after the write is old.
- **Before and after:** the note is checked twice inside the write lock (§5.1), and only the difference is shown.
  - Overwrite and `edit_note`: before is the text read from disk at step 2, the same text the guard checks, against the index as it stands before the write. After is the new text, once the note's index rows are updated.
  - `move_note`: before is the text at the old path against the old index; after is the same text at the new path. The path decides the namespace, the base of relative links and the entry point's spelling, so a move can add findings without changing a byte.
  - Check 6 reads the other note's `status` from its stored copy, so an edit made to it outside Recognitor counts from the next sweep.
- **Why only new:** an agent acts on what a tool tells it. Old findings would push it to edit parts of a note, or notes the user wrote by hand, that nobody asked it to touch. `lint` lists them for the user.
- **Checks:** those about one note: 1 to 6, 8 (the entry point's spelling, a Markdown link's case), a link to another namespace without `ns::`, and the type schemas, which skip archive notes as in [02-open-questions-and-tradeoffs.md §8](02-open-questions-and-tradeoffs.md#8-schema-validation-strategy-structural-linting--vault-wide-type-schemas-resolved). Check 6 reads this note's own `status` and `supersedes` lines against the other note in the index.
- **Not checked:** names (check 7), because a write refuses them (§6); a namespace without `index.md` and the bootstrap hint, because they are about the vault; and a `schemas.toml` that cannot be used, because it is about the file. With such a file, the schema checks are skipped and `lint` reports the file.
- **Shape:** `lint: <n> warning(s); the note is saved`, then one line per finding: `- line <n>: <message>`, or `- <message>` when the finding has no line. Findings with no line come first, then by line. The messages are those of §12.2, without the note id. "The note is saved" keeps the agent from reading the block as a refusal.
- **Place:** after the result, before the bootstrap notice, which stays last (§1.1). No block when nothing new was found, so a clean write costs no tokens, and no tool description changes.
- **No write, no block:** `Already there, nothing was written.` (§9) and every refusal (§5.3) carry no lint block.

---

## 6. Note Identity and Moves

Recognitor follows the Obsidian model: a note's address is its path. It replaced the earlier permalink identity on 2026-11-15; resolution rules are in [03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution).

- **Id = path:** a note's id is its path from the vault root without `.md` (`common/working-style/ADHD profile`). Output always shows it as written. The database stores its lowercase **key** instead ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)), and no output shows the key. Both come from the path, never from frontmatter, with no slug conversion, so nothing can drift.
- **Kept as written, matched in lowercase:** links, tool arguments and namespaces match a lowercase key of the path ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)).
- **ASCII names only:** every note and folder name is ASCII, so a key means the same on every platform. Other files (images, PDFs) are never indexed, so their names are not checked. A non-ASCII name is handled like a case twin in the table below.
- **Portable paths:** a path a write tool creates (`write_note`'s `path`, `move_note`'s `new_path`) is refused when a part is empty, `.` or `..`, or starts with a dot; when a part contains a control character or any of `\ : * ? " < > |`; or when a folder name ends in a space or a dot. `..` would leave the vault, hidden folders are never indexed (`.recognitor/` is the trash), `:` creates an alternate data stream on NTFS, and Windows drops a trailing space or dot, so two folder names would land on one folder. The refusal reads `The path is not a note path. Use folder/name from the vault root, with no empty, hidden or ".." parts and none of \ : * ? " < > |.`
- **No case twins:** two paths with the same key are an error at any level, folders included (`common/ADHD profile.md` and `common/adhd profile.md`; `Baseline/` and `baseline/`). The same name in different folders is fine (every namespace has an `index.md`).

| Where | What happens with a case twin or a non-ASCII name |
| :--- | :--- |
| `write_note`, `move_note` | Refused when the key already exists; the error returns the existing note. Refused when the path has a non-ASCII character. A new path reuses the spelling of folders that already exist (`Baseline/x.md` lands in `baseline/`) |
| Files created outside Recognitor (Linux, git) | One or none: none of the twins is indexed. `sync` and `rebuild` print a warning naming every twin and exit 0, so the rest of the vault stays indexed and usable; `lint` reports it as an error and fails. Twin folders leave out every note under both. A non-ASCII name is left out and reported the same way. The files stay on disk, and the first sweep after a rename indexes them |
| A read, an edit or a link that hits the name | Treated as a note that does not exist: "not found", or a plain ghost. Twins never enter the system and nothing records them |
| A create or a move onto the name | Refused, with the path of every twin: the check reads the disk, not the index (§5.1 step 2) |

- **Rejected: indexing one twin.** Picking a winner by sort order or by index age guesses which file the user meant, and the other one silently drops out of search.
- **Rejected: indexing both twins.** Links and tool calls match by key, so they cannot tell the two apart.
- **`permalink` is not indexed:** the frontmatter field has no meaning for Recognitor and is left untouched. Obsidian Publish reads it as the page's web address; Basic Memory reads it as its own id.
- **Maintenance is the user's job:** Recognitor does not work around broken links. It reports them and offers tools to fix them, and the user decides.

### 6.1 Moving a Note

An MCP tool cannot pause mid-call to ask the user, so a move takes two calls:

1. `move_note(note, new_path, expected_checksum)` moves the file and returns the links that are now broken.
2. The agent shows the list to the user, who picks which linking notes to update.
3. `relink(old, new, notes)` rewrites the links to the moved note in the chosen notes. One write lock (§5.1) covers the call; each note is read from disk, patched and renamed on its own.

- **CLI:** `recognitor move` runs both steps in one command and asks the user in between (§12.1). The steps are two lock periods, not one transaction.
- **Broken means changed by the move, whatever the link looks like:** under the write lock, `move_note` resolves the affected links before and after the move: the links whose key matches one of the note's names, and the note's own links. It reports every link whose target changed or became a ghost:

| Link | Breaks because |
| :--- | :--- |
| `[[b/Caching]]`, in an `ideas` note | The path changed |
| `[[ideas::Caching]]` | The namespace changed |
| `[[Caching]]` | The name now matches two notes |
| `[[./Setup]]` inside the moved note | It now starts from the new folder |

- **Links inside the moved note** are fixed with `edit_note` (`find_replace`), because `relink` rewrites only links that point to the moved note.

**Which links `relink` rewrites:** in each chosen note, the links that pointed to the note at `old`. A link qualifies when it resolves to the note as if the note still were at `old`, and does not resolve to `new` now. A key match alone is not enough: in `ops/Runbook`, `[[Caching]]` can point to `ops/Caching` while `[[ideas::Caching]]` points to the moved note.

**The new link is written by the link rules** ([03-markdown-format-and-edge-syntax.md §4, "Writing a link"](03-markdown-format-and-edge-syntax.md#writing-a-link)): wherever the note moved, the link is set to reach it from the linking note. A wikilink, name or path, takes the form that table gives; the alias, `#heading`, `!` and a written `.md` stay. The name follows the file: a name that still names the note (its title, or its filename after the move) stays as written, and any other name becomes the note's new filename.

| Old wikilink (name, prefixed or path) | New link, after `ideas/Caching` moves to `design/b/Caching` |
| :--- | :--- |
| Linking note outside `design`, `Caching` unique in `design` | `[[design::Caching]]` |
| Linking note inside `design`, `Caching` unique in `design` | `[[Caching]]` |
| Linking note outside `design`, `design/a/Caching` also exists | `[[design::b/Caching]]` |
| Linking note inside `design`, `design/a/Caching` also exists | `[[b/Caching]]` |

- **Relative links keep their form:** `[[../ideas/Caching]]` and `[text](../ideas/Caching.md)` get the relative path from the linking note to the new file, percent-encoded in a Markdown link. A Markdown link that starts with `/` keeps its path from the vault root.
- **Moved and renamed:** after `cognition/index` moves to `recognitor/cognition-index`, `[[Index]]` in `cognition/hubs/Storage` becomes `[[recognitor::cognition-index]]`, and `[[cognition::index]]` in `recognitor/index` becomes `[[cognition-index]]`.
- **No form, no rewrite:** when the note now sits directly in its namespace folder and its name repeats in that namespace, no link names it alone ([03-markdown-format-and-edge-syntax.md §4, "Writing a link"](03-markdown-format-and-edge-syntax.md#writing-a-link)). `relink` leaves the link as written and warns once per linking note: `warning: skipped, no unambiguous link to design/Caching: ops/Runbook`.

**`relink` never rejects.** Each linking note is handled on its own. Rewriting a link replaces only the link text, so it cannot overwrite anyone's work and needs no checksum. Only `new` must match one note, and `old` must be an id with its folder; otherwise nothing is written. No match for `new` and a bad `old` are errors; several matches for `new` return their ids (§6.3):

| Case | Result |
| :--- | :--- |
| The old link is still there | Rewritten |
| The old link is gone (someone already edited it) | Skipped, and reported as a warning |
| A `notes` entry matches no note, or several | Skipped, and reported as a warning; for several, the warning lists their ids (§6.3) |
| An archive note, a file that cannot be read, is not UTF-8 or cannot be replaced | Skipped, and reported as a warning |
| No link names the moved note alone (no form, above) | That link is left as written, and reported as a warning |
| A link whose target is not written inside it (a reference-style Markdown link) | Skipped, and reported as a warning naming the link |

```
relinked: 4 links in 3 notes
warning: skipped, link already gone: recognitor/Sync design
notice: bootstrap/Profile loads as instructions at session start
```

- **Bootstrap notice:** one `notice:` line per bootstrap note written (§1.1).
- **Links by name usually survive:** `[[Caching]]` keeps resolving after a move unless the name now matches two notes, so the list is usually short.
- **Links in unchosen notes** become ghosts and show up in `recognitor ghosts`.

### 6.2 Moves Outside Recognitor

A move in Obsidian, Explorer or git looks to the sweep like one file deleted and one file created. The sweep does not report it: the sweep usually runs inside another tool call, whose output has no place for the report. The links the move broke become ghosts instead, and `recognitor ghosts` shows where the note probably went:

```
ideas::Caching   3 links   → maybe moved to design/Caching
Gall's law       2 links
```

- **The hint** is worked out when the command runs, so nothing is stored. It shows only when exactly one note has the ghost's filename (`notes.name_key`); for a shared name such as `index`, it shows nothing.
- **Only path and prefixed links get a hint.** A bare name that matched a note's filename would have resolved ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)), so a bare-name ghost never has one. In a vault of bare-name links, `ghosts` lists what to fix, but cannot say where a note went.
- **Fix:** `ghosts <link>` lists the linking notes and, with a hint, prints the `recognitor relink` command (§12.3); or `relink` through an agent (§6.1).

### 6.3 Notes Named in Tool Calls

A tool argument that names a note (`read_note`, `context`, `edit_note`, `move_note`, `delete_note`, and `relink`'s `new` and `notes`; its `old` is the old id, which no note has any more) accepts an id, a path or a link name, matched by key. It resolves like a link ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)), but with steps 1, 2 and 4 only: a tool call has no linking note, so step 3 has no namespace to prefer.

| Matches | Result |
| :--- | :--- |
| One | The tool runs |
| Several | The tool runs on none of them and returns their ids, sorted, as a normal result; the agent calls again with one |
| None | An error: `"<name>" not found`, quoting the name as given |

```
read_note("index")
→ "index" matches 3 notes. Call again with one id:
  common/index
  compass/Index
  baseline/index
```

`compass::index` (step 2) or the id itself always matches one note.

---

## 7. `read_note(note, section?, query?)`

Returns one note. Predictable by design: the content always comes from that one file.

- **Input:** `note` (an id, path or link name, §6.3), optional `section` (a heading), optional `query` (used only when the note is too long, §7.2). The CLI takes them as `--section <heading>` and `--query <text>` (§12).
- **Output:** the whole file exactly as on disk, frontmatter included, so an overwrite built from it never loses tags or type. The tool adds one header line above it: the id, with its title when it differs (§4.2), two spaces, and the checksum:

```
recognitor/Decisions  checksum: 3fa9c1e2b7d0
---
type: hub
---
# Decisions
...
```

- **Sections:** a section is a heading and everything under it, up to the next heading of the same or a higher level, so its subsections stay inside it. Only headings at the top level of the note count; a heading inside a list item or a quote is text. A section's name is its heading text as written, without the `#` marks or setext underline, inline markup kept: ``## The `foo` flag`` is named ``The `foo` flag``. The note's first heading is its title, not a section, when it is an H1, as for the indexer's title (doc 03 §2). A note whose first heading is `##` or lower has no title heading, so every heading in it is a section, and a later `#` heading is a top-level section. Everything before the first heading after the title is the **head**: frontmatter, title, summary and any other text there. A **top-level section** is one that is not inside another. `section` can name a heading at any level. Headings are found with the parser the indexer uses, so a heading-shaped line inside code is text ([03-markdown-format-and-edge-syntax.md §3](03-markdown-format-and-edge-syntax.md#code-is-never-parsed)). The `## Relations` section (doc 03 §3) follows the same rule.

### 7.1 Short Checksums

Tools show and accept the first 12 hex characters of the note's SHA-256 checksum instead of all 64. Twelve characters are enough to detect a change to one note, and they save about 30 tokens on every read and edit. The database keeps the full hash.

### 7.2 Long Notes

A safety net for huge notes such as session transcripts. The average note in the reference vault is about 600 tokens (estimate), so normal notes are never cut.

- **Limit:** `read.max_tokens` in the config, default 4,000.
- **What counts:** the whole output, header line and closing line included, as in search and `context`. A note is cut only when its whole output would be over the limit.
- **A head over the limit:** the frontmatter stays whole. The rest of the head follows block by block (the title, paragraphs, lists, code blocks), each whole and in order, while it fits. The first block that does not fit ends the head, and the blocks after it are not shown; they cannot be read in parts. Sections that fit are still filled in after a head that ended early. When the note has no sections at all, the closing line says so: `[cut at 4000 tokens. The rest has no headings, so it cannot be read in parts.]`
- **Always kept:** the head (§7): frontmatter, title, summary and any other text before the first heading. The frontmatter tells the agent what kind of note it is (`type`, `tags`, `status`).
- **Then:** the top-level sections that best match `query`, whole, until the limit. Without `query`, sections are taken in file order.
- **Ranking:** `query` follows search's query text rules (§4). Sections that contain every query word come first, then sections that contain any of them, then the rest in file order, as §4.2 picks observation lines. Within each group, the better match comes first by the BM25 rank search uses, with the same tokenizer, so a word matches its other forms as it does in search. The ranking runs on a temporary in-memory table built at read time; nothing is stored. A query with no words left counts as no query, not as an error, because it only guides the cut.
- **Fill:** sections are taken in rank order. A section that does not fit is skipped and the next one is tried, so one large section does not keep smaller ones out. A section is never cut; a skipped one is named in the closing line.
- **Shown in file order:** sections are picked by relevance but displayed in their original order, so the note still reads naturally.
- **Closing line,** added by the tool, never part of the note. One line in brackets, right after the shown text, with no blank line before it. It is built from the parts below, in table order; each part appears only when it applies, and each part but the last ends with a period:

```
[cut at 4000 tokens. Shown, with checksums: "Storage" 9b1e04c2aa71, "Search" 51c0e3f2d8aa. Not shown: "Index", "Notes". Read one with section: "Index"]
```

| Part | When |
| :--- | :--- |
| `cut at 4000 tokens.`: the limit in plain digits, as in the bootstrap warning | Always |
| `Shown, with checksums: "Storage" 9b1e04c2aa71, …`: shown sections in file order, each with its section checksum (§7.3) | Some section is shown |
| `Not shown: "Index", "Notes". Read one with section: "Index"`: best match first (file order without `query`); the hint names the first | Some section is not shown |
| `Part of the head is not shown; it cannot be read in parts.` | Head blocks were left out and the note has sections |
| `The rest has no headings, so it cannot be read in parts.` | The note has no sections |

  Sections are named by their heading text as written, numbered when siblings share a name (§7.3). Only top-level sections are listed, so each name is also its path.
- **Whole-note checksum on a cut read too:** the header line carries the note's checksum as on a whole read, and the closing line lists each shown section's checksum for `replace_section`. `move_note` and `delete_note` keep the whole file, so a partial copy is enough to guard them. `write_note` never overwrites a note over the read limit (§11), so an agent can never replace the whole note from a partial copy and silently delete the sections it did not see.

### 7.3 One Section

With `section`, the tool returns that section only, with the section's checksum in the header line, ready for `replace_section` (§5.2).

```
recognitor/Decisions > Storage  checksum: 9b1e04c2aa71
## Storage
Text…
### Write lock
Text…
```

- **Header line:** the id (as in §4.2, so a titled note reads `a/N, titled "T" > Storage  checksum: …`), ` > `, the section's path from its top-level section (`Storage`, `Storage > Write lock`), two spaces, and the checksum. The path is the one `section` takes back.
- **Body:** the section exactly as on disk, from the start of its heading line to the start of the next heading of the same or a higher level, or the end of the file. Subsections and trailing blank lines are included.
- **Section checksum:** the first 12 hex characters (§7.1) of the SHA-256 of exactly the body's bytes. `read_note` and the `replace_section` guard (§5.2) compute it the same way, so an edit inside the section changes it and an edit outside it does not.
- **A section over the limit:** cut like a note (§7.2), one level down. Its head is its heading line and the text before its first subsection; its subsections are ranked by `query` and filled as top-level sections are. The header line carries no checksum, and the closing line names subsections by their paths: `Not shown: "Verbatim > Day 2". Read one with section: "Verbatim > Day 2"`.

- **Matching:** `section` names a heading by its text, compared in lowercase and without its `#` marks, as note names are (§6.3): `"storage"`, `"Storage"` and `"## Storage"` all match `## Storage`. A plain name matches a heading at any level. A path of headings from a top-level section through its direct children, joined by ` > `, names a heading inside others: `"Storage > Notes"`. When sibling sections share a name (compared in lowercase), each gets its number among them, from 1 in file order: `Notes [1]`, `Notes [2]`; a unique name stays plain. Numbered names work in paths (`"Storage > Notes [2]"`), and every list of sections shows them, so any name shown can be passed back. ` > ` is used because headings often contain `/` (`## Read/write`).
- **Several matches:** a normal result that returns no section and lists each match by its path, so the agent calls again with one. The id is shown as in §4.2:

```
"Notes" matches 2 sections in a/N. Call again with one:
  Storage > Notes
  Search > Notes
```

- **No match:** an error that lists the note's top-level sections, so the agent can retry without another read (§5.3): `No section "Storge" in a/N. Sections: "Storage", "Search".` A note with no sections at all says so instead: `No section "Storge" in a/N. The note has no sections.`

---

## 8. `context(note, page?)`

Shows what surrounds a note without reading its neighbors.

```
recognitor/Decisions

Links out:
- part_of: recognitor/index
  Active memory project for the Recognitor codebase.
- relates_to: compass/concepts/Feedback loops
  What you build reacts to its own results.
- relates_to: Gall's law  (not written yet)
- relates_to: archive/recognitor/Session 2026-11-14  (archive)
  Full record of the write-lock design session.

Links in:
- links_to, from recognitor/Sync design
  How the index stays in step with the files.
```

- **One level only, no `depth` argument:** to go further, the agent calls `context` again on a neighbor. The LLM decides where to jump (§1).
- **Each link:** predicate, id (with its title when it differs, as in §4.2), and summary. Ghosts are marked `(not written yet)`, archive notes `(archive)`.
- **Order:** outgoing links in written order; incoming links most recently changed first.
- **One fact, one line:**
  - An inverse pair (`has_part` in A, `part_of` in B) is shown once, as the link the note itself wrote, under links out. So `context(A)` shows `has_part: B` and `context(B)` shows `part_of: A`.
  - A link from the note to itself is shown once, under links out.
  - Two links with the same predicate that resolve to the same note (`[[Caching]]` and `[[research/Caching]]`) are one line; the first written stays.
- **Stores (§1.1):** bootstrap notes are never listed ([05-database-schema-specification.md §3.3](05-database-schema-specification.md#33-two-stage-graph-discovery)). Archive notes are listed with their marker. Any note can be the note asked about (§6.3), a bootstrap note included; its links to and from other bootstrap notes stay hidden.
- **Page:** as in search (§4): `page` starts at 1 and defaults to 1, it is the number the "more" line names, and the CLI takes it as `--page <n>`.
- **Limit:** up to 20 links per page, and a page stays under 1,000 tokens (estimated as characters ÷ 4). Links out come first, then links in, and a page fills across both. A link is never cut: a link that does not fit with its summary starts the next page, so a page of long summaries holds fewer links. Each page starts with the note's id and shows the `Links out:` and `Links in:` headings for the links it holds. Both numbers are tuning parameters, set with the retrieval benchmark ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)).
- **Closing lines:** as in search (§4.2). While more links exist, the last line is `5 more: page 2`; the last page has none. A page past the end prints one line, `Page 3 is past the end: 25 links on 2 pages.`, as a normal result, not an error.
- **No links:** a note with no links to list prints one line, `No links to or from a/Alone.`, with the id shown as in §4.2. It is a normal result, not an error, on every page number.

---

## 9. `edit_note(note, op, ...)`

Changes part of a note without rewriting it. Each op patches the file's bytes in place; Markdown is never regenerated from the parse tree. Guards and rejections follow §5.2 and §5.3; store rules follow §1.1.

| Op | Does |
| :--- | :--- |
| `append` | Adds text at the end of a named section. Without a section, at the end of the main text, just before `## Observations` |
| `add_observation` | Adds a line under `## Observations`, creating the section if needed. Skipped if the same line already exists |
| `add_relation` | Adds a line under `## Relations`, the same way |
| `find_replace` | Replaces text X with Y; X must occur exactly once. When Y is empty and X was a whole line, the line is removed |
| `replace_section` | Replaces one whole section. Empty content removes the section, heading included |

Every edit case maps to one op:

| Edit case | Op |
| :--- | :--- |
| Add text to a section, or add a new section | `append` |
| Add an observation or a relation | `add_observation`, `add_relation` |
| Insert in the middle of a section | `find_replace` (X becomes X plus the new text) |
| Change a small piece of text, frontmatter included | `find_replace` |
| Delete text, an observation or a relation | `find_replace` to empty |
| Rewrite or delete a section | `replace_section` |
| Rewrite the whole note | `write_note` with a checksum (§5.2) |

- **Rejected: `prepend`.** The top of a note is frontmatter, title and summary, so there is no sensible place to insert; `find_replace` covers the rare case.
- **Arguments:** as in §2.1. `section` names a heading as `read_note`'s does (§7.3): matching, several matches and no match work the same.
- **Observation and relation lines:** `text` is the line without its leading `- `, which the tool adds: `[fact] Tokens expire after 15 minutes.`, `part_of [[Security]]`. A line that is not an observation ([03-markdown-format-and-edge-syntax.md §2, "Observation Syntax"](03-markdown-format-and-edge-syntax.md#observation-syntax)) or a typed relation (doc 03 §3, Tier 1) is refused: it would sit in the note as a plain list item and never be indexed.
- **Where new text goes:** `append` without `section` inserts before `## Observations`; without it, before `## Relations`; with neither, at the end of the note. A `## Observations` that `add_observation` creates goes before `## Relations`, or at the end; a `## Relations` that `add_relation` creates goes at the end.
- **Result:** `<id>  checksum: <12 hex>`, as `write_note` (§11), plus the lint block (§5.4) and the bootstrap notice (§1.1).
- **Already there:** `add_observation` or `add_relation` with a line the section already has writes nothing and returns a normal result: `Already there, nothing was written.`, a line break, then the result line. The lines are compared after trimming, in the section's own text, subsections excluded. No bootstrap notice, because nothing was written.
- **Archive:** any op but `append` on an archive note is refused: `Archive notes are append only: use append to add to one.`
- **Argument messages:** `edit_note needs a note`, `edit_note needs an op`, `edit_note needs text`, `edit_note needs find`, `edit_note needs section`, `edit_note needs expected_checksum`; an unknown op gets `op needs one of append, add_observation, add_relation, find_replace, replace_section`. An argument the op does not take is `unexpected argument: <name>` (§2.1), so `section` with `add_observation` cannot put the line somewhere unintended. An empty string counts as not given, because models fill optional arguments with `""`.
- **Empty `text`:** `append`, `add_observation` and `add_relation` need text. For `find_replace`, empty `text` deletes the found text; whitespace is a replacement like any other, so `foo  bar` can become `foo bar`. For `replace_section`, `text` that is empty or only whitespace deletes the whole section.
- **`replace_section` text** is the whole new section, heading included. Text that does not start with a heading line is refused (§5.3), because its body would merge into the section above.
- **Spacing:** `append` puts the text after the section's last content line, subsections included, with one blank line above it; when that line and the new text are both list items, the text joins the list with no blank line. The blank lines that followed stay after the new text. `add_observation` and `add_relation` put the line at the end of the section's own text, before any subsection.
- **Line breaks:** a line break in `find` matches `\n` or `\r\n` in the note, and new text is written with the note's line breaks, so an agent never has to type `\r`.

---

## 10. `delete_note(note, expected_checksum)`

- **Vault trash, not a hard delete:** the file moves to `.recognitor/trash/<timestamp>/<original path>`. A move inside the vault works the same on every platform and drive, and it cannot half-fail. The timestamp folder keeps two deletes of the same path apart and records when each happened.
- **Never indexed:** `.recognitor/` is a hidden folder ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)).
- **Timestamp:** UTC to the second, with dashes for colons: `2026-11-18T19-18-52Z`. Windows refuses `:` in a name, and the folders sort by time. A second delete of the same path in the same second gets `-2`, then `-3`.
- **Folders stay:** a folder the delete leaves empty stays in place. Removing it would race with a human working in it, and git ignores empty folders anyway.
- **Undo:** move the file back. `recognitor trash empty` removes `.recognitor/trash` (§12.4). `delete_note` creates the directory again on the next delete.
- **Output:** the note's id, its trash path, and the links the delete broke. They become ghosts, and the user decides whether to clean them up.

```
deleted: ideas/Caching
trash: .recognitor/trash/2026-11-18T19-18-52Z/ideas/Caching.md
broken links: 3 links in 2 notes
- ideas/index: indexes Caching  (ambiguous)
- ops/Runbook, titled "Runbook v2": links_to ideas::Caching
- ops/Runbook, titled "Runbook v2": relates_to ../ideas/Caching
```

- **Broken means changed, as in §6.1:** a link that now reaches no note is listed as it is; a name that now matches several notes ends with `  (ambiguous)`; a link that now resolves to another note ends with `  (now <id>)`. A redirected link is listed because it still resolves, so `recognitor ghosts` never shows it.
- **One line per link:** the linking note's id (with its title when it differs, as in §4.2), the predicate, and the target as written, the text a `find_replace` fix needs. The count line is worded as in `relink` (§6.1). With nothing broken it reads `broken links: none`. Lines are sorted by linking note id ignoring case, then by line, predicate and target as written.
- **No cap:** every line is a decision for the user, and a cut would hide exactly those lines. Deleting a heavily linked hub gives a long result.
- **Bootstrap notice:** last, as `notice: <id> no longer loads as instructions at session start` (§1.1).
- **Guard:** the whole-note checksum (§5.2). `expected_checksum` is required: a missing or empty one gives `delete_note needs expected_checksum` (§2.1).
- **Long notes:** deleted like any other. A cut read shows the whole-note checksum (§7.2), and the trash keeps the whole file.
- **Order of checks:** archive (§1.1), checksum. Refusal texts: §5.3.
- **Stores:** §1.1.
- **Rejected: the OS trash.** Windows skips the Recycle Bin on network drives, macOS asks permission to control Finder, and the Linux trash can fail on servers and other mounts.

---

## 11. `write_note(path, content, expected_checksum?)` and `move_note(note, new_path, expected_checksum)`

Both are built from the rules above.

| | `write_note` | `move_note` |
| :--- | :--- | :--- |
| **Input** | `path` from the vault root (`.md` optional), `content` (the whole file, frontmatter included), optional `expected_checksum` | `note`, `new_path`, `expected_checksum` |
| **Without checksum** | Create only; rejected if the note exists (§5.2) | Not allowed |
| **Refused when** | The key already exists (§6), the path has no top-level folder, the path has a non-ASCII character, or the path is not portable (§6). An overwrite is also refused when the note on disk is over the read limit (§7.2) | The target's key already exists, unless it is the note's own key: a case-only rename is allowed and breaks no links (§6). Also when `new_path` has no top-level folder or a non-ASCII character, or is not portable (§6), and when the note is in the archive (§1.1) |
| **Output** | `<id>  checksum: <12 hex>`, then the lint block (§5.4) | The new id, and every link the move broke, with the `relink` call for links into the note; links inside it are fixed with `edit_note` (§6.1) |
| **Folders** | Created as needed; existing folders keep their spelling (§6) | Created as needed; existing folders keep their spelling (§6) |

- **Stores:** the bootstrap notice and the archive limits follow §1.1.
- **Checksum, but no note:** `write_note` with `expected_checksum` on a path where no note exists is the §6.3 error `"<path>" not found`, quoting the path as given. It never creates, so a typo in the path cannot make a new note.
- **Empty `content` is refused:** content that is empty or only whitespace gets `write_note needs content` (§2.1). An empty overwrite would remove a note without the trash; that is `delete_note`'s job (§10).
- **`expected_checksum: ""` counts as absent:** models often send an empty string for an optional argument, and a create never overwrites, so this is safe. Any other value is the 12-character form (§7.1), matched ignoring case.
- **No title in the result:** the output shows the id alone, unlike §4.2, because the agent just wrote the title.
- **Long notes:** `write_note` never overwrites a note over the read limit (§7.2). The agent has seen only part of it, and a whole-note write would delete the rest. `edit_note` changes it a section at a time. Creates, `move_note` and `delete_note` are unaffected. Order of checks for an overwrite: not found (§6.3), archive (§1.1), read limit, checksum.

**`move_note` output:** the move line, the broken links, the `relink` call, the lint block (§5.4), and the bootstrap notice.

```
moved: ideas/Caching -> design/b/Caching
broken links: 3 links in 2 notes
- design/b/Caching: links_to ./Setup
- ops/Runbook, titled "Runbook v2": links_to ideas::Caching
- ops/Runbook, titled "Runbook v2": relates_to ../ideas/Caching
relink(old: "ideas/Caching", new: "design/b/Caching", notes: ["ops/Runbook"])
```

- **Broken links:** the block is `delete_note`'s (§10): one line per link, the `(ambiguous)` and `(now <id>)` markers, `broken links: none` when nothing broke, no cap, and the same order.
- **The moved note's own links** are listed under its new id. They are fixed with `edit_note` (§6.1), so they never go in the `relink` call.
- **The `relink` call:** one line, ids without titles. `notes` holds every listed note whose link pointed to the moved note; the user picks from it (§6.1). The line is left out when no such note is listed. A link that went to another note and now reaches the moved note is listed with `(now <new id>)`, but it is not in the call: `relink` skips a link that already resolves to `new`.
- **Not listed:** a link that reached no note before and reaches the moved note now. The move fixed it, as a create does.
- **Bootstrap notice:** last. A move into or inside the bootstrap gives `notice: <new id> loads as instructions at session start`; a move out gives `notice: <old id> no longer loads as instructions at session start` (§1.1).
- **Into the archive:** allowed as a create (§1.1). The note's own links leave the graph, because archive notes have no edges. They are not listed, because their text did not change.
- **Same path:** a normal result, not an error: `The note is already at <id>, so nothing was moved.` A new spelling of the same key is a case-only rename, the extension included (`Note.MD` to `Note.md`).
- **`new_path`:** from the vault root, `.md` optional in any case.
- **Case-only rename:** the file system may ignore case, so the file passes through a hidden temporary name next to it, `.<name>.recognitor-move`. A crash between the two steps leaves the note under that name; nothing is lost, and renaming it back restores it.
- **Order of checks:** the note (§6.3), archive (§1.1), the path (§6), the target on disk (twins, an existing key, the same path), checksum.
- **Argument messages:** `move_note needs a note`, `move_note needs new_path`, `move_note needs expected_checksum`. An empty string counts as not given, as in §9.
- **Refusal texts:** §5.3.
- **Path refusals:** `The path has no top-level folder. Write it from the vault root, as folder/name.` and `The path has a non-ASCII character. Note and folder names are ASCII only.`, plus the portable-path text (§6). Other refusals: §5.3.
- **On the CLI:** `recognitor move` prints this block with two lines changed (§12.1).

---

## 12. CLI

The CLI has no `write`, `edit` or `delete` commands. A human edits notes in an editor or Obsidian and the next sweep picks the changes up; agents write through MCP. The CLI offers only what a human cannot do by hand. Its output is the same Markdown text as the MCP tools; `move` alone changes two lines (§12.1).

| Command | Why it exists |
| :--- | :--- |
| `bootstrap [--list]`, `search`, `context`, `read` | Inspect what agents see; the session-start hook runs `bootstrap` ([04-information-stores-and-lifecycle.md §5.1](04-information-stores-and-lifecycle.md)) |
| `move`, `relink` | A move needs the link check and `relink` (§6.1); a move made outside Recognitor needs `relink` alone (§6.2) |
| `ghosts [<link>]`, `lint [--strict]`, `doctor`, `trash empty` | Maintenance |
| `sync`, `rebuild`, `config get/set` | Engine control |

- **Same arguments as MCP:** a CLI command takes its MCP tool's arguments by the same names. The first required argument is positional, and every optional one is an option with its name: `search <query> --ns <name> --page <n>`, `context <note> --page <n>`, `read <note> --section <heading> --query <text>`. A tool with several required arguments takes them all as positionals, in order, a list last: `relink <old> <new> <note>...`. The one exception is `move`, which takes no `expected_checksum` (§12.1).
- **An option on the wrong command is a bad argument** (§2.1), never ignored: ignoring `--page 2` on `read` would return a wrong result that looks right. The message names the commands that take it: `--ns belongs to search`, `--page belongs to search and context`, `--section and --query belong to read`, `--relink belongs to move`, `--list belongs to bootstrap`, `--strict belongs to lint`, `--report belongs to sync and rebuild`.
- **Which vault:** the `--vault <path>` flag, else the `RECOGNITOR_VAULT` environment variable, else `vault.path` in the config file. With none of them set, the command fails with an error that names all three. Sessions, hooks and the MCP server start in code repos, not in the vault, so the vault is never guessed from the current folder. The MCP server finds its vault the same way.
- **Config file:** `config.toml` in the per-user local config folder, `ProjectDirs::config_local_dir()` from the `directories` crate (Windows: `%LOCALAPPDATA%\recognitor\config\config.toml`). Local, not roaming, because a vault path belongs to one machine. The `RECOGNITOR_CONFIG` environment variable points at another config file, for tests and portable setups. `config get/set` reads and writes it and keeps comments and unknown keys; keys follow the `section.name` style ([05-database-schema-specification.md §5](05-database-schema-specification.md#5-database-file-location)). Harness entries are `[harness.<name>]` tables in that same file. They are not `config get` or `config set` keys: those commands leave a harness table as it is, and `search` does not read it. A TOML syntax error still fails every command that parses the file. What the tables hold, and how `doctor` reads them, is §12.5.

`load_bootstrap` behaves as specified in [04-information-stores-and-lifecycle.md §2.2](04-information-stores-and-lifecycle.md#22-loading-load_bootstrap), failure output included; it keeps no state, so it needs no change for the stateless server.

### 12.1 `move <note> <new_path> [--relink all|none|ask]`

The one command that asks the user something. It runs both steps of §6.1: `move_note`, then `relink` on the notes the user picks.

```
$ recognitor move ideas/Caching design/b/Caching
moved: ideas/Caching -> design/b/Caching
broken links: 3 links in 2 notes
- design/b/Caching: links_to ./Setup
- ops/Runbook, titled "Runbook v2": links_to ideas::Caching
- ops/Runbook, titled "Runbook v2": relates_to ../ideas/Caching
recognitor relink "ideas/Caching" "design/b/Caching" "ops/Runbook"
edit by hand: 1 link in design/b/Caching (relink does not touch the moved note's own links)
1 note has a link to the moved note:
  1. ops/Runbook
Rewrite the links in which? [all / none / numbers, e.g. 1,3] (Enter = none): all
relinked: 2 links in 1 note
```

- **No checksum:** the checksum guards a version the caller read (§5.2), and a person at a prompt read nothing. The CLI reads the note itself just before the move and passes that checksum, so a save in an editor between the two still refuses the move. A name that matches several notes lists their ids and moves nothing (§6.3).
- **Two lock periods:** `move_note` commits and releases the write lock (§5.1) before the prompt; `relink` takes its own lock after the answer. The lock is never held while a person thinks. A move whose prompt is abandoned stays done.
- **The `relink` line** of §11 becomes the CLI command that does the same: ids in double quotes, and `--vault "<path>"` at the end only when `--vault` was given. It is printed before the prompt, so Ctrl-C still leaves the way to finish on screen.
- **`edit by hand:`** follows when the moved note's own links broke, because the CLI has no `edit` command (§12). It shows even when no linking note is listed.
- **Lint block** (§5.4): after `edit by hand:`, which stays next to the `relink` command it explains, and before the bootstrap notice.
- **Streams:** the move block and the `relink` block (§6.1) go to stdout; the move block is flushed before the prompt. The prompt, its list and its messages go to stderr, so stdout holds only Markdown text.
- **The prompt** shows only when the `relink` line names at least one note.

| Answer | Notes relinked |
| :--- | :--- |
| `all`, `a`, `yes`, `y`, in any case | Every listed note |
| `none`, `n`, `no`, Enter alone, or the end of input | None. stderr says `No links were rewritten. The relink command above does it later.` |
| Numbers: `1,3`, `1 3`, `2-4` | Those notes |
| Anything else | None yet: the reason, then the question again, with no limit. `"maybe" is not an answer. Enter all, none, or numbers from 1 to 3, like 1,3.` or `4 is not in the list, which has 3 notes. Enter all, none, or numbers from 1 to 3.` |

- **Enter means none:** no file is rewritten without an explicit answer.
- **Numbers that leave notes out:** after the `relink` block, the `recognitor relink` command follows again on stdout, naming only the notes left out, so the last command on screen is the one that finishes the job.
- **`--relink`:** a CLI-only option. `all` and `none` answer without asking, and print nothing to stderr. `ask` reads the answer from stdin even when it is a pipe: `echo 1,3 | recognitor move ... --relink ask`. The default is `ask` when stdin is a terminal and `none` when it is not; then the move is kept and stderr says `stdin is not a terminal, so no links were rewritten. Run the relink command above, or pass --relink all.`
- **Exit codes:** as for every command (§2.1, "Errors"). A refusal (§5.3) is an error: stdout stays empty and nothing moves. The same path, several matches and relink warnings exit 0.
- **Usage messages:** `move needs a note and a new_path, and takes nothing else`, `--relink needs all, none or ask`, `--relink belongs to move`.

### 12.2 `lint [--strict]`

Checks the vault against the rules of [02-open-questions-and-tradeoffs.md §8](02-open-questions-and-tradeoffs.md#8-schema-validation-strategy-structural-linting--vault-wide-type-schemas-resolved) and prints one line per finding, then a summary line. It writes no note file.

```
$ recognitor lint
warning: design/Index: entry point is spelled "Index.md"; spell it index.md
warning: ops/Deploy:23: link "Caching" reaches design/Caching in another namespace without a prefix; write it as [[design::Caching]]
warning: ops/Runbook:14: summary paragraph is 333 characters; the limit is 300
lint: 0 errors, 3 warnings, 0 hints; 106 notes checked
```

- **Line shape:** `<severity>: <subject>[:<line>]: <message>`. The severity is `error`, `warning` or `hint`. The subject is a note id or `.recognitor/schemas.toml`. A finding about the vault as a whole (names, a namespace without `index.md`, the bootstrap hint) has no subject: `warning: namespace "ops" has no index.md; load_namespace lists its notes instead`.
- **Order:** errors, then warnings, then hints. Within each, by subject ignoring case, then by line; a finding with no line comes before the subject's findings that have one, and findings on one line keep the order the checks found them.
- **Summary line:** always last, `lint --strict:` when the flag is given. A clean vault prints only this line.
- **Severity:** errors are check 7 (a non-ASCII name, case twins) and a `schemas.toml` that cannot be used. Everything else is a warning, a file the sweep could not read included: a lint that could not read a file has not passed. `--strict` prints every warning as `error:`. The bootstrap hint stays a hint.
- **Sweep first:** lint runs a sync, then checks the stored copy of every note with the sweep's own parser and link resolver. A sync with no changes reads no files and the index keeps no warnings, so this is how lint sees every note's warnings. Type schemas read the note's text, not its stored rows, because bootstrap and archive notes store no observations or edges.
- **Streams:** the report goes to stdout, even with errors, and stderr stays empty (§2.1, "Errors").
- **Write tools** show the one-note findings a write introduced, with the same messages (§5.4).

| Exit | When |
| :--- | :--- |
| 0 | No error finding |
| 1 | At least one error finding (with `--strict`, any warning), or the command failed (§2.1) |
| 2 | Bad arguments (§2.1) |

**Messages.** The parser's warnings (checks 1, 2, 3, 5, and an observation with no text) and the ambiguous link (check 4) keep the text `sync --report` prints. The others:

| Check | Message |
| :--- | :--- |
| 4, a link to another namespace without `ns::` ([03 §4](03-markdown-format-and-edge-syntax.md#writing-a-link)) | `link "<link>" reaches <id> in another namespace without a prefix; write it as [[<ns>::<link>]]` |
| 6, no confirming edge | `status is "superseded", but no note links to it with supersedes` |
| 6, the target disagrees | `supersedes [[<link>]], but <id> has status "<status>", not "superseded"` |
| 7, names (as `sync` prints them, §6) | `non-ASCII name, not indexed: <paths> (<n> notes left out)`, `case twins, none of them indexed: <paths> (<n> notes left out)` |
| 8, entry point | `entry point is spelled "<name>"; spell it index.md` |
| 8, Markdown link | `link "<link>" differs in case from the file <path>; GitHub and Linux tools need the exact case` |
| 9 | `namespace "<folder>" has no index.md; load_namespace lists its notes instead`; for the bootstrap, `namespace "<folder>" has no index.md, so load_bootstrap returns an error` |
| Hint | `the vault has no bootstrap/ folder with notes, so load_bootstrap returns an error` |
| Schema, `required_fields` | `schema "<type>": required field "<field>" is missing`, or `... is empty` |
| Schema, `allowed` | `schema "<type>": <field> is "<value>"; allowed: <a>, <b>`; `is empty` or `is not a plain value` in place of the quoted value |
| Schema, `required_categories` | `schema "<type>": no [<category>] observation` |
| Schema, `required_relations` | `schema "<type>": no <predicate> relation under ## Relations` |
| `schemas.toml` | `cannot be read: <reason>`; `is not valid TOML, so no schema is checked: line <n>: <reason>`; `unknown key "<key>"; the file holds only [type.<name>] blocks`; `"type" must be a table of [type.<name>] blocks`; `[type.<name>] must be a table`; `[type.<name>] unknown key "<key>"; the keys are required_fields, allowed, required_categories, required_relations`; `[type.<name>] <key> must be a list of strings`, where an entry inside `allowed` is named `allowed.<field>`; `[type.<name>] allowed must be a table of lists, like { status = ["draft"] }` |

- **Usage messages:** `--strict belongs to lint`.

### 12.3 `ghosts [<link>]`

Lists the ghosts (links that reach no note, [03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)), most links first, with the "maybe moved to" hint of §6.2. Read-only. The query is [05-database-schema-specification.md §3.5](05-database-schema-specification.md#35-ghost-notes-discovery-unwritten-concept-backlog).

```
$ recognitor ghosts
ideas::Caching   3 links   → maybe moved to design/Caching
Gall's law       2 links
```

- **Name:** the link as written; links that differ only in case are one ghost, shown in one spelling. A relative or Markdown link shows its key, the path from the vault root with a leading `/` (`/ideas/caching`), which also works as `<link>`.
- **Count:** `1 link`, `<n> links`.
- **Order:** most links first, then by key.
- **Ambiguous:** a link that matches several notes is a ghost too (doc 03 §4, step 5). It is marked `(ambiguous)` and gets no hint, because a single filename match would point at the wrong note.
- **Stores:** links from bootstrap notes count; archive notes have no edges, so they add none.
- **No ghosts:** `No ghosts.`
- **Sync:** like `search`, it syncs first unless the last sync is under 2 seconds old.
- **No cap:** every ghost is listed, as every broken link is in §10.

**`ghosts <link>`** shows one ghost in full: its summary line, then one line per link to it, in §10's order, then the `relink` command when there is a hint.

```
$ recognitor ghosts ideas::Caching
ideas::Caching   3 links   → maybe moved to design/Caching
- ops/Runbook: line 12: links_to ideas::Caching
- ops/Runbook: line 30: relates_to ideas::Caching
- ops/Deploy: line 4: links_to ideas::Caching
recognitor relink "ideas/Caching" "design/Caching" "ops/Runbook" "ops/Deploy"
```

- **Link lines:** `- <id>: line <n>: <predicate> <link as written>`; the id carries `, titled "<title>"` when the title differs (§4.2), and so does the hint's target.
- **Matching `<link>`:** its key, ignoring case, or any spelling as written, so a link copied from a note finds its ghost. Several matching ghosts print as blocks separated by a blank line. None: `No ghost matches "<link>".`, exit 0.
- **An ambiguous ghost** adds `matches <n> notes: <id>, <id>` and prints no command.
- **The `relink` command** is quoted like `move`'s (§12.1), with `--vault "<path>"` only when `--vault` was given. Its `old` is the id the link pointed to:

| Link | `old` |
| :--- | :--- |
| `/ns/folder/name` (a relative or Markdown link) | `ns/folder/name` |
| `ns::folder/name`, `ns::name` | `ns/folder/name`, `ns/name` |
| `folder/name`, no prefix | `<linking note's namespace>/folder/name`; one command per distinct `old` |
| A bare name | No command: it never has a hint (§6.2) |

- **Spelling of `old`:** as the link writes it (§6, "Id = path"), so `[[old/Ghost target]]` in `tmp` gives `tmp/old/Ghost target`. Links that differ only in case use the spelling the summary line shows. A relative or Markdown link has only its key, so its `old` is the key without the leading `/`.

- **Usage messages:** `unexpected argument: <word>` for a second positional.

### 12.4 `trash empty`

Removes `.recognitor/trash` (§10). The directory goes with its contents, notes and other files alike. The rest of the vault stays, including other files under `.recognitor/`. The folder is hidden and never indexed, so the command does not open the database and does not ask anything.

Success prints nothing. Stdout and stderr stay empty, and the exit code is 0. A missing trash directory is already clear, and so is an empty one; an empty directory is removed too.

A delete the operating system refuses stops the command. Files already removed stay removed. The file that could not be removed is left as it was: same bytes, same name. Stdout stays empty and the error goes to stderr (§2.1).

A symlink or junction in place of the trash directory is not followed. Nothing is deleted, and the error is `<path>: refusing to follow a link out of .recognitor/trash`. A trash path that exists and is not a directory gets `<path>: not a directory`. A link inside the trash is removed; its target outside the trash stays.

- **Usage messages:** `usage: trash empty` when `trash` has no subcommand or one other than `empty`. A word after `empty` is `unexpected argument: <word>`.

### 12.5 `doctor`

Checks the bootstrap block in each installed harness instruction file ([04-information-stores-and-lifecycle.md §5.3](04-information-stores-and-lifecycle.md#53-the-bootstrap-block)). Read-only: it creates nothing and edits nothing. It takes no harness name and no flag of its own. `--vault` applies, as it does for `bootstrap`.

```
$ recognitor doctor
claude +
antigravity -
codex ↑
opencode ?
pi ×
---
recognitor bootstrap write antigravity, codex
```

- **Line:** `<name> <mark>`, or `<name> <file> <mark>` when that harness lists more than one file. The path stays off the report. The marks are the states of §5.3 of doc 04:

| Mark | State |
| :--- | :--- |
| `+` | up to date |
| `-` | block absent |
| `↑` | out of date |
| `?` | misconfigured |
| `×` | missing |

- **Suggestion:** after the list, when any file is `-` or `↑`, a `---` line and one command: `recognitor bootstrap write <name>[, <name>...]`. Names are those harnesses, once each, in the order they first appear. One name has no comma. No such file means no `---` and no command. `?` and `×` are never listed: a one-marker file and a missing file stay as they are until someone fixes them by hand. `bootstrap write` and `bootstrap remove` are not commands in this version. The line names the fix.
- **Order:** the four built-ins, then names the file adds, in file order. Files of one harness follow its `files` list.
- **Not installed:** a harness whose directory is absent is skipped. Nothing is printed for it, and the directory is not created. When every harness is skipped, stdout and stderr stay empty and the exit code is 0.
- **Vault:** after the table parses and every present harness directory is a directory, `doctor` compiles the payload the same way `recognitor bootstrap` does, including when every harness was skipped. No vault is a usage error (§2.1). A missing `bootstrap/index.md` is the same error as `bootstrap`.
- **Streams:** the report goes to stdout, and stderr stays empty. A bad table, a path that exists and is not a directory, or an instruction path that exists and is not a file goes to stderr as `error:`, with nothing on stdout and no partial report.
- **Order of checks:** the table is read first, then the directories. Either failure exits before the vault is opened, so a bad table does not sweep.

| Exit | When |
| :--- | :--- |
| 0 | Every printed file is `up to date`, or every harness was skipped |
| 1 | Any file is not `up to date`, or the table or a path shape is wrong, or the command failed (§2.1) |
| 2 | Bad arguments, including no vault (§2.1) |

**Harness tables.** A missing config file is an empty document. The four built-ins of doc 04 §5.3 exist anyway. The file is not rewritten to materialize them.

```toml
[harness.claude]
directory = "~/.claude"
files = ["CLAUDE.md"]

[harness.my-tool]
directory = "D:/apps/my-tool"
```

- **Overlay:** the same name, compared case-insensitively, replaces only the fields that are present. `[harness.claude]` with only `directory` keeps `CLAUDE.md`. The report uses the canonical spelling: `Claude` selects `claude`.
- **A new name** adds a harness, after the four, in file order. Its `directory` is required. The canonical name is the key folded to ASCII lowercase: a letter, then letters, digits, and hyphens.
- **Two keys that differ only by case** are an error.
- **`files`:** optional. Omitted or `[]` means one file, `AGENTS.md`. A non-empty array is the whole list. Any field other than `directory` and `files` is an error, so `file` cannot fall through to `AGENTS.md`.
- **`directory`:** a home-relative path or an absolute path. `~/.claude` and `~\.claude` are the same path. `~` is `directories::BaseDirs::home_dir()` (`%USERPROFILE%` on Windows, `$HOME` on Unix). `~` alone, `~` plus a username, an empty directory, and a path with no `~` and no drive or root (`.claude`, `bin\my_harness`) are errors. Recognitor does not guess from the current folder. An absolute path is used as given. A `~` path with no user profile is an error. An absolute path does not need one. `CODEX_HOME`, `XDG_CONFIG_HOME`, and `OPENCODE_CONFIG_DIR` are not read.
- **Each `files` entry** is a relative path inside the harness directory. Absolute, empty, or a `..` component is an error. A repeated path in one harness is an error: `/` and `\` are the same path, a `.` component is ignored, and case counts as written.
- **On disk:** an installed harness has its directory. A named file that is missing is `missing` in the report, and the other files are still listed. A path that exists and is not a directory, or an instruction path that exists and is not a file, is an error and nothing else is printed.

A harness error is `error: <config path>: <message>`. The rows that start with `<path>` or `<home>` are the whole text after `error: `.

| Case | Message |
| :--- | :--- |
| `[harness]` is not a table | `[harness] must be a table of harnesses` |
| Two keys differ only by case | `harness names "<a>" and "<b>" differ only by case` |
| An entry is not a table | `[harness.<name>] must be a table` |
| Illegal name | `harness name "<name>" is illegal: use ASCII lowercase letters, digits and hyphens, starting with a letter` |
| New harness, no directory | `[harness.<name>] needs a directory` |
| `directory` is not a string | `[harness.<name>].directory must be a string` |
| `files` is not an array of strings | `[harness.<name>].files must be an array of strings` |
| One `files` entry is not a string | `[harness.<name>].files[<n>] must be a string` |
| Unknown field | `[harness.<name>] has unknown field "<field>"; only directory and files are valid` |
| Empty directory | `[harness.<name>].directory is empty` |
| `~` alone | `[harness.<name>].directory is "~"; use ~/... or an absolute path` |
| `~` plus a username, or `~` not followed by `/` or `\` | `[harness.<name>].directory "<value>" is not a home-relative path; use ~/... or an absolute path` |
| Relative directory | `[harness.<name>].directory "<value>" is relative; use ~/... or an absolute path` |
| Empty file path | `[harness.<name>].files has an empty path` |
| Absolute file path | `[harness.<name>].files entry "<path>" is absolute; use a path inside the harness directory` |
| A `..` component | `[harness.<name>].files entry "<path>" has a .. component` |
| Repeated file path | `[harness.<name>].files repeats "<path>"` |
| No user profile | `<home>: no user profile directory found; set harness directories to absolute paths` |
| Path exists and is not a directory | `<path>: <path> exists and is not a directory` |
| Instruction path is a directory | `<path>: <path> is a directory; expected an instruction file` |
| Instruction path is not a file | `<path>: <path> is not a file` |

- **Usage messages:** `unexpected argument: <word>`.

---

## 13. Future Tools

Ideas recorded for later. None is part of the first version.

- **`recall(query, max_tokens)`:** answers "give me the best material for this question" instead of "show me this note". It walks links from the best matches and packs the most relevant sections of several notes into one token budget, marking which note each piece came from. Kept apart from `read_note` so that a read stays predictable. Builds on the section scoring of §7.2. Whether it is worth building is decided by the benchmark's "tokens spent to reach the answer" measure ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)).

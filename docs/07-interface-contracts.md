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
- **Bootstrap notice:** the tool result states that the note loads as instructions at session start ([04-information-stores-and-lifecycle.md §2.3](04-information-stores-and-lifecycle.md#23-writes)).
- **Archive is append only:** `write_note` without a checksum creates an archive note, and `edit_note` with `append` adds to one. Every other write to an archive note is refused. `relink` never reaches one, because archive notes have no edges ([04-information-stores-and-lifecycle.md §4](04-information-stores-and-lifecycle.md#4-archive-store)).
- **A move checks both stores:** the one the note leaves and the one it enters. A move out of the archive is refused, because it changes an archive note. A move into the archive counts as a create, so a knowledge note can be archived by moving it. A move into or out of the bootstrap is allowed and carries the bootstrap notice.
- **Explicit request:** doc 04 shows archive notes "only on explicit request". Passing `ns: "archive"` to `search` is one; reading a note that `context` marked `(archive)` is another, because a curated note linked to it.

---

## 2. Inventory

| Surface | Tools / commands |
| :--- | :--- |
| **MCP** | `load_bootstrap`, `load_namespace`, `search`, `context`, `read_note`, `write_note`, `edit_note`, `move_note`, `relink`, `delete_note` |
| **CLI** | `bootstrap [--list]`, `search`, `context`, `read`, `move`, `relink`, `ghosts`, `trash empty`, `sync`, `rebuild`, `lint [--strict]`, `config get/set` |

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

- **Word for word:** the server sends this text exactly as written here. The retrieval benchmark ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)) can change it, through this section.
- **Write tools** get their lines here when they are built, under the same rules.

**Errors:** this document calls each outcome either an error or a normal result. On the wire:

| Outcome | MCP | CLI |
| :--- | :--- | :--- |
| Error: `"x" not found`, `load_namespace("bootstrap")`, a query with no words, no such section | A tool result with `isError: true`; its text is the message | `error: <message>` on stderr, exit 1 |
| Normal result: no hits, several matches (§6.3), a page past the end, no links | A tool result with the text | The text on stdout, exit 0 |
| Bad arguments: `query` missing, `page` not a number, an argument the tool does not take | As an error | `error: <message>` and the usage text on stderr, exit 2 |
| Unknown tool: a name not in `tools/list` | A JSON-RPC error (`-32602`), as the MCP spec prescribes. Clients offer only listed tools, so agents do not reach it | No CLI twin: an unknown command is a bad argument |

- **Why a tool result:** each error message names the fix (call `load_bootstrap`, the list of sections), so it comes back as text the model reads and acts on. The flag tells the client the call failed, so a refusal never passes for a success.
- **No prefix over MCP:** the text is the CLI's message without the `error:` prefix; the flag already says it.
- **Argument messages:** they match the CLI's texts, without its dashes. A missing argument reads `<tool> needs <what>`: `search needs a query`, `read_note needs a note`, `load_namespace needs ns`. An argument of the wrong type reads `<argument> needs text`, or `page needs a number from 1`. New tools follow the same two patterns.
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
| Create on an existing note, overwrite, `move_note`, `delete_note` | The whole note as it is now, with its checksum, cut at the read limit |
| Create or move onto case twins | The path of every twin, without content |

The rejection never repeats the agent's own request: the agent already has it in context.

---

## 6. Note Identity and Moves

Recognitor follows the Obsidian model: a note's address is its path. It replaced the earlier permalink identity on 2026-11-15; resolution rules are in [03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution).

- **Id = path:** a note's id is its path from the vault root without `.md` (`common/working-style/ADHD profile`). Output always shows it as written. The database stores its lowercase **key** instead ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)), and no output shows the key. Both come from the path, never from frontmatter, with no slug conversion, so nothing can drift, and Obsidian path links match without conversion.
- **Kept as written, matched in lowercase:** links, tool arguments and namespaces match a lowercase key of the path ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)).
- **ASCII names only:** every note and folder name is ASCII, so a key means the same on every platform. Other files (images, PDFs) are never indexed, so their names are not checked. A non-ASCII name is handled like a case twin in the table below.
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
2. The agent shows the list to the user, who picks which links to update.
3. `relink(old, new, notes)` rewrites the chosen links. Each rewrite is a guarded edit of the linking note (§5).

- **CLI:** `recognitor move` asks the user directly and does both steps in one command.
- **Broken means changed by the move, whatever the link looks like:** under the write lock, `move_note` resolves the affected links before and after the move: the links whose key matches one of the note's names, and the note's own links. It reports every link whose target changed or became a ghost:

| Link | Breaks because |
| :--- | :--- |
| `[[ideas/caching]]` | The path changed |
| `[[ideas::Caching]]` | The namespace changed |
| `[[Caching]]` | The name now matches two notes |
| `[[./Setup]]` inside the moved note | It now starts from the new folder |

- **Links inside the moved note** are fixed with `edit_note` (`find_replace`), because `relink` rewrites only links that point to the moved note.

**`relink` never rejects.** Each linking note is handled on its own. Rewriting a link replaces only the link text, so it cannot overwrite anyone's work and needs no checksum:

| Case | Result |
| :--- | :--- |
| The old link is still there | Rewritten |
| The old link is gone (someone already edited it) | Skipped, and reported as a warning |

```
relinked: 4
warning: skipped, link already gone: recognitor/Sync design
```
- **Links by name usually survive:** `[[Caching]]` keeps resolving after a move unless the name now matches two notes, so the list is usually short.
- **Unchosen links** become ghosts and show up in `recognitor ghosts`.

### 6.2 Moves Outside Recognitor

A move in Obsidian, Explorer or git looks to the sweep like one file deleted and one file created. The sweep does not report it: the sweep usually runs inside another tool call, whose output has no place for the report. The links the move broke become ghosts instead, and `recognitor ghosts` shows where the note probably went:

```
ideas/caching   3 links   → maybe moved to design/Caching
Gall's law      2 links
```

- **The hint** is worked out when the command runs, so nothing is stored. It shows only when exactly one note has the ghost's filename (`notes.name_key`); for a shared name such as `index`, it shows nothing.
- **Fix:** `recognitor relink` on the CLI, or `relink` through an agent (§6.1).

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
- **No whole-note checksum on a cut read:** the header line carries no checksum, and the closing line lists each shown section's checksum instead. An agent can never overwrite the whole note from a partial copy and silently delete the sections it did not see.

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

---

## 10. `delete_note(note, expected_checksum)`

- **Vault trash, not a hard delete:** the file moves to `.recognitor/trash/<timestamp>/<original path>`. A move inside the vault works the same on every platform and drive, and it cannot half-fail. The timestamp folder keeps two deletes of the same path apart and records when each happened.
- **Never indexed:** `.recognitor/` is a hidden folder ([03-markdown-format-and-edge-syntax.md §4](03-markdown-format-and-edge-syntax.md#4-note-identity-and-link-resolution)).
- **Undo:** move the file back. `recognitor trash empty` clears the folder.
- **Output:** the links that now point nowhere. They become ghosts, and the user decides whether to clean them up.
- **Guard:** the whole-note checksum (§5.2).
- **Stores:** §1.1.
- **Rejected: the OS trash.** Windows skips the Recycle Bin on network drives, macOS asks permission to control Finder, and the Linux trash can fail on servers and other mounts.

---

## 11. `write_note(path, content, expected_checksum?)` and `move_note(note, new_path, expected_checksum)`

Both are built from the rules above; no new decisions.

| | `write_note` | `move_note` |
| :--- | :--- | :--- |
| **Input** | `path` from the vault root (`.md` optional), `content` (the whole file, frontmatter included), optional `expected_checksum` | `note`, `new_path`, `expected_checksum` |
| **Without checksum** | Create only; rejected if the note exists (§5.2) | Not allowed |
| **Refused when** | The key already exists (§6), the path has no top-level folder, or the path has a non-ASCII character (§6) | The target's key already exists, unless it is the note's own key: a case-only rename is allowed and breaks no links (§6). Also when `new_path` has no top-level folder or a non-ASCII character |
| **Output** | `<id>  checksum: <12 hex>` | The new id, and every link the move broke, with the `relink` call for links into the note; links inside it are fixed with `edit_note` (§6.1) |
| **Folders** | Created as needed; existing folders keep their spelling (§6) | Created as needed; existing folders keep their spelling (§6) |

- **Stores:** the bootstrap notice and the archive limits follow §1.1.

---

## 12. CLI

The CLI has no `write`, `edit` or `delete` commands. A human edits notes in an editor or Obsidian and the next sweep picks the changes up; agents write through MCP. The CLI offers only what a human cannot do by hand. Its output is the same Markdown text as the MCP tools.

| Command | Why it exists |
| :--- | :--- |
| `bootstrap [--list]`, `search`, `context`, `read` | Inspect what agents see; the session-start hook runs `bootstrap` ([04-information-stores-and-lifecycle.md §5.1](04-information-stores-and-lifecycle.md)) |
| `move`, `relink` | A move needs the link check and `relink` (§6.1); a move made outside Recognitor needs `relink` alone (§6.2) |
| `ghosts`, `lint [--strict]`, `trash empty` | Maintenance |
| `sync`, `rebuild`, `config get/set` | Engine control |

- **Same arguments as MCP:** a CLI command takes its MCP tool's arguments by the same names. The first required argument is positional, and every optional one is an option with its name: `search <query> --ns <name> --page <n>`, `context <note> --page <n>`, `read <note> --section <heading> --query <text>`.
- **Which vault:** the `--vault <path>` flag, else the `RECOGNITOR_VAULT` environment variable, else `vault.path` in the config file. With none of them set, the command fails with an error that names all three. Sessions, hooks and the MCP server start in code repos, not in the vault, so the vault is never guessed from the current folder. The MCP server finds its vault the same way.
- **Config file:** `config.toml` in the per-user local config folder, `ProjectDirs::config_local_dir()` from the `directories` crate (Windows: `%LOCALAPPDATA%\recognitor\config\config.toml`). Local, not roaming, because a vault path belongs to one machine. The `RECOGNITOR_CONFIG` environment variable points at another config file, for tests and portable setups. `config get/set` reads and writes it and keeps comments and unknown keys; keys follow the `section.name` style ([05-database-schema-specification.md §5](05-database-schema-specification.md#5-database-file-location)).

`load_bootstrap` behaves as specified in [04-information-stores-and-lifecycle.md §2.2](04-information-stores-and-lifecycle.md#22-loading-load_bootstrap), failure output included; it keeps no state, so it needs no change for the stateless server.

---

## 13. Future Tools

Ideas recorded for later. None is part of the first version.

- **`recall(query, max_tokens)`:** answers "give me the best material for this question" instead of "show me this note". It walks links from the best matches and packs the most relevant sections of several notes into one token budget, marking which note each piece came from. Kept apart from `read_note` so that a read stays predictable. Builds on the section scoring of §7.2. Whether it is worth building is decided by the benchmark's "tokens spent to reach the answer" measure ([02-open-questions-and-tradeoffs.md §11](02-open-questions-and-tradeoffs.md#11-target-scale--benchmark-resolved)).

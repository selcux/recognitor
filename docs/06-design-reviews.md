# Recognitor Design Reviews

This document is the running log of design and architecture reviews for Recognitor. Every review is recorded here.

---

## How This Log Works

1. **One section per review, newest first.** Each review heading carries its id (`DR<n>`), date, and scope.
2. **Reviews are frozen.** A review's text is never rewritten after it lands. Later corrections go into a new review.
3. **Findings are tracked by id.** Each review ends with a tracker table (`DR<n>-<nn>`). Later reviews and commits update the **Status** column only.
4. **Status values:** `Open`, `Resolved` (with commit or doc reference), `Rejected` (with reason), `Superseded` (with the finding that replaces it).

### Review Index

| Id | Date | Scope | Baseline commit | Score | Findings |
| :--- | :--- | :--- | :--- | :--- | :--- |
| [DR3](#dr3-2026-11-16-closing-review-docs-01-07) | 2026-11-16 | Closing review, docs 01-07 | `2a4d895` | 8/10 | 6 resolved |
| [DR2](#dr2-2026-11-16-alignment-review-docs-01-07) | 2026-11-16 | Alignment review, docs 01-07 | `8512d2b` | 7/10 | 13 resolved |
| [DR1](#dr1-2026-11-14-full-design-review-docs-01-05) | 2026-11-14 | Full design review, docs 01-05 | `9db3af1` | 6/10 | 16 resolved, closed 2026-11-15 |

---

## DR3: 2026-11-16, closing review (docs 01-07)

**Score: 8/10.** Every DR2 fix landed where its rule lives. In four places a second copy still carries the old form, and two of those copies contradict the fix.

No finding is Critical or High. Every finding is a second place that disagrees with the main rule. Scope, set by the user: only missing or incomplete DR2 fixes, contradictions, and rules that cannot be built as written. Each fix is the smallest edit to the existing text.

---

### What's solid (keep it)

| Decision | Why it holds up |
|---|---|
| Store access table (doc 07 §1.1) | It is the one home for store rules, and doc 02 §5, doc 04 §1-4 and doc 05 §1, §3.6, §3.7 link to it. The full-text query handles a search with no `ns` correctly: `?2` is NULL, so `FALSE OR NULL` drops archive rows. |
| `ref_key` and `name_key` | Ghost grouping and targeted re-resolution each have an index: `idx_edges_ref`, `idx_notes_name`, and `idx_notes_title` on `title_key` alone. |
| Tool arguments resolve by steps 1, 2, 4 (doc 07 §6.3) | The "several matches" path is the common case, and it is specified. In the real vault, `index` exists in `common` (`index.md`), `compass` (`Index.md`) and `recognitor` (`index.md`). |
| No custom bootstrap directory | `bootstrap-dir`, `bootstrap_root` and "custom bootstrap" appear only in the two sentences that reject them (doc 02 §10, doc 04 §2.1). |
| ASCII-only names | The same rule with the same wording in doc 02 §8, doc 03 §4, doc 05 §2.1 and doc 07 §6, §11. |

**DR2 re-check:**

| DR2 | Now | Gap |
|---|---|---|
| 01 id and key | Holds in docs 05 and 07, leftover elsewhere | doc 03 §5 SQL (DR3-04); doc 07 §6.3 example (DR3-06) |
| 02 store rules | Holds, leftover elsewhere | "whole vault" in doc 01 §3 and doc 07 §1 (DR3-06) |
| 03 create guard | Partial | doc 07 §5.1 step 2 (DR3-01); doc 02 §4 (DR3-06) |
| 04 bootstrap notice | Holds | |
| 05 link keys | Holds, leftover elsewhere | doc 03 §3 vs §4 (DR3-05) |
| 06 one upsert | Holds | |
| 07 root files | Holds, leftover elsewhere | doc 04 §1 rule box (DR3-06) |
| 08 bootstrap failure | Partial | doc 05 §3.1 (DR3-02); lint list (DR3-03) |
| 09 move reports | Holds, leftover elsewhere | doc 07 §6.1 and §11 (DR3-06) |
| 10 tool arguments | Holds, leftover elsewhere | `relink` `old` (DR3-06) |
| 11 fixed bootstrap | Holds | |
| 12 ASCII names | Holds, leftover elsewhere | doc 05 §2.1 "the sweep reports" (DR3-06) |
| 13 stale statements | Holds | |

That is 4 that hold, 7 that hold with a leftover elsewhere, and 2 that are partial. Under log rule 3 the DR2 statuses stay `Resolved`, and the DR3 tracker carries the gaps.

**DR1 re-check:** I spot-checked against the current text and found no regression. Two DR1 tracker cells are stale (DR3-06).

---

### Medium priority

#### 1. The create guard, as written, refuses every existing folder and every case-only rename (DR2-03, partial)

| Doc 07 | Says |
|---|---|
| §5.1 step 2 | "A create or a move instead reads each folder on the target path from disk and compares every name in it by key (§6). Any match refuses, even a file the index does not know yet, and even case twins." |
| §6, table row 1 | "A new path reuses the spelling of folders that already exist (`Baseline/x.md` lands in `baseline/`)" |
| §11, `move_note` | Refused when the target's key exists, "unless it is the note's own key: a case-only rename is allowed" |

Taken literally, `write_note("baseline/New.md")` in the test vault reads the vault root, finds `baseline` and refuses. A case-only rename finds the note itself and refuses. The code is built from step 2, so the coder has to guess which rule wins.

Fix, doc 07 §5.1 step 2, replace the last sentence:

```
Any match on the last name refuses, even a file the index does not know yet, unless it is the
note's own key (a case-only rename, §11). A folder that matches is reused with its spelling;
twin folders refuse (§6).
```

#### 2. The bootstrap walk query drops the ghosts the payload must list (DR2-08, partial)

| Doc | Says |
|---|---|
| doc 04 §2.2 | "Ghost links in the tree: the payload ends with one line listing them" |
| doc 05 §3.1 | `JOIN notes n ...` with the comment "ghosts drop out of the JOIN"; it selects `e.target_key` only |

If the walk is built from doc 05, it never sees a ghost, so the "not loaded" line is always empty. Fix, doc 05 §3.1 (same filters, ghosts kept):

```sql
SELECT e.target_key, e.target_ref                          -- target_key NULL = ghost, for the "not loaded" line
FROM edges e
LEFT JOIN notes n ON n.key = e.target_key
WHERE e.source_key = ?1
  AND e.is_inline = 0                                      -- ## Relations links only
  AND e.predicate != 'supersedes'                          -- never load a retired version
  AND (n.namespace = 'bootstrap' OR e.target_key IS NULL)  -- never leave the bootstrap
ORDER BY e.position;
```

#### 3. No lint check covers a missing `bootstrap/` folder (DR2-08, partial)

Doc 04 §2.2 says the error line ends with "Run `recognitor lint`" and that "`lint` names the cause". Doc 02 §8 lists every lint check:

- Check 7 covers twins.
- Check 9 covers a namespace without `index.md`.
- Nothing covers a vault with no `bootstrap/` folder. In that case there is no `bootstrap` namespace for check 9 to look at.

Evidence: the test vault (`C:\Users\selcuk\agent-pkm`) has `baseline/` and `compass/` and no `bootstrap/`. Today `recognitor bootstrap` would print the error and exit non-zero, then send the user to a command whose nine checks say nothing about it.

Fix, doc 02 §8 check 9: "Every namespace has an `index.md`, and the vault has `bootstrap/index.md`; a missing one gets a warning."

#### 4. Doc 03 §5 keeps an old copy of the `context` queries (DR2-01 leftover)

Doc 03 §5, item 3:

```sql
SELECT predicate, target_ref, target_key FROM edges WHERE source_key = ?;
SELECT predicate, source_key FROM edges WHERE target_key = ?;
```

| Point | doc 03 §5 | doc 05 §3.3, §3.4 and doc 07 §8 |
|---|---|---|
| Output | Keys (`target_key`, `source_key`) | `n.path`. Doc 07 §6: "no output shows the key" |
| Order | None | `e.position`; `n.mtime DESC` |
| Bootstrap | Not filtered | "Never listed" (doc 07 §1.1) |

DR2-06 fixed this same pattern for the `INSERT`: the same SQL in two docs, one of them stale. Fix: replace the code block with "Queries: [doc 05 §3.3](05-database-schema-specification.md#33-two-stage-graph-discovery) (links out) and [§3.4](05-database-schema-specification.md#34-backlink-discovery-incoming-references) (links in)."

#### 5. Doc 03 gives two rules for a Markdown link's target (DR2-05 leftover)

| Rule | doc 03 §3, Markdown links | doc 03 §4, link key |
|---|---|---|
| Encoding | "Percent-encoding is decoded" | "with `%20` decoded" |
| `.md` | "Exact files only: no title lookup and no added `.md`" | The key is "without its ... `.md`", and "resolution reads only the key" |

The two rules give different results:

- `[x](Note%20%28draft%29.md)`: under §3 it decodes to `Note (draft)` and matches. Under §4, `%28` and `%29` stay encoded and the link stays a ghost.
- `[x](Setup)` written in `common/notes/`: under §3 it does not match `Setup.md`. Under §4 its key is `common/notes/setup`, the same key as `[x](Setup.md)`, so it matches.

In the test vault this does not show up yet: it has 0 Markdown links. The resolver still has to pick one rule. Fix, two edits:
- doc 03 §4: change "with `%20` decoded" to "with percent-encoding decoded (§3)".
- doc 03 §3: delete "and no added `.md`".

#### 6. Stale and disagreeing statements

| Where | Problem | Fix |
|---|---|---|
| doc 01 §3, doc 07 §1 | "Searches and graph walks span the whole vault." A few lines later, doc 07 §1.1 keeps the bootstrap out, and the archive out unless asked. Doc 07 §4 already says "every knowledge note". | "span every knowledge namespace; bootstrap and archive follow §1.1" |
| doc 04 §1, Default Store Rule | "Anything outside `bootstrap/` and `archive/` is knowledge." But root files and hidden folders are not notes (doc 03 §4). | "Every other top-level folder is knowledge." The doc 04 §1 table already says this. |
| doc 02 §4, Atomic file writes | "rename over the target" for every write. Doc 07 §5.1 step 4 says creates and moves use a rename that cannot replace. | Append "; creates and moves use a rename that cannot replace ([07 §5.1](07-interface-contracts.md#51-the-write-lock))" |
| doc 05 §2.1, last bullet | "The sweep ... reports them until one is renamed." Doc 07 §6.2 says the sweep runs inside other tool calls and has no place to report. Doc 03 §4 and doc 07 §6 say `sync`, `rebuild` and `lint` report. | "`sync`, `rebuild` and `lint` report them until one is renamed." |
| doc 07 §6.1 step 1 | `move_note(note, new_path)`, but §11 makes `expected_checksum` required | `move_note(note, new_path, expected_checksum)` |
| doc 07 §11, `move_note` output | "every link the move broke, with the `relink` call that fixes them". §6.1 says links inside the moved note are fixed with `edit_note`. | "... with the `relink` call for links into the note; links inside it are fixed with `edit_note` (§6.1)" |
| doc 07 §6.3 | `relink` is listed among tools whose note argument must match a note. After a move, `relink(old, ...)` names the old id, which no note has, so it always returns `not found`. | "`relink` (`new` and `notes` only; `old` is the old id, which no note has any more)" |
| doc 07 §6.3, example | Prints `compass/index`. The file is `compass/Index.md` in both the real vault and the test vault, and output shows the id "as written" (doc 07 §6). | `compass/Index` |
| doc 06, DR1 tracker | DR1-05 cites "doc 05 §3.5", which is now the ghosts query; vector search is §3.6. DR1-11 cites "content-hash observation ids", which DR2-13 removed. | Edit the Status cells only (log rule 3): change to "§3.6", and append "(observation ids later removed, DR2-13)" |

---

### Process

DR2's process note applies again, at a smaller scale. Each fix went into the paragraph that owns the rule. A second copy elsewhere kept the old form: doc 02 §4, doc 03 §5, doc 05 §3.1 and the doc 06 tracker cells. Before marking a finding Resolved, search all docs for the rule's old wording and for any SQL that touches the changed columns.

### Suggested next move

Make one commit and start it with DR3-01, DR3-02 and DR3-05. They touch the create guard, the bootstrap walk query and the link key. The working tree already has `src/resolve.rs` and `src/index.rs` (from git status; I did not read them), so code built now runs into these three first. Put the one-line edits from DR3-03, DR3-04 and DR3-06 in the same commit.

---

### DR3 Finding Tracker

| Id | Finding | Severity | Status |
| :--- | :--- | :--- | :--- |
| DR3-01 | Create guard wording (doc 07 §5.1 step 2) refuses existing folders and case-only renames, against §6 and §11 (DR2-03 partial) | Medium | Resolved: only a match on the last name refuses; matching folders are reused, the note's own key is allowed (doc 07 §5.1 step 2) |
| DR3-02 | Bootstrap walk query (doc 05 §3.1) drops the ghosts that doc 04 §2.2 lists (DR2-08 partial) | Medium | Resolved: the walk query keeps ghosts with a LEFT JOIN and returns `target_ref` (doc 05 §3.1) |
| DR3-03 | No lint check covers a missing `bootstrap/`, though doc 04 §2.2 sends the user to `lint` (DR2-08 partial) | Medium | Resolved: `load_bootstrap` reads only the index and errors without an entry point; a vault without a bootstrap is valid; lint gives a hint for a missing `bootstrap/`, and check 7 already covers twins (doc 02 §8, doc 04 §2.2) |
| DR3-04 | Doc 03 §5 keeps an old copy of the `context` queries that returns keys (DR2-01 leftover) | Medium | Resolved: SQL removed from doc 03 §5, which links doc 05 §3.3 and §3.4 |
| DR3-05 | Doc 03 §3 and §4 decode Markdown link targets and strip `.md` differently (DR2-05 leftover) | Medium | Resolved: the link key decodes all percent-encoding, and Markdown links follow the key on `.md` (doc 03 §3, §4) |
| DR3-06 | Stale and disagreeing statements (whole-vault search, store rule box, rename over, sweep reports, `move_note` signature and output, `relink` `old`, example id, DR1 tracker cells) | Medium | Resolved: wording aligned in doc 01 §3, doc 02 §4, doc 04 §1, doc 05 §2.1, doc 07 §1, §6.1, §6.3, §11; DR1-05 and DR1-11 status cells updated |

---

## DR2: 2026-11-16, alignment review (docs 01-07)

**Score: 7/10.** The case rule is sound, and the paragraphs that state it agree with each other. The places next to them do not: the doc 05 queries, the store rules missing from doc 07, and the write path on case-sensitive disks.

No finding is Critical: nothing here loses data on a first run on Windows. Two High findings leak archive content or lose data if the code follows one doc and not the other.

---

### What's solid (keep it)

| Decision | Why it holds up |
|---|---|
| Spelling kept, matching by lowercase key (doc 03 §4) | Matches how Obsidian links. On the real vault, compass's `Index.md` resolves as the entry point and only gets a spelling warning. |
| One or none for case twins (doc 07 §6) | Both rejected options are argued with the failure they cause. The gate works per name, so one bad pair does not stop the rest of the vault. |
| `COLLATE NOCASE` banned for names (doc 05 §2.1) | Correct. SQLite's NOCASE folds only ASCII A to Z (SQLite docs), so `Ö` would never match `ö`. |
| Write lock first, then re-read from disk (doc 07 §5.1) | Right order. A human edit inside the 2-second window is caught, and in WAL mode readers never wait. |
| A guard for each edit op, and rejections carry current data (doc 07 §5.2-5.3) | Two agents editing different parts of a note both succeed, and a retry needs no extra read. |
| No whole-note checksum on a cut read (doc 07 §7.2) | An agent cannot overwrite a note from a partial copy. Small rule, real protection. |

**DR1 re-check:** 12 of 16 findings still hold and DR1-07 stays superseded. 3 regressed in part: DR1-02 (see DR2-01), DR1-08 (DR2-04), DR1-11 (DR2-06).

---

### High priority

#### 1. "id" means two different strings, and the doc 05 queries cannot produce the doc 07 output

| Doc | Says |
|---|---|
| doc 03 §4, doc 07 §6 | "the path from the vault root without `.md`, e.g. `common/working-style/ADHD profile`. Output always shows it as written. There is no stored id and no slug conversion" |
| doc 05 §2.1, `notes.id` | "Key: path without the extension, lowercased (e.g. `common/working-style/adhd profile`)" |
| doc 05 §2.1, `notes.path` | "output shows it without `.md` as the note's id" |

Doc 05 alone uses "id" for both the lowercase key (the column) and the spelled path (from `path`). Docs 03 and 07 say no id is stored, but one is. `edges.source_id`, `edges.target_id` and `observations.note_id` all hold the key.

The queries behind doc 07 output select the key, not the spelling:

| Query | Selects | Doc 07 needs |
|---|---|---|
| doc 05 §3.3 (`context`, links out) | `e.target_id`; no `path`, no `ORDER BY` | spelled id, "outgoing links in written order" (doc 07 §8) |
| doc 05 §3.4 (`context`, links in) | `e.source_id`; no `path`, no `ORDER BY` | spelled id, "incoming links most recently changed first" |
| doc 05 §3.6 (vector search) | `n.id` | spelled id (doc 07 §4.2) |

Built as written, every `context` line prints `common/working-style/adhd profile`. An agent that copies that string into `write_note` or `move_note` spreads lowercase file names through the vault.

Doc 07 §4.2 also says "the title is added only when it differs from the filename", but not whether case counts. In the three projects I can read, 4 of 48 notes differ only in case (`index.md` titled `Index` twice, `hubs/decisions.md` titled `Decisions`, `schemas/concept.md` titled `Concept`).

Fix:
- Add a terms block to doc 03 §4 and link it from docs 05 and 07: **id** = path without `.md`, as written (shown and accepted). **key** = the id lowercased (stored, used to match).
- Rename the columns to say what they hold: `notes.key`, `edges.source_key`, `edges.target_key`, `observations.note_key`. Replace "There is no stored id" with "The key is derived from the path, never from frontmatter."
- Select `n.path` in §3.3, §3.4 and §3.6. Add `ORDER BY e.position` to §3.3 and `ORDER BY n.mtime DESC` to §3.4.
- Compare title and filename by key in doc 07 §4.2.

#### 2. Archive content leaks into normal search, and doc 07 has no archive rules

| Doc | Says |
|---|---|
| doc 04 §4 | "Archive notes are excluded from default searches and graph walks, and are searched only on explicit request." |
| doc 04 §1 table | Archive, main writer: "Agents, append only" |
| doc 02 §5 | "archive notes appear only on explicit request" |
| doc 07 §4 | "`ns` is a perspective, not a filter: every note in the vault can match." |
| doc 05 §2.4 | The `notes_fts` triggers skip only `bootstrap`, so archive notes are in the full-text index |
| doc 05 §3.3, doc 07 §8 | `context` excludes only bootstrap targets |

No doc 07 input can express "explicit request": `search(query, ns?)` has no flag for it. Doc 05 §3 has no full-text query at all; only the vector query applies the archive filter. A `search` built from doc 07 returns raw transcripts in every session. Doc 07 also lets `edit_note`, an overwriting `write_note` and `delete_note` change archive notes, which contradicts "append only".

The bootstrap has the same gap. Doc 04 §3.1 says "`bootstrap` cannot be loaded this way". Doc 07 §3 has no such rule, and the doc 05 §3.2 query (`id = ?1 || '/index'`) returns `bootstrap/index` for `load_namespace("bootstrap")`.

Fix: put one access table in doc 07 §1. The code reads store rules from there and nowhere else:

| Tool | bootstrap | knowledge | archive |
|---|---|---|---|
| `search` | never | yes | only when `ns` is `archive` |
| `context` | never listed | yes | listed, marked `(archive)` |
| `load_namespace` | refused, points to `load_bootstrap` | `index.md` | `index.md` |
| `read_note` | yes | yes | yes |
| write tools | allowed, with the notice (DR2-04) | yes | create and `append` only |

Listing archive targets in `context` matches the user's own archive rule (global `AGENTS.md`, "Detailed records"): curated notes point to archive records, and following that link is the explicit request. Add the full-text query, with the archive condition, to doc 05 §3.

#### 3. On a case-sensitive disk, a create can overwrite a file or make a twin

Doc 07 gives two answers for the same call:

| Doc 07 | Says |
|---|---|
| §6, table row 3 | A tool call that hits a twin's name is "Treated as a note that does not exist" |
| §5.2 | A create is rejected when "The note already exists" |
| §6, table row 1 | `write_note` is "Refused when the key already exists" |
| §5.1 step 4 | "write a temporary file, rename it over the note" |

Walk on Linux, where twins can exist:

1. `baseline/Nutrition.md` and `baseline/nutrition.md` both exist (from git). Neither is indexed.
2. An agent calls `write_note("baseline/nutrition.md", ...)` with no checksum.
3. By §6 row 3 the note does not exist, so the create passes. The rename replaces the human's `nutrition.md`. Rust's `std::fs::rename` replaces an existing target (Rust std docs).

Second walk, with no twins at the start: a human creates `baseline/Sleep.md`. Inside the 2-second gate the index does not know it yet. `write_note("baseline/sleep.md")` reads that exact path from disk (§5.1 step 2), finds nothing on Linux, and creates a twin. At the next sweep both notes drop out of the index, and the human's note disappears from search.

`move_note` has the same hole ("Refused when the target's key already exists").

Fix:
- Write tools check the key against the directory listing on disk, one path segment at a time. Never against the index, and never by opening the exact path. Any match refuses: one file returns that note, several files return all their paths.
- State in doc 07 §6 that "does not exist" applies to reads and links only, never to the create guard.
- Create with a rename that cannot replace: for example `renameat2` with `RENAME_NOREPLACE` on Linux, `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows, or a hard link that fails when the target exists.

#### 4. The bootstrap notice is missing from the edit tools in doc 07

| Doc | Says |
|---|---|
| doc 04 §2.3 | "When an MCP write or edit touches a note in the bootstrap, the tool result states that the note loads as instructions at session start." |
| doc 07 §11 | The notice is listed for `write_note` and `move_note` only |
| doc 07 §9 (`edit_note`), §6.1 (`relink`), §10 (`delete_note`) | No notice |

`edit_note` with `add_relation` on `bootstrap/index.md` is the shortest way to make a note load in every future session. It bypasses doc 04 §2.3's other control ("A new bootstrap note loads only once a loaded note links to it"), because the edit is that link. So the notice is the only signal left, and the contract the code is built from leaves it out. This is a partial regression of DR1-08.

Fix: make the notice a rule in doc 07 §5 for every write tool: `write_note`, `edit_note`, `move_note` into or out of the bootstrap, `relink`, `delete_note`. Doc 04 §2.3 links to it.

#### 5. Links are stored only as written, so ghosts double and resolution has no index

Links now match by key, but nothing in the schema holds a link's key or a note's filename key.

| Symptom | Evidence |
|---|---|
| One ghost listed twice | doc 05 §3.5 groups by `target_ref`, the link "exactly as written". `[[Gall's law]]` and `[[Gall's Law]]` are one ghost by the rule and two rows in `recognitor ghosts`. |
| One link stored twice | The edge key is `(source_id, predicate, target_ref)` (doc 05 §2.2). `[[Caching]]` and `[[caching]]` in one note make two rows, so `context` lists the target twice. |
| No index for filename names | doc 03 §4: a "name" is "a note's title or its filename without `.md`". Doc 05 has `idx_notes_title (namespace, title_key)` and nothing for filenames. Step 4 (unique name in the whole vault) cannot use an index that starts with `namespace`. |
| Every link re-resolved on each add | doc 03 §4 and doc 05 §2.2: after every sync that adds, removes or retitles a note, "all links are re-resolved". A create through `write_note` is an add, and write-through (doc 02 §4) runs it under the write lock. |

At the doc 02 §11 target (10,000 notes, 20,000 links), resolving every link with a filename scan costs about 200 million row checks per added note (estimate: 20,000 links × 10,000 notes, one scan per link). That means seconds under the write lock. `busy_timeout` is 5 s (doc 05 §4.1), so other writers start to fail.

Fix:
- Add `notes.name_key` (the filename stem, lowercased) with an index, and an index on `title_key` alone for step 4.
- Add `edges.ref_key`: the link lowercased, without alias or heading, with `%20` decoded. Use it in the edge primary key and in the ghosts query.
- Re-resolve only the edges whose `ref_key` matches a name or path of the note that was added, removed or retitled, plus that note's own edges. A few index lookups instead of a full pass.

Contracts that justify it: `ghosts` (doc 07 §2) and write-through latency (doc 02 §4, §11).

---

### Medium priority

#### 6. Doc 03's `INSERT` fails again (DR1-11 regression)

Doc 03 §5: `INSERT INTO notes (id, namespace, type, title, path, summary, content, checksum, mtime, size)`. Doc 05 §2.1 now has `title_key TEXT NOT NULL` with no default. The statement fails on the NOT NULL rule, and its `DO UPDATE` list would never refresh `title_key` after a retitle. DR1 found the same bug with `checksum`. The column list lives in two docs, so every schema change breaks one of them. This is not the known column-order gap: a required column is missing.

Fix: delete the SQL from doc 03 §5 and link doc 05 §4.2. The upsert then lives only in doc 05.

#### 7. The `default` namespace has an unreachable entry point and can merge with a folder

- Doc 03 §4: "Notes stored directly in the vault root belong to the `default` namespace." A root `index.md` therefore has the id `index`.
- Doc 05 §3.2: `WHERE id = ?1 || '/index'`. For `default` that is `default/index`, which never matches.
- `default` is not reserved (doc 03 §4 reserves only `bootstrap` and `archive`). A top-level folder `default/` joins the same namespace with a second entry point, and same-namespace resolution (step 3) mixes the two sets of notes.

Real vault: the root holds `AGENTS.md` (the user's global instructions include `~/agent-memory/AGENTS.md`), so `default` exists on the first run.

Fix: reserve `default`. A top-level folder with that key is a lint error and is not indexed. Special-case the entry query for root notes.

#### 8. `load_bootstrap()` has no failure output

Doc 04 §2.2 starts with "Load `bootstrap/index.md`" and never says what happens when that fails. The case rule added new ways to lose the entry: `bootstrap/index.md` next to `bootstrap/Index.md` (twins, neither indexed), or folders `Bootstrap/` and `bootstrap/` ("Twin folders leave out every note under both"). Doc 07 §6 says only `sync`, `rebuild` and `lint` report twins. So the session-start hook injects an empty payload and nobody is told. Ghost links in the tree also drop out silently ("Never follow: ... ghost links").

The user already relies on failures being visible: the `common` index says the first reply "names what loaded, or says it failed".

Fix: when the entry is missing or left out, `load_bootstrap` returns one error line with the cause (missing, or the twin paths), and `recognitor bootstrap` exits non-zero. The payload ends with one line listing the links that did not load. Add `load_bootstrap` to doc 07 §6's list of reporters for this one case.

#### 9. Move reports miss broken links, and the outside-move report has nowhere to go

| Gap | Evidence |
|---|---|
| Prefixed links break silently | doc 07 §6.1: "Only path-style links such as `[[ideas/caching]]` break". A move to another namespace also breaks `[[ideas::Caching]]` (step 2). The real vault has such links: `relates_to [[cognition::index]]` in `recognitor/index`. |
| Name links can turn ambiguous | Doc 03 §4 admits it ("unless the move makes the name ambiguous"), but doc 07 §11 reports only "the path-style links that broke". |
| The moved note's own links | Relative links inside the moved note (`[[../guides/Setup.md]]`, `[x](../ops/Deploy%20Checklist.md)`) now resolve from the new folder and break. Doc 07 reports incoming links only. |
| Moves outside Recognitor | doc 07 §6.2: the sweep "points the user to `relink`". `relink` is MCP-only (doc 07 §2, §12). The sweep usually runs inside an MCP call (doc 02 §4: "on query or tool invocation"), whose output has no slot for the report, and nothing stores it. Once that sweep commits, the old row is gone. |

Fix:
- Define "broken" by the result, not by the link's shape. Under the write lock, `move_note` resolves the affected edges before and after the move, and reports every edge, incoming or outgoing, whose target changed or became a ghost.
- Store probable moves in a small table until they are relinked or dismissed. Show them in `sync` and `ghosts`. Add `relink` to the CLI.

#### 10. Tool arguments given by name have no source namespace

Doc 07 §7: `note` is "an id, path or link name". Doc 03 §4 step 3 needs "the source note's namespace", and a stateless tool call has none. Each of the three projects I can read has a note named `index`, so `read_note("index")` and `context("Index")` are always ambiguous. Doc 07 does not say what comes back: "not found", the first match, or a list. The same applies to `context`, `move_note`, `delete_note` and `relink`.

Fix: tool arguments resolve by steps 1, 2 and 4 only. An ambiguous name returns the matching ids, never "not found". `move_note` and `delete_note` need a unique match.

#### 11. The custom bootstrap directory collides with `<vault>/bootstrap/`

Doc 05 §2.1: "notes from a custom bootstrap directory are stored as `bootstrap/<relative path>`". No doc says what happens to an existing `<vault>/bootstrap/` folder when `bootstrap-dir` points elsewhere. Both map to the same `path` and `id`, which are `UNIQUE`. That happens on the first migration if the old folder stays behind. Also undefined: where `write_note("bootstrap/x.md")` writes, and how `delete_note` moves a bootstrap note into `.recognitor/trash/` when the directory is on another drive. Doc 07 §10's "cannot half-fail" holds only within one drive.

Fix: while `bootstrap-dir` is set, `<vault>/bootstrap/` is not indexed and lint reports it as an error. Tool paths that start with `bootstrap/` map to the configured directory. Bootstrap deletes use a trash folder inside that directory.

#### 12. "Windows, macOS and Obsidian behave the same way" is not true for non-ASCII names

Doc 03 §4 makes this claim. Two gaps:

- **Windows folds differently.** NTFS compares names through its own uppercase table (general knowledge, not verified here), which maps dotless `ı` to `I`. So `kırmızı.md` and `KIRMIZI.md` are one file on Windows, but Unicode lowercasing gives two keys (`kırmızı`, `kirmizi`). `[[KIRMIZI]]` will not resolve to a file Windows treats as the same name, and a create guard that checks keys only would open and replace that file (see DR2-03).
- **No normalization.** `ö` can be one code point, or `o` plus a combining mark (older macOS file systems stored the second form). The keys differ, so a link typed on Windows to a file named on a Mac stays a ghost.

Latent today: 0 of 48 file names in the three projects I can read contain non-ASCII characters. The user writes Turkish, so it will not stay latent.

Fix: normalize to NFC, then lowercase. Replace the claim with what Recognitor does (NFC, then Unicode lowercasing), and say that Windows can still treat two different keys as one file, which is why write tools check the disk (DR2-03).

#### 13. Stale and disagreeing statements

| Where | Problem | Fix |
|---|---|---|
| doc 02 §3 vs doc 03 §2 | "1-sentence note summary" vs "cut at a sentence boundary within 300 characters". Known from shadow mode; listed here because the two docs contradict each other. | Doc 02 links doc 03 §2 and stops restating it. |
| doc 02 §8 vs doc 04 §3.1, doc 07 §3 | "lint reports every namespace without an `index.md`" is missing from doc 02's numbered list. | Add it as check 9. |
| doc 02 §8 vs doc 07 §6 | Only `lint --strict` "exits non-zero", but doc 07 says that for twins "`lint` fails". | Errors exit non-zero in plain `lint`; `--strict` turns warnings into errors. |
| doc 02 §4 | The sweep "compares each file's `(mtime, size)`". Doc 03 §4 and doc 05 §2.1 add two more signals: path spelling and twin grouping. | Add both to the sync rules table. |
| Config keys | `bootstrap-dir` (doc 02 §10, doc 04 §2.1), `bootstrap.max_tokens` (doc 04 §2.2), `read.max_tokens` (doc 07 §7.2), `db_path` (doc 05 §5). | One style, for example `bootstrap.dir`, `read.max_tokens`, `db.path`. |
| Contract first | `observations.id` and `idx_obs_category` serve no doc 07 tool. `search`, the main tool, has no query in doc 05 §3. | Add the search query (DR2-02). Keep the two items only if a doc 07 tool uses them. |

---

### Process

- The paragraphs that state the case rule agree. The contradictions sit next to them: doc 05 §3 queries, doc 07 §3, §8, §9, §10, and doc 02 §3, §4, §8. When a term changes, search every doc for the old word ("id", "exists", "path-style").
- Rules live away from where code is built. Store rules sit in docs 02, 04 and 05, but the code is built from doc 07. Column lists sit in docs 03 and 05. Give each rule one home and link to it.

### Suggested next move

Add the store access table from DR2-02 to doc 07 §1. One small table closes the archive leak (DR2-02), the missing bootstrap notice (DR2-04) and the `load_namespace("bootstrap")` gap. It also gives the shadow-mode code one place to read store rules from.

---

### DR2 Finding Tracker

| Id | Finding | Severity | Status |
| :--- | :--- | :--- | :--- |
| DR2-01 | "id" means both the spelled path and the lowercase key; doc 05 queries return keys and lack the order doc 07 promises | High | Resolved: id and key terms (doc 03 §4, doc 07 §6); `key` columns, queries return `path` in the promised order (doc 05 §2.1-2.3, §3); title compared by key (doc 07 §4.2) |
| DR2-02 | Archive content leaks into normal search; doc 07 has no per-store rules (archive request, append only, `load_namespace("bootstrap")`) | High | Resolved: store access table (doc 07 §1.1), linked from docs 02, 04 and 05; full-text query with the archive condition (doc 05 §3.7) |
| DR2-03 | On a case-sensitive disk, a create or move can overwrite a twin or create one | High | Resolved: creates and moves check the folders on disk by key and use a rename that cannot replace; twins refuse creates (doc 07 §5.1-5.3, §6) |
| DR2-04 | Bootstrap notice missing from `edit_note`, `relink` and `delete_note` in doc 07 (partial DR1-08 regression) | High | Resolved: the store access table gives every write tool the bootstrap notice, and a move checks both stores (doc 07 §1.1) |
| DR2-05 | Links stored only as written: ghosts and edges doubled by case, no filename index, full re-resolution on every add | High | Resolved: `edges.ref_key` and `notes.name_key` with indexes; ghosts grouped by key; re-resolution only for links that match the changed note's names (doc 03 §4, doc 05 §2.1, §2.2, §3.5) |
| DR2-06 | Doc 03 `INSERT` omits `title_key` (DR1-11 regression) | Medium | Resolved: SQL removed from doc 03 §5, which links doc 05 §4.2; the upsert updates every column except `pk` and `key` |
| DR2-07 | `default` namespace: entry point unreachable, merges with a `default/` folder | Medium | Resolved: `default` is an ordinary namespace name, like any top-level folder; files in the vault root are not notes and are never indexed, and writes there are refused (doc 03 §4, doc 05 §2.1, doc 07 §11) |
| DR2-08 | `load_bootstrap()` has no failure output for a missing or twinned entry or for ghost links | Medium | Resolved: a missing entry point returns one error line and a non-zero CLI exit; ghost links are listed at the end of the payload; `lint` names twins (doc 04 §2.2, doc 07 §12) |
| DR2-09 | Move reports miss prefixed, ambiguous and outgoing links; the outside-move report has no destination and the CLI has no `relink` | Medium | Resolved: `move_note` reports every link the move changed; outside moves leave ghosts, `ghosts` hints at the new place without a stored table, and the CLI gets `relink` (doc 03 §4, doc 05 §3.5, doc 07 §2, §6.1, §6.2, §11, §12) |
| DR2-10 | Tool arguments given by name have no source namespace; `index` is always ambiguous | Medium | Resolved: tool arguments resolve by steps 1, 2 and 4; several matches return the ids and run nothing (doc 07 §6.3) |
| DR2-11 | Custom bootstrap directory collides with `<vault>/bootstrap/`; writes and trash undefined | Medium | Resolved: no custom bootstrap directory in this version; the bootstrap is always `<vault>/bootstrap/` (docs 02, 03, 04 §1, §2.1, doc 05 §2.1, §2.6) |
| DR2-12 | Lowercasing claim is wrong for non-ASCII names; no Unicode normalization | Medium | Resolved: file and folder names are ASCII only, tested on NTFS and ext4; non-ASCII names are refused or left out like case twins (doc 02 §8, doc 03 §4, doc 05 §2.1, doc 07 §6, §11) |
| DR2-13 | Stale and disagreeing statements (summary, lint list and exit code, sweep signals, config keys, unused schema items) | Medium | Resolved: summary links doc 03 §2; lint check 9 and exit codes; name signals in the sweep rules; config keys as `section.name`; `observations.id` and `idx_obs_category` removed (doc 02 §3, §4, §8, doc 05 §2.3, §5) |

---

## DR1: 2026-11-14, full design review (docs 01-05)

**Score: 6/10.** The core idea is right. The specs are thin exactly where the code will break first.

The docs say "Resolved" eight times. Yet the bootstrap query, as written, would not load your own ADHD profile.

---

### What's solid (keep it)

| Decision | Why it holds up |
|---|---|
| Files are the source of truth, the database is a rebuildable copy | Schema changes need no migrations. Bump `PRAGMA user_version`, and on a mismatch rebuild. It's your best feature and the docs don't claim it. Say it. |
| Links to notes that don't exist yet (ghost notes) | You get a backlog of unwritten notes for free. |
| Typed `## Relations` plus plain inline links | Prose stays clean, and the predicates stay explicit. |
| Two-stage retrieval (outline first, full note on request) | Right shape for a limited context window. |
| Sync on demand, no background watcher | Behaves the same after sleep/wake, branch switches and cloud sync. |
| sqlite-vec as the vector store | It's pre-1.0, but the database is disposable, so swapping it out later is cheap. |

---

### Critical: breaks on the first real run

#### 1. Bootstrap fails on your actual vault

| Problem | Evidence |
|---|---|
| `namespace = 'common'` is an exact match | ADHD profile lives at `common/working-style/adhd-profile`, and All In at `common/personas/all-in`. Only 3 of the 9 notes you load today would match. |
| No way to pick which notes load | `tier IN ('profile','behavior')` loads **every** persona in `common/personas/`. Your `Answer style` rule says "at most one persona". |
| Ordering is wrong | `ORDER BY tier ASC` is alphabetical, so `behavior` comes before `profile`. |
| The budget is fiction | Doc promises < 500 tokens. Today's set is about 2,500 tokens (my estimate: characters ÷ 4). |
| The two docs disagree | Doc 04 filters on `type IN (...)`, doc 05 filters on `tier IN (...)`. |

The real problem: the multi-step reading procedure was not only fragile. It also encoded **which notes to load**. Move that choice into data and keep the single call:

```markdown
# Bootstrap            (type: manifest)
## Relations
- loads [[Identity]]
- loads [[ADHD profile]]
- loads [[Output rules]]
- loads [[All In]]      <- switching persona = editing this one line
```

`get_bootstrap()` then follows the manifest's edges, with a depth limit. The token budget becomes a lint warning, not a promise. This also fixes #8.

#### 2. Link resolution and note identity are not specified

- **Resolution:** how does `[[Security Architecture]]` become `backend/security-architecture`? Same namespace only? Match by title or by filename? What about collisions (`Auth Flow.md` vs `auth-flow.md`)? Windows is case-insensitive, Linux is case-sensitive.
- **Renames:** renaming a file silently turns every incoming edge into a ghost. Nothing rewrites the files that link to it.
- **Namespace source:** doc 03 puts `namespace: backend` in frontmatter, but doc 02 §8 infers it from the path. When they disagree, which wins?
- **Hierarchy:** is `common/working-style` a namespace or just a folder? Either answer breaks something above.
- **Obsidian:** `[[ns::Note]]` is not Obsidian syntax. Obsidian shows a broken link, and `:` isn't even allowed in Windows filenames.

Fix:

```sql
target_ref TEXT NOT NULL,  -- the link as written: "backend::Auth Flow"
target_id  TEXT,           -- resolved after each sync; NULL = ghost
```

- Make the path the only source of namespace. A frontmatter `namespace:` that disagrees is a lint error.
- Resolution order: exact path → same folder → unique filename in the vault → title/alias → ghost.
- Prefer path-style links like `[[backend/auth-flow]]`, which Obsidian resolves natively. Accept `::` only when importing from Basic Memory.

#### 3. Unstable `rowid` silently corrupts search

| Mechanism | Result |
|---|---|
| `notes` uses `id TEXT PRIMARY KEY`, so its `rowid` is implicit. SQLite's `VACUUM` may renumber implicit rowids (it's in the SQLite docs). | The full-text table and `notes_vec` both key on `rowid`. They end up pointing at the wrong notes. |
| An upsert written as `INSERT OR REPLACE` does not fire delete triggers unless `recursive_triggers` is on. | Stale full-text rows and orphaned vectors pile up. |
| Foreign keys are off by default and need `PRAGMA foreign_keys=ON` on every connection. | "Deleted files cascade automatically" is false by default. |

Fix:

```sql
pk INTEGER PRIMARY KEY,   -- stable rowid; full-text and vectors key on this
id TEXT NOT NULL UNIQUE,
```

- Write updates as `ON CONFLICT(id) DO UPDATE` only, never `REPLACE`.
- List the required PRAGMAs in the spec.
- Change the full-text trigger to `AFTER UPDATE OF title, summary, content`. Today a change to `mtime` alone rewrites the full-text row.

---

### High priority

#### 4. The embedding pipeline doesn't exist yet
- **No model, no runtime, no schedule.** 384 dimensions points at small models like MiniLM or bge-small. Running one locally (for example through ONNX) breaks both "single static binary" and "sub-millisecond startup".
- **Sync blocks on it.** Re-embedding changed notes during sync stalls the query.
- **Long notes get cut.** These models read at most 256 tokens (MiniLM) or 512 (bge-small). A long note's embedding only reflects its top part.
- **No staleness tracking.** No trigger fires on update and no model id is stored. Change the model and the vectors become silently wrong.
- **Rebuild gets expensive.** "Disposable" turns into minutes of CPU.

Fix:
- Use a table `embeddings(note_pk, chunk, model_id, content_hash, vector)`.
- Cache vectors by `(model_id, content_hash)` in a separate file that survives `rebuild`.
- Embed observations or chunks, not whole notes.
- Embed off the query path. Full-text search covers the gap while vectors catch up.

#### 5. The vector search filters after ranking
The vector query returns the top 10 across the whole vault first. The `JOIN` filters by namespace afterwards, so it can return 0 rows. Put the filter columns inside the vector table (sqlite-vec ≥ 0.1.6) so the filter runs during the search:

```sql
CREATE VIRTUAL TABLE chunks_vec USING vec0(
  embedding float[384] distance_metric=cosine,
  ns_root text,   -- filtered during the search, not after
  tier text
);
```

#### 6. Sync under concurrency
- **Several processes at once.** Each agent session runs its own MCP server, plus the CLI. You need WAL mode, `busy_timeout`, and the sweep inside `BEGIN IMMEDIATE`. Store `last_sweep_time` in a database `meta` table. If it lives in process memory, CLI calls never benefit from the 2-second window.
- **Compare `(mtime, size) !=`, not `>`.** Cloud sync can deliver files with older timestamps.
- **Same-timestamp edits.** An edit that lands in the same timestamp tick as the last index pass gets missed. Re-hash any file whose timestamp is ≥ the start of the last sweep (git solves this the same way).
- **Skip unchanged content.** Use `checksum` to skip re-parsing when the timestamp changed but the content didn't. `git checkout` touches every file.
- **Read your own writes.** `write_note` must update the index in the same call. Otherwise a search within 2 seconds misses the agent's own write.
- **Lost updates.** Writes need `expected_checksum`, as Basic Memory does today. Write via a temp file and rename. Never regenerate Markdown from the parse tree. Patch byte ranges instead; pulldown-cmark gives you the source offsets.
- **Database location.** Keep it outside synced folders. WAL files under Dropbox/OneDrive are a known way to corrupt SQLite. Use `%LOCALAPPDATA%\recognitor\<vault-hash>.db`.

#### 7. Cross-domain traversal contradicts your own rules
Your AGENTS.md says "Never read other projects unless I ask". Doc 02 §5 lets traversal cross namespaces freely. The default filter only limits the **starting** search. One `relates_to` link from a coding note pulls `baseline` health summaries into a work session.

Fix:
- Make session scope an **allowlist** (`[recognitor, common, compass]`), not just a default.
- Mark sensitive namespaces as `sealed`: never traversed unless the allowlist names them.

#### 8. Notes can promote themselves into the system prompt
Bootstrap injects every `type: preference` note in `common` as instructions. Any agent that can call `write_note` can add one. That note then rides into every future session: a prompt injection that persists, triggered by one YAML key.
- The manifest from #1 closes this: a new note isn't loaded until a human links it.
- Also block MCP writes from creating `profile`, `behavior` or `rule` notes.
- **Archive fails open.** A typo like `type: sesion_record` lands a raw transcript in the default `knowledge` tier, so it shows up in normal search. Add a path rule: anything under `archive/**` is archive tier, whatever its `type`.

---

### Medium priority

#### Overclaims (you'll catch these on sight)

| Claim | Reality |
|---|---|
| "Sub-millisecond startup" | Process creation on Windows alone usually takes several ms (estimate, measure it). The MCP server stays running, so startup only matters for the CLI. The real win is fast queries inside one process. |
| "20 nodes in under 300 tokens" | About 15 tokens per node for id, title and predicate, plus about 25 for the summary. That's roughly 800 tokens. |
| Edge "weights" (docs 01, 02 §6, 03) | No column holds them and the Markdown has no syntax for them. Say they're computed: typed links count more than inline ones, and hubs are dampened. |
| Basic Memory's vector search is "basic/separate" | The `bm_search` tool in this session offers hybrid text + semantic search. The comparison table undersells the baseline. |

#### Parser collisions with your real vault
- Obsidian tasks `- [ ] todo` and `- [x] done` would parse as observations.
- Link bullets like `- [Title](file.md)` would parse as category `Title`. Use a regex like `^[a-z][a-z0-9_-]*\]\s`, and never match when `(` follows the bracket.
- `- has_part [[Feedback loops]] (what you build…)` appears in your own compass notes, but doc 03 says Relations lines must "strictly conform". Your vault would produce warnings everywhere. Store that trailing text in `edges.context`.
- Undefined link forms: `[[A|alias]]`, `[[A#heading]]`, `![[embed]]`.
- The summary paragraph has no length limit. Cap it.
- The `@effective:` qualifier syntax only appears in a SQL comment. Specify it in doc 03.

#### Schema nits
- **`edges` table:**
  - `AUTOINCREMENT` and the `id` column are unused. Make it `WITHOUT ROWID` with `PRIMARY KEY (source_id, predicate, target_ref)`.
  - `idx_edges_source` and `idx_edges_lookup` duplicate the start of the unique index.
  - Backlinks need an index on `(target_id, predicate)`, which is missing.
- **Duplicate facts:** `A has_part B` plus `B part_of A` stores one fact as two rows. Store one direction at ingest.
- **Two sources for "superseded":** your memory rules use both a `supersedes` edge and `status: superseded`. Derive the status from the edge and lint any mismatch.
- **Observations:**
  - Ids built as `note_id:line` change on every edit, so agents end up citing stale ids.
  - There's no full-text index on observations. "Surgical retrieval" can only filter by category, not search the text.
- **Doc 03 bug:** its `INSERT` leaves out `checksum`, which is `NOT NULL` with no default. It fails as written.

#### Tiers vs your vault
- Your vault uses these types: `index, hub, profile, preference, reference, queue, session_record, record, plan, decision, concept, process`. The `rule` tier maps from `instruction` and `workflow`, which your vault never uses, so that tier starts empty.
- Nothing defines where the **active namespace** comes from: env var, working directory, config, or a tool argument.
- You enforce per-type schemas today with `bm schema validate --strict` in a git hook. Doc 02 §8 defers schemas "until drift demands them". Migrating would drop a check you already rely on.

#### Doc hygiene
- README links only docs 01 and 02.
- Doc 01 §7 still says vectors and table definitions are "Open", and still mentions "Rust-native HNSW".
- `specification` (doc 03) vs `spec` (doc 04).
- The observations table definition is duplicated in docs 04 and 05, so they'll drift.
- "Zero-exit-code gate" is backwards. A gate exits non-zero on failure.

#### petgraph probably isn't needed
Personalized PageRank is about 30 lines of iteration over plain arrays. As far as I know, petgraph doesn't ship a personalized version anyway. A CLI that loads the whole edge list on every call pays more for the load than for the walk. Add it only if a plain SQL walk with hub dampening turns out not to be enough.

---

### Process

1. **The order is backwards.** Doc 02 §9 postpones the MCP/CLI design. The tool contracts are the requirements, and the schema should follow from the queries. Writing the bootstrap query is what exposed gap #1. The next doc should list each tool with its input, output and token budget.
2. **No target scale.** The README promises to fix "scaling bottlenecks" without saying at what size. Pick numbers, for example 10k notes, 100k observations and 50k edges, with typical and worst-case latency per operation on Windows. Add a recall test: 20–30 real questions from your sessions, scored against Basic Memory.

### Suggested next move: a read-only "shadow mode"

Point Recognitor read-only at the existing `~/agent-memory` vault. Build only the indexer, full-text search, the bootstrap manifest and `context`. No writes, no vectors.

You get something working early, and it answers half of this review with data: real token counts, real link-resolution failures, the real ghost count. Low effort, visible result.

### Compass

- **Feedback loops:** agents link what they retrieve, and the ranking favors what is linked. That reinforcing loop makes hubs snowball. Hub dampening is the balancing loop against it, but it's only a guess until you measure how links grow on real data. The 2-second sync window is a delay in another balancing loop, and the write-through in #6 removes it where it hurts.
- **Gap:** Gall's law (working complex systems grow out of working simple ones) fits better and isn't studied yet. `Candidates` is already at its 5-line cap, so I didn't add it.

---

### DR1 Finding Tracker

| Id | Finding | Severity | Status |
| :--- | :--- | :--- | :--- |
| DR1-01 | Bootstrap fails on the actual vault (namespace match, no selection, ordering, budget, doc mismatch) | Critical | Resolved: three stores by location; `load_bootstrap()` link tree (doc 02 §10, doc 04 §2, doc 05 §3.1) |
| DR1-02 | Link resolution and note identity are not specified | Critical | Resolved: path as id (permalink first, replaced by the path on 2026-11-15), top-level folder as namespace, resolution order, move and relink flow (doc 03 §4, doc 05 §2.1-2.2, doc 07 §6) |
| DR1-03 | Unstable `rowid` silently corrupts full-text and vector search | Critical | Resolved: doc 05 §2.1, §2.4, §4 |
| DR1-04 | Embedding pipeline doesn't exist yet | High | Resolved: kept as optional feature, built later; model and runtime chosen then (doc 02 §2, doc 05 §2.5) |
| DR1-05 | Vector search filters after ranking | High | Resolved: doc 05 §2.5, §3.6 |
| DR1-06 | Sync under concurrency (multi-process, timestamps, write-through, lost updates, DB location) | High | Resolved: doc 02 §4, doc 05 §2.6, §4, §5 |
| DR1-07 | Cross-domain traversal contradicts the per-project read rule | High | Superseded: allowlist and sealed namespaces dropped on 2026-11-15; a namespace is a perspective, not a boundary, with no isolation in early versions (doc 02 §5, doc 07 §1) |
| DR1-08 | Notes can promote themselves into the system prompt; archive tier fails open | High | Resolved: archive path rule, bootstrap write flag, new bootstrap notes load only once linked (doc 04 §2.3, §4) |
| DR1-09 | Overclaims (startup, traversal tokens, edge weights, Basic Memory comparison) | Medium | Resolved: claims restated or deferred to the benchmark; weights computed; comparison rebuilt from Basic Memory v0.24 (doc 01 §2-5, doc 02 §6, doc 03, README) |
| DR1-10 | Parser collisions with the real vault | Medium | Resolved: observation, relation, link-form, Markdown-link, code, qualifier and summary rules, checked against Basic Memory v0.24 (doc 03 §2-4, doc 05 §2.2-2.3, doc 04 §4) |
| DR1-11 | Schema nits (edges table, duplicate facts, superseded status, observation ids and search, doc 03 `INSERT`) | Medium | Resolved: `WITHOUT ROWID` edges, inverse pairs merged in queries, edge-based superseded status, content-hash observation ids (observation ids later removed, DR2-13), `observations_fts` (doc 05 §2.2-2.3, §4.2, doc 03 §3, §5) |
| DR1-12 | Tiers vs the real vault (empty `rule` tier, active namespace source, schema gate) | Medium | Resolved: tiers replaced by stores, namespace passed by the agent after `load_namespace` (stateless server; doc 04 §3.1, doc 07 §3), vault-wide TOML type schemas with `lint --strict` gate (doc 02 §8) |
| DR1-13 | Doc hygiene | Medium | Resolved: README links all docs; doc 01 §7 status current; doc 04 renamed to `04-information-stores-and-lifecycle.md`; observations DDL lives only in doc 05; `spec` used consistently; strict lint exits non-zero (doc 02 §8) |
| DR1-14 | petgraph probably isn't needed | Medium | Resolved: graph library demoted to an implementation detail; plain link-following first, Personalized PageRank only if the benchmark demands it (doc 01 §5, doc 02 §1, §6) |
| DR1-15 | Tool contracts should come before the schema | Process | Resolved: `07-interface-contracts.md` is the next design doc; schema changes must be justified by a contract (doc 02 §9) |
| DR1-16 | No target scale or benchmark | Process | Resolved: 100× scale target, p95 latency targets, retrieval benchmark against Basic Memory (doc 02 §11) |

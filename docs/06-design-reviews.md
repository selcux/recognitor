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
| [DR1](#dr1-2026-11-14-full-design-review-docs-01-05) | 2026-11-14 | Full design review, docs 01-05 | `9db3af1` | 6/10 | 16 resolved, closed 2026-11-15 |

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
| DR1-02 | Link resolution and note identity are not specified | Critical | Resolved: permalink as id, top-level folder as namespace, resolution order (doc 03 §4, doc 05 §2.1-2.2) |
| DR1-03 | Unstable `rowid` silently corrupts full-text and vector search | Critical | Resolved: doc 05 §2.1, §2.4, §4 |
| DR1-04 | Embedding pipeline doesn't exist yet | High | Resolved: kept as optional feature, built later; model and runtime chosen then (doc 02 §2, doc 05 §2.5) |
| DR1-05 | Vector search filters after ranking | High | Resolved: doc 05 §2.5, §3.5 |
| DR1-06 | Sync under concurrency (multi-process, timestamps, write-through, lost updates, DB location) | High | Resolved: doc 02 §4, doc 05 §2.6, §4, §5 |
| DR1-07 | Cross-domain traversal contradicts the per-project read rule | High | Resolved: allowlist + sealed namespaces (doc 02 §5) |
| DR1-08 | Notes can promote themselves into the system prompt; archive tier fails open | High | Resolved: archive path rule, bootstrap write flag, new bootstrap notes load only once linked (doc 04 §2.3, §4) |
| DR1-09 | Overclaims (startup, traversal tokens, edge weights, Basic Memory comparison) | Medium | Resolved: claims restated or deferred to the benchmark; weights computed; comparison rebuilt from Basic Memory v0.24 (doc 01 §2-5, doc 02 §6, doc 03, README) |
| DR1-10 | Parser collisions with the real vault | Medium | Resolved: observation, relation, link-form, Markdown-link, code, qualifier and summary rules, checked against Basic Memory v0.24 (doc 03 §2-4, doc 05 §2.2-2.3, doc 04 §4) |
| DR1-11 | Schema nits (edges table, duplicate facts, superseded status, observation ids and search, doc 03 `INSERT`) | Medium | Resolved: `WITHOUT ROWID` edges, inverse pairs merged in queries, edge-based superseded status, content-hash observation ids, `observations_fts` (doc 05 §2.2-2.3, §4.2, doc 03 §3, §5) |
| DR1-12 | Tiers vs the real vault (empty `rule` tier, active namespace source, schema gate) | Medium | Resolved: tiers replaced by stores, active namespace set by `enter_namespace` (doc 04), vault-wide TOML type schemas with `lint --strict` gate (doc 02 §8) |
| DR1-13 | Doc hygiene | Medium | Resolved: README links all docs; doc 01 §7 status current; doc 04 renamed to `04-information-stores-and-lifecycle.md`; observations DDL lives only in doc 05; `spec` used consistently; strict lint exits non-zero (doc 02 §8) |
| DR1-14 | petgraph probably isn't needed | Medium | Resolved: graph library demoted to an implementation detail; plain link-following first, Personalized PageRank only if the benchmark demands it (doc 01 §5, doc 02 §1, §6) |
| DR1-15 | Tool contracts should come before the schema | Process | Resolved: `07-interface-contracts.md` is the next design doc; schema changes must be justified by a contract (doc 02 §9) |
| DR1-16 | No target scale or benchmark | Process | Resolved: 100× scale target, p95 latency targets, retrieval benchmark against Basic Memory (doc 02 §11) |

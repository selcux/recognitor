# Recognitor Information Architecture & Lifecycle Specification

This specification defines the 5-tier classification framework for information managed by Recognitor, outlining how different categories of notes are partitioned by rate of change, frontmatter type, loading policy, and harness integration.

---

## 1. The Pacing Layers of Agent Context

Information in an autonomous agent environment changes at vastly different speeds. Tangling slow-moving grounding facts with fast-moving domain knowledge leads to either bloated system prompts or fragile procedural retrieval.

Recognitor formalizes a **5-Tier Pacing Architecture** to uncomplect these concerns:

```
+-------------------------------------------------------------+
|  Tier 1: Grounding Profile (Identity, ADHD, Focus Windows)  | <-- Bootstrap: loaded once at session start
+-------------------------------------------------------------+
|  Tier 2: Behavioral Frame (Personas, Tone, Output Style)    | <-- Filter: active session switch
+-------------------------------------------------------------+
|  Tier 3: Operational Rules (Tool Rules, Harness Workflows)  | <-- Environment: bound to repo / execution
+-------------------------------------------------------------+
|  Tier 4: Domain Knowledge PKM (Specs, Concepts, Decisions)  | <-- On-Demand: cold storage (DEFAULT TIER)
+-------------------------------------------------------------+
|  Tier 5: Deep Archive (Session Records, Raw Transcripts)    | <-- Frozen: excluded unless requested
+-------------------------------------------------------------+
```

---

## 2. Information Tier Definitions

| Tier | Name | Description & Examples | Frontmatter `type` | Loading Policy | Token Budget Target |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Tier 1** | **Grounding Profile** | Stable facts about the user: identity, cognitive traits, working styles, focus windows. | `profile` | **Bootstrap:** Injected automatically at session start. Persistent throughout session. | < 300 tokens |
| **Tier 2** | **Behavioral Frame** | Tone and interaction switches: output formatting, writing rules, active persona. | `preference` | **Filter:** Active switch chosen at session start. Swappable mid-conversation. | < 250 tokens |
| **Tier 3** | **Operational Rules** | Harness-level instructions: tool usage workflows, repository constraints, build protocols. | `instruction`, `workflow` | **Environment:** Bound to repository root (`AGENTS.md`) or tool invocation. | ~500 tokens |
| **Tier 4** | **Domain Knowledge** | Core PKM corpus: technical specifications, architectural decisions, code designs, thinking concepts. | `decision`, `concept`, `spec`, `note` *(or undeclared)* | **On-Demand:** Cold storage in SQLite. Queried via graph walks or vector search only. | Pay-per-query (compact skeletons) |
| **Tier 5** | **Deep Archive** | Detailed session logs, raw transcript dumps, verbatim error traces. | `archive`, `session_record` | **Frozen:** Excluded from standard searches. Queried only on explicit request. | Zero until requested |

---

## 3. The Default Tier Rule

To ensure zero friction for casual note-taking and external editor integration (e.g. Obsidian):

> **Rule:** If a note does not declare a `type` in its YAML frontmatter, or if the `type` is unclassified, Recognitor automatically assigns it to **Tier 4 (Domain Knowledge / PKM)**.

- Casual Markdown notes without frontmatter are never injected into startup context.
- They remain indexed, searchable, and traversable via the knowledge graph without accidental prompt pollution.
- Upgrading a note to Tier 1 or Tier 2 requires an explicit frontmatter declaration (`type: profile` or `type: preference`).

---

## 4. Observation Granularity & Open Categories

Recognitor treats the **Note** as the canonical file on disk, but treats individual **Observations** as indexed atomic units in SQLite:

```markdown
## Observations
- [fact] Access tokens expire after 15 minutes.
- [rule] Refresh tokens must be rotated on every exchange.
- [avoid] Storing refresh tokens in browser localStorage.
```

### Database Representation

SQLite maintains an open string column for observation categories, avoiding rigid enums:

```sql
CREATE TABLE observations (
    id TEXT PRIMARY KEY,
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    category TEXT NOT NULL,
    content TEXT NOT NULL,
    qualifiers TEXT,
    line_number INTEGER NOT NULL
);

CREATE INDEX idx_obs_category ON observations(category);
CREATE INDEX idx_obs_note ON observations(note_id);
```

- **Open Vocabulary:** Users and models may author arbitrary brackets (`[fact]`, `[rule]`, `[avoid]`, `[decision]`, `[hypothesis]`, `[measurement]`).
- **Surgical Retrieval:** Agents can query specific rules or facts without loading the complete parent note document into prompt context.

---

## 5. Harness Integration: The One-Shot Bootstrap Pattern

Legacy systems require complex, multi-hop procedural instructions in harness configurations (e.g. "Read note A, follow link B, skip link C, read note D").

Recognitor replaces this fragility with the **One-Shot Bootstrap Tool**:

### Tool: `recognitor.get_bootstrap()`

1. **Harness Instruction:**
   ```markdown
   Session start: Call recognitor.get_bootstrap() and apply the returned profile and persona.
   ```
2. **Engine Execution:**
   ```sql
   SELECT title, content FROM notes 
   WHERE type IN ('profile', 'preference') 
     AND namespace = 'common';
   ```
3. **Result:** Recognitor compiles the active user profile, communication preferences, and active persona into a compact, unified Markdown payload (< 500 tokens) returned in a single tool call.

### Tier-Filtered Domain Search

During active task execution, knowledge retrieval queries are scoped to prevent cross-tier pollution:

```json
recognitor.search(
    query="authentication tokens",
    tiers=[4]
)
```

The database query automatically filters out profiles, personas, and harness rules, ensuring LLM attention remains focused strictly on relevant domain knowledge.

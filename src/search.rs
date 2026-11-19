//! Full-text search (doc 07 §4, doc 05 §3.7).
//!
//! The candidates come from `notes_fts` ordered by BM25 (doc 05 §3.7). The `ns` boost reorders them, paging cuts the
//! page, and the observation lines under each hit come from `observations_fts`. Nothing here opens a vault file.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};

use crate::error::{Error, Result};
use crate::paths::{note_id, titled_id};

/// Hits per page (doc 07 §4.2). A tuning parameter, set with the retrieval benchmark; it is also the size of the fill.
pub const PAGE_SIZE: usize = 5;
/// Candidates fetched before the `ns` boost and paging (doc 05 §3.7: `LIMIT 50`). The hits of one query never exceed
/// this number, so the "N more" count is capped by it too.
pub const CANDIDATES: usize = 50;
/// Observation lines shown under one hit (doc 07 §4.2).
pub const MAX_OBSERVATIONS: usize = 3;

/// PLACEHOLDER: the docs leave the boost values to the retrieval benchmark (doc 07 §4, doc 02 §11). A note in `ns`
/// gets its score multiplied by `1 + NS_BOOST`.
pub const NS_BOOST: f64 = 0.5;
/// Longest fact line, in characters, without its indent (doc 07 §4.2, "Budget"). A tuning parameter, to be set with
/// the retrieval benchmark.
pub const LINE_MAX_CHARS: usize = 200;
/// Tokens a page may take, estimated as characters ÷ 4 (doc 07 §4.2, "Budget"). A tuning parameter.
pub const PAGE_TOKEN_BUDGET: usize = 1000;

/// PLACEHOLDER: as [`NS_BOOST`], for a note outside `ns` that a note in `ns` links to.
pub const LINKED_BOOST: f64 = 0.25;

/// How candidate notes and links are boosted during search ranking (doc 07 §4, doc 02 §6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingConfig {
    /// Boost applied to notes inside the target namespace: score = -rank * (1 + ns_boost).
    pub ns_boost: f64,
    /// Boost applied when a note in `ns` links to this candidate via an explicit `## Relations` edge (`is_inline = 0`).
    pub rel_boost: f64,
    /// Boost applied when a note in `ns` links to this candidate via an inline link (`is_inline = 1`).
    pub inline_boost: f64,
    /// Hub dampening scaling for high-degree source notes.
    pub hub_dampening: HubDampening,
    /// Maximum cumulative linked boost for a single candidate.
    pub max_linked_boost: f64,
    /// Number of candidates fetched from FTS5 before re-ranking.
    pub candidates: usize,
    /// Number of hits per page.
    pub page_size: usize,
}

/// How high-degree source notes have their outgoing edge weights dampened (doc 02 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HubDampening {
    #[default]
    None,
    Log,
    Sqrt,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            ns_boost: NS_BOOST,
            rel_boost: LINKED_BOOST,
            inline_boost: LINKED_BOOST,
            hub_dampening: HubDampening::None,
            max_linked_boost: 1.0,
            candidates: CANDIDATES,
            page_size: PAGE_SIZE,
        }
    }
}

/// Diagnostic trace from search execution for benchmarks and evaluation.
#[derive(Debug, Clone)]
pub struct SearchTrace {
    pub page: SearchPage,
    /// Keys of all FTS5 candidate notes (in raw BM25 rank order) before re-ranking.
    pub candidate_keys: Vec<String>,
    /// Keys of all candidate notes after re-ranking and boost calculation.
    pub ranked_keys: Vec<String>,
}

/// The line before the hits that match only some of the words (doc 07 §4.2, "Some of the words").
pub const FILL_SEPARATOR: &str = "Notes with only some of the words:";

/// One observation line under a hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationLine {
    pub category: String,
    /// The qualifier as written, `@` included, when the fact has one (doc 07 §4.2).
    pub qualifier: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The id as written (doc 03 §4).
    pub id: String,
    /// Set only when the title differs from the filename by key (doc 07 §4.2).
    pub title: Option<String>,
    pub summary: Option<String>,
    pub observations: Vec<ObservationLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    pub hits: Vec<Hit>,
    /// 1-based page number.
    pub page: usize,
    /// Number of the first hit on this page, counted over all pages.
    pub first_number: usize,
    /// Hits after this page.
    pub more: usize,
    /// Hits over all pages (at most [`CANDIDATES`]).
    pub total: usize,
    /// Hits per page of this search.
    pub page_size: usize,
    /// All notes that match, when more than [`CANDIDATES`] do (doc 07 §4.2, "Kept matches"). Counted only when the
    /// candidate query returned [`CANDIDATES`] rows.
    pub matches: Option<usize>,
    /// The query as given, without its own `"` characters and trimmed: the no-hits line quotes it.
    pub query: String,
    /// Where the fill starts in `hits` (doc 07 §4.2, "Some of the words"): the hits from this index on match only
    /// some of the words, and the line `Notes with only some of the words:` stands before them. `None` when the page
    /// has no fill. The fill belongs to page 1 only.
    pub fill_start: Option<usize>,
}

struct Candidate {
    key: String,
    path: String,
    namespace: String,
    title: String,
    title_key: String,
    name_key: String,
    summary: Option<String>,
    /// `bm25()`: lower is better.
    rank: f64,
    score: f64,
}

/// Runs the full-text query (doc 07 §4). `ns` is a perspective, not a filter. `page` is 1-based.
///
/// The text never reaches FTS5 as query syntax: [`match_expression`] quotes every word (doc 07 §4, "Query text").
/// A query with no words is refused with [`Error::EmptyQuery`].
pub fn search(conn: &Connection, query: &str, ns: Option<&str>, page: usize) -> Result<SearchPage> {
    search_with_config(conn, query, ns, page, &RankingConfig::default())
}

/// [`search`] with another page size; the tests use it to reproduce the example in doc 07 §4.2.
#[doc(hidden)]
pub fn search_with_page_size(
    conn: &Connection,
    query: &str,
    ns: Option<&str>,
    page: usize,
    page_size: usize,
) -> Result<SearchPage> {
    search_with_config(
        conn,
        query,
        ns,
        page,
        &RankingConfig {
            page_size,
            ..Default::default()
        },
    )
}

/// [`search`] with a custom [`RankingConfig`].
pub fn search_with_config(
    conn: &Connection,
    query: &str,
    ns: Option<&str>,
    page: usize,
    cfg: &RankingConfig,
) -> Result<SearchPage> {
    Ok(search_with_trace(conn, query, ns, page, cfg)?.page)
}

/// Runs full-text search with [`RankingConfig`] and returns a [`SearchTrace`] with diagnostic candidate pools.
pub fn search_with_trace(
    conn: &Connection,
    query: &str,
    ns: Option<&str>,
    page: usize,
    cfg: &RankingConfig,
) -> Result<SearchTrace> {
    let page = page.max(1);
    let parts = query_parts(query);
    let expr = expression_of(&parts, " AND ").ok_or(Error::EmptyQuery)?;
    // The observation lines try every word first, then any word (doc 07 §4.2). One word has one form.
    let any_expr = expression_of(&parts, " OR ").filter(|any| *any != expr);
    let ns_key = ns.map(str::to_lowercase);
    let mut candidates = candidates_with_limit(conn, &expr, ns_key.as_deref(), cfg.candidates)?;
    let mut candidate_keys: Vec<String> = candidates.iter().map(|c| c.key.clone()).collect();
    let matches = if candidates.len() == cfg.candidates {
        Some(count_matches(conn, &expr, ns_key.as_deref())?).filter(|n| *n > cfg.candidates)
    } else {
        None
    };
    rank_by_score(conn, &mut candidates, ns_key.as_deref(), cfg)?;

    // Some of the words (doc 07 §4.2, doc 05 §3.7): fewer notes than a page match every word, and the query has
    // more than one part (`any_expr` is `None` for one), so the notes that match any word and are not hits yet fill the
    // page. They are ranked the same way among themselves and stand after every note with all the words.
    let all_count = candidates.len();
    if all_count < cfg.page_size
        && let Some(any) = &any_expr
    {
        let have: HashSet<String> = candidates.iter().map(|c| c.key.clone()).collect();
        let mut fill = candidates_with_limit(conn, any, ns_key.as_deref(), cfg.candidates)?;
        fill.retain(|c| !have.contains(&c.key));
        candidate_keys.extend(fill.iter().map(|c| c.key.clone()));
        rank_by_score(conn, &mut fill, ns_key.as_deref(), cfg)?;
        fill.truncate(cfg.page_size - all_count);
        candidates.extend(fill);
    }
    // The fill only completes page 1: `total` is at most one page whenever there is a fill, so there is never a "more"
    // line or a page 2.
    let fill_start = (page == 1 && candidates.len() > all_count).then_some(all_count);
    let ranked_keys: Vec<String> = candidates.iter().map(|c| c.key.clone()).collect();

    let total = candidates.len();
    let start = ((page - 1) * cfg.page_size).min(total);
    let end = (start + cfg.page_size).min(total);
    let mut hits = Vec::with_capacity(end - start);
    let keys: Vec<&str> = candidates[start..end].iter().map(|c| c.key.as_str()).collect();
    let mut lines = page_observation_lines(conn, &expr, any_expr.as_deref(), &keys)?;
    for c in &candidates[start..end] {
        let observations = lines.remove(&c.key).unwrap_or_default();
        hits.push(Hit {
            id: note_id(&c.path).to_string(),
            title: (c.title_key != c.name_key).then(|| c.title.clone()),
            summary: c.summary.clone(),
            observations,
        });
    }
    let search_page = SearchPage {
        hits,
        page,
        first_number: start + 1,
        more: total - end,
        total,
        page_size: cfg.page_size,
        matches,
        query: query.replace('"', "").trim().to_string(),
        fill_start,
    };
    Ok(SearchTrace {
        page: search_page,
        candidate_keys,
        ranked_keys,
    })
}

/// The `ns` boost, then the order of a page: best score first, ties by key.
fn rank_by_score(
    conn: &Connection,
    candidates: &mut [Candidate],
    ns_key: Option<&str>,
    cfg: &RankingConfig,
) -> Result<()> {
    if let Some(ns_key) = ns_key {
        boost(conn, candidates, ns_key, cfg)?;
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.key.cmp(&b.key)));
    Ok(())
}

/// Turns the agent's text into an FTS5 `MATCH` expression (doc 07 §4, "Query text"): every word in double quotes, a
/// `"` inside a word doubled, text the agent put in double quotes kept as one phrase, an unmatched `"` ignored, the
/// parts joined with AND. No operators. `None` when no word is left.
///
/// A `"` opens a phrase only at the start of a word and only when a closing `"` follows; any other `"` is part of
/// its word. A word with no letter or digit (`++`) has nothing FTS5 could match, so it is left out.
pub fn match_expression(text: &str) -> Option<String> {
    expression_of(&query_parts(text), " AND ")
}

pub(crate) fn expression_of(parts: &[&str], joiner: &str) -> Option<String> {
    let quoted: Vec<String> = parts
        .iter()
        .map(|p| format!("\"{}\"", p.replace('"', "\"\"")))
        .collect();
    (!quoted.is_empty()).then(|| quoted.join(joiner))
}

/// The words and quoted phrases of a query, as [`match_expression`] reads them.
pub(crate) fn query_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        let (part, after) = if let Some(inner) = rest.strip_prefix('"') {
            match inner.find('"') {
                Some(end) => (&inner[..end], &inner[end + 1..]),
                // Unmatched: the quote is ignored and the word after it counts as a word.
                None => {
                    rest = inner.trim_start();
                    continue;
                }
            }
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        };
        if part.chars().any(char::is_alphanumeric) {
            parts.push(part.trim());
        }
        rest = after.trim_start();
    }
    parts
}

/// Every note the query matches, under the same archive condition as [`candidates`] (doc 07 §4.2, "Kept matches").
///
/// The condition is written as `rowid NOT IN (archive notes)`, not as a join: a join looks up the note row of every
/// match and took about 100 ms for a word in all of 10,000 notes, where this form counts straight from the index
/// (about 1 ms). The result is the same.
fn count_matches(conn: &Connection, expr: &str, ns_key: Option<&str>) -> Result<usize> {
    let n: i64 = conn
        .prepare_cached(
            "SELECT COUNT(*) \
             FROM notes_fts \
             WHERE notes_fts MATCH ?1 \
               AND (?2 = 'archive' OR rowid NOT IN (SELECT pk FROM notes WHERE namespace = 'archive'))",
        )?
        .query_row(params![expr, ns_key], |r| r.get(0))?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// Doc 05 §3.7. The result is the query in the doc, written in two steps for speed: the `MATERIALIZED` step with its
/// own `ORDER BY rank` lets FTS5 sort the matches before the join. As one join, SQLite looks up the note row of every
/// match first; on 10,000 notes and a word in all of them that took about 35 ms against 7 ms.
#[allow(dead_code)]
fn candidates(conn: &Connection, expr: &str, ns_key: Option<&str>) -> Result<Vec<Candidate>> {
    candidates_with_limit(conn, expr, ns_key, CANDIDATES)
}

fn candidates_with_limit(conn: &Connection, expr: &str, ns_key: Option<&str>, limit: usize) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare_cached(
        "WITH matches AS MATERIALIZED ( \
             SELECT rowid AS pk, bm25(notes_fts) AS rank FROM notes_fts WHERE notes_fts MATCH ?1 \
             ORDER BY rank \
         ) \
         SELECT n.key, n.path, n.namespace, n.title, n.title_key, n.name_key, n.summary, m.rank \
         FROM matches m \
         JOIN notes n ON n.pk = m.pk \
         WHERE (n.namespace != 'archive' OR ?2 = 'archive') \
         ORDER BY m.rank, n.key \
         LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![expr, ns_key, limit as i64], |r| {
        let rank: f64 = r.get(7)?;
        Ok(Candidate {
            key: r.get(0)?,
            path: r.get(1)?,
            namespace: r.get(2)?,
            title: r.get(3)?,
            title_key: r.get(4)?,
            name_key: r.get(5)?,
            summary: r.get(6)?,
            rank,
            score: -rank,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// The `ns` boost (doc 07 §4.1): notes in `ns`, and notes that a note in `ns` links to, score higher.
fn boost(conn: &Connection, candidates: &mut [Candidate], ns_key: &str, cfg: &RankingConfig) -> Result<()> {
    if (cfg.rel_boost - cfg.inline_boost).abs() < f64::EPSILON && cfg.hub_dampening == HubDampening::None {
        let mut linked_stmt = conn.prepare_cached(
            "SELECT EXISTS ( \
                 SELECT 1 FROM edges e JOIN notes s ON s.key = e.source_key \
                 WHERE e.target_key = ?1 AND s.namespace = ?2)",
        )?;
        let mut linked_keys = HashSet::new();
        for c in candidates.iter().filter(|c| c.namespace != ns_key) {
            if linked_stmt.query_row(params![c.key, ns_key], |r| r.get::<_, bool>(0))? {
                linked_keys.insert(c.key.clone());
            }
        }
        for c in candidates.iter_mut() {
            if c.namespace == ns_key {
                c.score = -c.rank * (1.0 + cfg.ns_boost);
            } else if linked_keys.contains(&c.key) {
                c.score = -c.rank * (1.0 + cfg.rel_boost);
            }
        }
        return Ok(());
    }

    let mut edge_stmt = conn.prepare_cached(
        "SELECT e.is_inline, s.key \
         FROM edges e JOIN notes s ON s.key = e.source_key \
         WHERE e.target_key = ?1 AND s.namespace = ?2",
    )?;
    let mut degree_stmt = conn.prepare_cached("SELECT COUNT(*) FROM edges WHERE source_key = ?1")?;

    for c in candidates.iter_mut() {
        if c.namespace == ns_key {
            c.score = -c.rank * (1.0 + cfg.ns_boost);
        } else {
            let mut total_boost = 0.0;
            let rows = edge_stmt.query_map(params![c.key, ns_key], |r| {
                Ok((r.get::<_, i64>(0)? == 1, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (is_inline, source_key) = row?;
                let base_weight = if is_inline { cfg.inline_boost } else { cfg.rel_boost };
                let dampening = match cfg.hub_dampening {
                    HubDampening::None => 1.0,
                    HubDampening::Log => {
                        let out_degree: i64 = degree_stmt.query_row(params![source_key], |r| r.get(0))?;
                        1.0 + (1.0 + out_degree as f64).ln()
                    }
                    HubDampening::Sqrt => {
                        let out_degree: i64 = degree_stmt.query_row(params![source_key], |r| r.get(0))?;
                        (out_degree as f64).sqrt().max(1.0)
                    }
                };
                total_boost += base_weight / dampening;
            }
            if total_boost > 0.0 {
                let clamped_boost = total_boost.min(cfg.max_linked_boost);
                c.score = -c.rank * (1.0 + clamped_boost);
            }
        }
    }
    Ok(())
}

/// One stored observation of a hit, before it is ranked.
struct StoredObservation {
    pk: i64,
    note_key: String,
    line: ObservationLine,
    line_number: i64,
}

/// The observation lines of every hit on a page (doc 07 §4.2, "Below"): up to [`MAX_OBSERVATIONS`] per note, best
/// match first, ties by line number. The lines that hold every word are used; only a note with none of them gets the
/// lines that hold any word.
///
/// One statement ranks the observations of all the page's notes, not one per hit. BM25 needs the term statistics of
/// the whole index, and FTS5 gathers them again for every statement: ten statements per page took about 77 ms for a
/// word in all of 10,000 notes, where one takes about 3 ms.
fn page_observation_lines(
    conn: &Connection,
    all: &str,
    any: Option<&str>,
    note_keys: &[&str],
) -> Result<HashMap<String, Vec<ObservationLine>>> {
    let stored = stored_observations(conn, note_keys)?;
    let mut lines: HashMap<String, Vec<ObservationLine>> = HashMap::new();
    if stored.is_empty() {
        return Ok(lines);
    }
    let ranked = rank_observations(conn, all, &stored)?;
    let mut without: Vec<&StoredObservation> = Vec::new();
    let with_all: HashSet<&str> = ranked.iter().map(|(o, _)| o.note_key.as_str()).collect();
    collect_best(ranked, &mut lines);
    if let Some(any) = any {
        for o in &stored {
            if !with_all.contains(o.note_key.as_str()) {
                without.push(o);
            }
        }
        if !without.is_empty() {
            let ranked = rank_observations_of(conn, any, &without)?;
            collect_best(ranked, &mut lines);
        }
    }
    Ok(lines)
}

/// Every observation of the given notes.
fn stored_observations(conn: &Connection, note_keys: &[&str]) -> Result<Vec<StoredObservation>> {
    if note_keys.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; note_keys.len()].join(",");
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT pk, note_key, category, qualifiers, content, line_number \
         FROM observations WHERE note_key IN ({marks})"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(note_keys), |r| {
        Ok(StoredObservation {
            pk: r.get(0)?,
            note_key: r.get(1)?,
            line: ObservationLine {
                category: r.get(2)?,
                qualifier: r.get(3)?,
                content: r.get(4)?,
            },
            line_number: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn rank_observations<'a>(
    conn: &Connection,
    expr: &str,
    stored: &'a [StoredObservation],
) -> Result<Vec<(&'a StoredObservation, f64)>> {
    let refs: Vec<&StoredObservation> = stored.iter().collect();
    rank_observations_of(conn, expr, &refs)
}

/// The statement that scores the page's observations (`?1` is the `MATCH` expression, `?2` a JSON array of pks).
/// Its speed depends on the query plan; a test checks the plan.
#[doc(hidden)]
pub const OBSERVATION_RANK_SQL: &str = "SELECT o.pk, bm25(observations_fts) \
     FROM observations o \
     JOIN observations_fts ON observations_fts.rowid = o.pk \
     WHERE observations_fts MATCH ?1 \
       AND o.pk IN (SELECT value FROM json_each(?2))";

/// The BM25 score of each of `observations` that matches `expr`.
///
/// The statement starts from `observations` and joins `observations_fts`, with the pks in the `IN` list: SQLite then
/// scores only the matches in the list. With the pk test on `observations_fts.rowid`, or with the join the other way
/// round, it scored every match in the index first and took 16 to 46 ms instead of 3.
fn rank_observations_of<'a>(
    conn: &Connection,
    expr: &str,
    observations: &[&'a StoredObservation],
) -> Result<Vec<(&'a StoredObservation, f64)>> {
    let pk_list = format!(
        "[{}]",
        observations
            .iter()
            .map(|o| o.pk.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let by_pk: HashMap<i64, &'a StoredObservation> = observations.iter().map(|o| (o.pk, *o)).collect();
    let mut stmt = conn.prepare_cached(OBSERVATION_RANK_SQL)?;
    let rows = stmt.query_map(params![expr, pk_list], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    let mut ranked = Vec::new();
    for row in rows {
        let (pk, rank) = row?;
        if let Some(o) = by_pk.get(&pk) {
            ranked.push((*o, rank));
        }
    }
    Ok(ranked)
}

/// Best [`MAX_OBSERVATIONS`] per note: lower BM25 first, then the lower line number.
fn collect_best(mut ranked: Vec<(&StoredObservation, f64)>, lines: &mut HashMap<String, Vec<ObservationLine>>) {
    ranked.sort_by(|(a, ra), (b, rb)| {
        a.note_key
            .cmp(&b.note_key)
            .then_with(|| ra.total_cmp(rb))
            .then_with(|| a.line_number.cmp(&b.line_number))
    });
    for (o, _) in ranked {
        let note_lines = lines.entry(o.note_key.clone()).or_default();
        if note_lines.len() < MAX_OBSERVATIONS {
            note_lines.push(o.line.clone());
        }
    }
}

/// The observation lines of one note, one statement per note: the first version of the rule, kept so a test can check
/// that the batched version gives the same lines.
#[doc(hidden)]
pub fn reference_observation_lines(conn: &Connection, query: &str, note_key: &str) -> Result<Vec<ObservationLine>> {
    let parts = query_parts(query);
    let all = expression_of(&parts, " AND ").ok_or(Error::EmptyQuery)?;
    let any = expression_of(&parts, " OR ").filter(|any| *any != all);
    let (lo, hi): (Option<i64>, Option<i64>) = conn
        .prepare_cached("SELECT MIN(pk), MAX(pk) FROM observations WHERE note_key = ?1")?
        .query_row([note_key], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Ok(Vec::new());
    };
    let lines = observations_matching(conn, &all, lo, hi, note_key)?;
    match any {
        Some(any) if lines.is_empty() => observations_matching(conn, &any, lo, hi, note_key),
        _ => Ok(lines),
    }
}

fn observations_matching(
    conn: &Connection,
    expr: &str,
    lo: i64,
    hi: i64,
    note_key: &str,
) -> Result<Vec<ObservationLine>> {
    let mut stmt = conn.prepare_cached(
        "SELECT o.category, o.qualifiers, o.content \
         FROM observations_fts \
         JOIN observations o ON o.pk = observations_fts.rowid \
         WHERE observations_fts MATCH ?1 \
           AND observations_fts.rowid BETWEEN ?2 AND ?3 \
           AND o.note_key = ?4 \
         ORDER BY bm25(observations_fts), o.line_number \
         LIMIT ?5",
    )?;
    let rows = stmt.query_map(params![expr, lo, hi, note_key, MAX_OBSERVATIONS as i64], |r| {
        Ok(ObservationLine {
            category: r.get(0)?,
            qualifier: r.get(1)?,
            content: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Tokens of a text, estimated as characters ÷ 4 (rounded up).
fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

/// One fact line as it reads without indent: `- [category] @qualifier text`, cut to [`LINE_MAX_CHARS`] (doc 07 §4.2).
///
/// A line that is too long is cut at a word boundary and ends with `…`, the `…` included in the limit. A `.`, `,`,
/// `;` or `:` before the `…` is dropped. A word that does not end within the limit is cut where the limit falls, so
/// the line is never only a header.
fn fact_line(o: &ObservationLine) -> String {
    let header = match &o.qualifier {
        Some(q) => format!("- [{}] {q}", o.category),
        None => format!("- [{}]", o.category),
    };
    let line = format!("{header} {}", o.content);
    if line.chars().count() <= LINE_MAX_CHARS {
        return line;
    }
    let chars: Vec<char> = line.chars().collect();
    let room = LINE_MAX_CHARS - 1;
    let header_len = header.chars().count();
    // The cut may fall right after a word (the next character is a space) or inside one: step back to the space.
    let cut = if chars[room].is_whitespace() {
        room
    } else {
        (header_len + 1..room)
            .rev()
            .find(|i| chars[*i].is_whitespace())
            .unwrap_or(room)
    };
    let kept: String = chars[..cut].iter().collect();
    // A cut after a clause mark reads better without it: `evidence…`, not `evidence.…`.
    let kept = kept.trim_end().trim_end_matches(['.', ',', ';', ':']).trim_end();
    format!("{kept}…")
}

/// The text of doc 07 §4.2: one block per hit, then how many more hits exist. A last page has no "more" line. A query
/// with no hits at all gets the "No hits" line, on every page number. A page past the end of a query that has hits
/// gets the one "past the end" line.
///
/// The page is built in two passes (doc 07 §4.2, "Budget"): every hit keeps its id and summary, and the closing
/// lines stay. Fact lines are then added in rank order, the best hit's first, while the page stays within
/// [`PAGE_TOKEN_BUDGET`]; once one does not fit, no more are added.
pub fn render(page: &SearchPage) -> String {
    if page.total == 0 {
        return format!("No notes match \"{}\".\n", page.query);
    }
    let kept = page.matches.map(|all| {
        format!("Only the best {CANDIDATES} of {all} matches are kept; narrow the query to reach the rest.")
    });
    if page.hits.is_empty() {
        // A page past the end of a query that has hits (doc 07 §4.2, "Paging"): one line, a normal result.
        let pages = page.total.div_ceil(page.page_size.max(1));
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let mut line = format!(
            "Page {} is past the end: {} on {}.",
            page.page,
            plural(page.total, "result"),
            plural(pages, "page")
        );
        if let Some(kept) = kept {
            line.push(' ');
            line.push_str(&kept);
        }
        line.push('\n');
        return line;
    }
    let heads: Vec<String> = page
        .hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let mut head = format!(
                "{}. {}\n",
                page.first_number + i,
                titled_id(&hit.id, hit.title.as_deref())
            );
            if let Some(summary) = &hit.summary {
                head.push_str(&format!("   {summary}\n"));
            }
            head
        })
        .collect();
    let closing = match (page.more > 0, kept) {
        (true, Some(kept)) => format!("{} more: page {}. {kept}\n", page.more, page.page + 1),
        (true, None) => format!("{} more: page {}\n", page.more, page.page + 1),
        (false, Some(kept)) => format!("{kept}\n"),
        (false, None) => String::new(),
    };

    let separator = page.fill_start.map(|_| {
        format!(
            "{FILL_SEPARATOR}
"
        )
    });
    let mut used = heads.iter().map(|h| h.chars().count()).sum::<usize>()
        + closing.chars().count()
        + separator.as_ref().map_or(0, |s| s.chars().count());
    let mut full = false;
    let mut out = String::new();
    for (i, (head, hit)) in heads.iter().zip(&page.hits).enumerate() {
        if let Some(separator) = separator.as_ref().filter(|_| page.fill_start == Some(i)) {
            out.push_str(separator);
        }
        out.push_str(head);
        for o in &hit.observations {
            if full {
                break;
            }
            let line = format!("   {}\n", fact_line(o));
            let len = line.chars().count();
            if estimate_tokens(used + len) > PAGE_TOKEN_BUDGET {
                full = true;
                break;
            }
            used += len;
            out.push_str(&line);
        }
    }
    out.push_str(&closing);
    out
}

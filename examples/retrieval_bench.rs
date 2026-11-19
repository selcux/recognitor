//! Retrieval benchmark for `search` (doc 02 §11 and §6, doc 07 §4 and §13).
//!
//! usage: cargo run --release --example retrieval_bench -- <vault> [--questions <path>] [--db <path>]
//!                                                        [--ns-boost <f64>] [--rel-boost <f64>]
//!                                                        [--inline-boost <f64>] [--hub-dampening <none|log|sqrt>]
//!                                                        [--show] [--json]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use recognitor::bootstrap;
use recognitor::search::{self, HubDampening, RankingConfig};
use recognitor::{Index, SweepOptions};
use serde_json::{Value, json};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone)]
struct Question {
    id: String,
    query: String,
    namespace: Option<String>,
    expected_notes: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MissBucket {
    FtsZero,
    CandidateCap,
    Rerank,
    TokenBudget,
}

impl MissBucket {
    fn as_str(&self) -> &'static str {
        match self {
            Self::FtsZero => "MISS_FTS_ZERO",
            Self::CandidateCap => "MISS_CANDIDATE_CAP",
            Self::Rerank => "MISS_RERANK",
            Self::TokenBudget => "MISS_TOKEN_BUDGET",
        }
    }
}

#[derive(Debug)]
struct Args {
    vault: PathBuf,
    db: PathBuf,
    questions_path: PathBuf,
    ranking_cfg: RankingConfig,
    show: bool,
    json: bool,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut custom_questions = false;
    let mut questions_path = PathBuf::from("benches/retrieval/questions.json");
    let mut db = None;
    let mut show = false;
    let mut json = false;
    let mut ranking_cfg = RankingConfig::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--questions" => {
                questions_path = PathBuf::from(args.next().ok_or("--questions needs a path")?);
                custom_questions = true;
            }
            "--vault" => {
                positional.push(args.next().ok_or("--vault needs a path")?);
            }
            "--db" => {
                db = Some(PathBuf::from(args.next().ok_or("--db needs a path")?));
            }
            "--ns-boost" => {
                ranking_cfg.ns_boost = args.next().ok_or("--ns-boost needs a number")?.parse()?;
            }
            "--rel-boost" => {
                ranking_cfg.rel_boost = args.next().ok_or("--rel-boost needs a number")?.parse()?;
            }
            "--inline-boost" => {
                ranking_cfg.inline_boost = args.next().ok_or("--inline-boost needs a number")?.parse()?;
            }
            "--hub-dampening" => {
                let s = args.next().ok_or("--hub-dampening needs none, log, or sqrt")?;
                ranking_cfg.hub_dampening = match s.to_lowercase().as_str() {
                    "none" => HubDampening::None,
                    "log" => HubDampening::Log,
                    "sqrt" => HubDampening::Sqrt,
                    _ => return Err(format!("unknown hub dampening mode {s:?}").into()),
                };
            }
            "--show" => show = true,
            "--json" => json = true,
            _ => {
                if !arg.starts_with('-') {
                    positional.push(arg);
                } else {
                    return Err(format!("unexpected argument: {arg}").into());
                }
            }
        }
    }

    if positional.is_empty() {
        return Err("usage: retrieval_bench <vault> [--questions <path>] [--db <path>] [--ns-boost N] [--rel-boost N] [--inline-boost N] [--hub-dampening none|log|sqrt] [--show] [--json]".into());
    }

    let vault = PathBuf::from(&positional[0]);
    let db = db
        .unwrap_or_else(|| std::env::temp_dir().join(format!("recognitor-retrieval-bench-{}.db", std::process::id())));

    if !custom_questions && !questions_path.exists() {
        let example = PathBuf::from("benches/retrieval/questions.example.json");
        if example.exists() {
            questions_path = example;
        }
    }

    Ok(Args {
        vault,
        db,
        questions_path,
        ranking_cfg,
        show,
        json,
    })
}

fn load_questions(path: &PathBuf) -> Res<Vec<Question>> {
    let raw = fs::read_to_string(path).map_err(|e| format!("cannot read questions file at {:?}: {e}", path))?;
    let val: Value =
        serde_json::from_str(&raw).map_err(|e| format!("cannot parse questions JSON at {:?}: {e}", path))?;
    let list = val
        .get("questions")
        .and_then(Value::as_array)
        .ok_or("expected 'questions' array in JSON")?;
    let mut questions = Vec::new();
    for q in list {
        let id = q.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let query = q.get("query").and_then(Value::as_str).unwrap_or("").to_string();
        let namespace = q.get("namespace").and_then(Value::as_str).map(String::from);
        let expected_notes = q
            .get("expected_notes")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();
        let tags = q
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default();
        questions.push(Question {
            id,
            query,
            namespace,
            expected_notes,
            tags,
        });
    }
    Ok(questions)
}

fn normalize_key(s: &str) -> String {
    let trimmed = s.trim().replace('\\', "/");
    let without_ext = trimmed.strip_suffix(".md").unwrap_or(&trimmed);
    without_ext.trim_start_matches('/').to_lowercase()
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[derive(Debug, Default)]
struct TagScore {
    count: usize,
    sum_recall_5: f64,
    sum_recall_50: f64,
    sum_mrr_5: f64,
    hits_5: usize,
}

fn main() -> Res<()> {
    let args = parse_args()?;
    let questions = load_questions(&args.questions_path)?;

    if questions.is_empty() {
        return Err("question dataset is empty".into());
    }

    let mut index = Index::open(&args.vault, Some(&args.db))?;
    let sync_rep = index.sync(SweepOptions::default())?;

    // Measure bootstrap payload size (required by doc 02 §11).
    let (boot_bytes, boot_tokens) = match bootstrap::load(index.connection()) {
        Ok(boot) => {
            let text = boot.render(bootstrap::DEFAULT_MAX_TOKENS);
            (text.len(), boot.tokens())
        }
        Err(e) => {
            if !args.json {
                eprintln!("note: bootstrap payload skipped ({e})");
            }
            (0, 0)
        }
    };

    let mut total_recall_5 = 0.0;
    let mut total_recall_50 = 0.0;
    let mut total_mrr_5 = 0.0;
    let mut total_hits_5 = 0;
    let mut total_page_tokens = 0;

    let mut tag_scores: HashMap<String, TagScore> = HashMap::new();
    let mut miss_counts: HashMap<MissBucket, usize> = HashMap::new();

    if !args.json {
        println!(
            "Indexed vault {:?}: {} notes, {} observations, {} edges (read {} files)",
            args.vault, sync_rep.totals.notes, sync_rep.totals.observations, sync_rep.totals.edges, sync_rep.files_read
        );
        println!("Bootstrap payload: {} bytes, ~{} tokens", boot_bytes, boot_tokens);
        println!(
            "Evaluating {} questions with config: ns_boost={:.2}, rel_boost={:.2}, inline_boost={:.2}, dampening={:?}\n",
            questions.len(),
            args.ranking_cfg.ns_boost,
            args.ranking_cfg.rel_boost,
            args.ranking_cfg.inline_boost,
            args.ranking_cfg.hub_dampening,
        );
    }

    for q in &questions {
        let expected_keys: Vec<String> = q.expected_notes.iter().map(|s| normalize_key(s)).collect();
        let expected_set: HashSet<String> = expected_keys.iter().cloned().collect();

        let trace = search::search_with_trace(
            index.connection(),
            &q.query,
            q.namespace.as_deref(),
            1,
            &args.ranking_cfg,
        )?;

        let page_text = search::render(&trace.page);
        let page_tokens = estimate_tokens(&page_text);
        total_page_tokens += page_tokens;

        let hit_keys: Vec<String> = trace.page.hits.iter().map(|h| normalize_key(&h.id)).collect();

        let candidate_keys: Vec<String> = trace.candidate_keys.iter().map(|k| normalize_key(k)).collect();
        let candidate_set: HashSet<String> = candidate_keys.iter().cloned().collect();

        let ranked_keys: Vec<String> = trace.ranked_keys.iter().map(|k| normalize_key(k)).collect();

        let mut found_in_5 = 0;
        let mut first_rank = None;
        for (i, k) in hit_keys.iter().enumerate() {
            if expected_set.contains(k) {
                found_in_5 += 1;
                if first_rank.is_none() {
                    first_rank = Some(i + 1);
                }
            }
        }

        let mut found_in_50 = 0;
        for k in &expected_keys {
            if candidate_set.contains(k) {
                found_in_50 += 1;
            }
        }

        let denom = expected_keys.len().max(1) as f64;
        let q_recall_5 = found_in_5 as f64 / denom;
        let q_recall_50 = found_in_50 as f64 / denom;
        let q_mrr = first_rank.map(|r| 1.0 / r as f64).unwrap_or(0.0);
        let q_hit = if found_in_5 > 0 { 1 } else { 0 };

        total_recall_5 += q_recall_5;
        total_recall_50 += q_recall_50;
        total_mrr_5 += q_mrr;
        total_hits_5 += q_hit;

        for tag in &q.tags {
            let entry = tag_scores.entry(tag.clone()).or_default();
            entry.count += 1;
            entry.sum_recall_5 += q_recall_5;
            entry.sum_recall_50 += q_recall_50;
            entry.sum_mrr_5 += q_mrr;
            entry.hits_5 += q_hit;
        }

        // Error classification for missed expected notes.
        for expected in &expected_keys {
            if !hit_keys.contains(expected) {
                let bucket = if !candidate_set.contains(expected) {
                    let any_match = trace.candidate_keys.iter().any(|k| normalize_key(k) == *expected);
                    if any_match {
                        MissBucket::CandidateCap
                    } else {
                        MissBucket::FtsZero
                    }
                } else {
                    let rank_pos = ranked_keys.iter().position(|k| k == expected);
                    match rank_pos {
                        Some(pos) if pos < args.ranking_cfg.page_size => MissBucket::TokenBudget,
                        _ => MissBucket::Rerank,
                    }
                };
                *miss_counts.entry(bucket).or_default() += 1;
            }
        }

        if args.show {
            println!(
                "[{}] query={:?} ns={:?} recall@5={:.2} recall@50={:.2} mrr={:.2}",
                q.id, q.query, q.namespace, q_recall_5, q_recall_50, q_mrr
            );
            println!("  expected: {:?}", q.expected_notes);
            println!("  hits ({}): {:?}", hit_keys.len(), hit_keys);
            if found_in_5 < expected_keys.len() {
                for exp in &expected_keys {
                    if !hit_keys.contains(exp) {
                        let in_50 = candidate_set.contains(exp);
                        let rank = ranked_keys.iter().position(|k| k == exp).map(|p| p + 1);
                        println!("    miss: {exp} (in_candidates={in_50}, ranked={rank:?})");
                    }
                }
            }
        }
    }

    let n = questions.len() as f64;
    let macro_recall_5 = total_recall_5 / n;
    let macro_recall_50 = total_recall_50 / n;
    let macro_mrr_5 = total_mrr_5 / n;
    let hit_at_5 = total_hits_5 as f64 / n;
    let avg_page_tokens = total_page_tokens as f64 / n;

    let mut by_tag_json = json!({});
    for (tag, score) in &tag_scores {
        let cnt = score.count.max(1) as f64;
        by_tag_json[tag] = json!({
            "count": score.count,
            "recall_5": score.sum_recall_5 / cnt,
            "recall_50": score.sum_recall_50 / cnt,
            "mrr_5": score.sum_mrr_5 / cnt,
            "hit_rate_5": score.hits_5 as f64 / cnt,
        });
    }

    let mut miss_buckets_json = json!({});
    for (b, count) in &miss_counts {
        miss_buckets_json[b.as_str()] = json!(count);
    }

    let report_json = json!({
        "total_questions": questions.len(),
        "macro_recall_5": macro_recall_5,
        "macro_recall_50": macro_recall_50,
        "macro_mrr_5": macro_mrr_5,
        "hit_at_5": hit_at_5,
        "bootstrap_bytes": boot_bytes,
        "bootstrap_tokens": boot_tokens,
        "avg_page_tokens": avg_page_tokens,
        "by_tag": by_tag_json,
        "miss_buckets": miss_buckets_json,
    });

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report_json)?);
        return Ok(());
    }

    println!("============================================================");
    println!("RETRIEVAL BENCHMARK REPORT (doc 02 §11)");
    println!("============================================================");
    println!("Overall Recall@5:   {:>6.2}%", macro_recall_5 * 100.0);
    println!("Overall Recall@50:  {:>6.2}%", macro_recall_50 * 100.0);
    println!("Overall Hit@5:      {:>6.2}%", hit_at_5 * 100.0);
    println!("Overall MRR@5:      {:>6.4}", macro_mrr_5);
    println!("Avg Page Tokens:    {:>6.1}", avg_page_tokens);
    println!(
        "Bootstrap Payload:  {} bytes (~{} tokens)",
        report_json["bootstrap_bytes"], report_json["bootstrap_tokens"]
    );
    println!("------------------------------------------------------------");
    println!("BREAKDOWN BY MECHANISM TAG:");
    println!(
        "{:<24} {:>5} {:>10} {:>11} {:>8} {:>8}",
        "Tag", "Count", "Recall@5", "Recall@50", "Hit@5", "MRR@5"
    );
    println!("{:-<70}", "");
    let mut tags: Vec<_> = tag_scores.keys().cloned().collect();
    tags.sort();
    for tag in tags {
        let s = &tag_scores[&tag];
        let cnt = s.count.max(1) as f64;
        println!(
            "{:<24} {:>5} {:>9.1}% {:>10.1}% {:>7.1}% {:>8.3}",
            tag,
            s.count,
            (s.sum_recall_5 / cnt) * 100.0,
            (s.sum_recall_50 / cnt) * 100.0,
            (s.hits_5 as f64 / cnt) * 100.0,
            s.sum_mrr_5 / cnt
        );
    }
    println!("------------------------------------------------------------");
    let total_misses: usize = miss_counts.values().sum();
    println!("DIAGNOSTIC MISS BUCKETS (Total misses: {}):", total_misses);
    for (b, count) in &miss_counts {
        println!("  {:<22}: {:>3}", b.as_str(), count);
    }
    println!("============================================================");

    let _ = fs::remove_file(&args.db);
    Ok(())
}

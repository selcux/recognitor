//! Layer 2A: Deterministic scripted comparison between Recognitor MCP and Basic Memory MCP (doc 02 §11, doc 07 §13).
//!
//! Measures apples-to-apples recall@5, payload tokens, and round-trip latency across the 25 golden questions
//! to decide the `recall(query, max_tokens)` architectural gate.
//!
//! usage: cargo build --release --example basic_memory_bench
//!        target/release/examples/basic_memory_bench <vault> <scratch-db> [--questions <path>] [--basic-memory <cmd>]

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProcess {
    fn spawn(cmd: &str, args: &[&str], envs: &[(&str, &str)]) -> Res<Self> {
        let mut child = Command::new(cmd)
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut s = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        s.initialize()?;
        Ok(s)
    }

    fn request(&mut self, method: &str, params: Value) -> Res<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        writeln!(stdin, "{message}")?;
        stdin.flush()?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err("the server closed stdout".into());
        }
        let response: Value = serde_json::from_str(&line)?;
        Ok(response)
    }

    fn initialize(&mut self) -> Res<Value> {
        let response = self.request(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "bm_bench", "version": "1"}}),
        )?;
        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )?;
        stdin.flush()?;
        Ok(response)
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Res<(String, bool)> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        let result = response
            .get("result")
            .ok_or_else(|| format!("protocol error: {response}"))?;
        let content = result["content"].as_array().and_then(|a| a.first());
        let text = match content {
            Some(c) => c["text"].as_str().unwrap_or("").to_string(),
            None => "".to_string(),
        };
        Ok((text, result["isError"].as_bool().unwrap_or(false)))
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[allow(dead_code)]
struct Question {
    id: String,
    query: String,
    namespace: Option<String>,
    expected_notes: Vec<String>,
    tags: Vec<String>,
}

fn load_questions(path: &Path) -> Res<Vec<Question>> {
    let text = fs::read_to_string(path)?;
    let val: Value = serde_json::from_str(&text)?;
    let arr = val["questions"].as_array().ok_or("no questions array")?;
    let mut out = Vec::new();
    for q in arr {
        let id = q["id"].as_str().ok_or("no id")?.to_string();
        let query = q["query"].as_str().ok_or("no query")?.to_string();
        let namespace = q["namespace"].as_str().map(|s| s.to_string());
        let expected_notes = q["expected_notes"]
            .as_array()
            .ok_or("no expected_notes")?
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        let tags = q["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        out.push(Question {
            id,
            query,
            namespace,
            expected_notes,
            tags,
        });
    }
    Ok(out)
}

/// Maps canonical vault path (e.g. "projects/recognitor/Decisions", "common/Identity") to Basic Memory project & note.
fn to_bm_target(path: &str) -> (String, String) {
    if let Some(rest) = path.strip_prefix("projects/") {
        if let Some((proj, id)) = rest.split_once('/') {
            (proj.to_string(), id.to_string())
        } else {
            (rest.to_string(), "index".to_string())
        }
    } else if let Some((proj, id)) = path.split_once('/') {
        (proj.to_string(), id.to_string())
    } else {
        ("common".to_string(), path.to_string())
    }
}

/// Normalizes note paths for comparisons (strips .md, converts backslashes, lowercases).
fn norm(s: &str) -> String {
    let s = s.replace('\\', "/");
    let bare = s.strip_suffix(".md").unwrap_or(&s);
    bare.trim().to_lowercase()
}

fn parse_recognitor_hits(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        for prefix in ["1. ", "2. ", "3. ", "4. ", "5. "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let id_part = rest.split_once(',').map(|(id, _)| id).unwrap_or(rest).trim();
                hits.push(id_part.to_string());
                break;
            }
        }
    }
    hits
}

fn parse_bm_hits(text: &str, project: &str) -> Vec<String> {
    let mut hits = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- ") {
            if let Some((file, _)) = rest.split_once('|') {
                let note_id = file.trim().strip_suffix(".md").unwrap_or(file.trim());
                let full = format!("{project}/{note_id}");
                hits.push(full);
            }
        }
    }
    hits
}

fn default_recognitor_binary() -> Res<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot find the target folder")?;
    Ok(dir.join(if cfg!(windows) { "recognitor.exe" } else { "recognitor" }))
}

fn main() -> Res<()> {
    let mut positional = Vec::new();
    let mut questions_path = PathBuf::from("benches/retrieval/questions.json");
    if !questions_path.exists() {
        questions_path = PathBuf::from("benches/retrieval/questions.example.json");
    }
    let mut bm_cmd = "basic-memory".to_string();
    let mut rec_bin = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--questions" => questions_path = PathBuf::from(args.next().ok_or("--questions needs a path")?),
            "--basic-memory" => bm_cmd = args.next().ok_or("--basic-memory needs a binary name")?,
            "--recognitor-bin" => rec_bin = Some(PathBuf::from(args.next().ok_or("--recognitor-bin needs a path")?)),
            _ => positional.push(PathBuf::from(arg)),
        }
    }

    let [vault, db] = <[PathBuf; 2]>::try_from(positional)
        .map_err(|_| "usage: basic_memory_bench <vault> <scratch-db> [--questions path]")?;

    let rec_bin = match rec_bin {
        Some(b) => b,
        None => default_recognitor_binary()?,
    };

    println!("=== Tier 2 Basic Memory Token-Spend Benchmark (doc 02 §11, doc 07 §13) ===");
    println!("Vault:           {}", vault.display());
    println!("Recognitor DB:   {}", db.display());
    println!("Questions:       {}", questions_path.display());
    println!("Basic Memory:    {}", bm_cmd);
    println!("Recognitor bin:  {}", rec_bin.display());

    let questions = load_questions(&questions_path)?;
    println!("Loaded {} questions for comparison.\n", questions.len());

    // 0. Pre-sync Recognitor DB so index is populated
    println!("Syncing Recognitor index...");
    {
        let mut init_idx = recognitor::Index::open(&vault, Some(&db))?;
        init_idx.sync(recognitor::SweepOptions::default())?;
    }

    // 1. Spawn servers
    println!("Starting Recognitor MCP server...");
    let rec_db_str = db.to_str().ok_or("invalid db path")?;
    let rec_cfg_str = db.with_extension("config.toml");
    let rec_cfg_str = rec_cfg_str.to_str().ok_or("invalid cfg path")?;
    let vault_str = vault.to_str().ok_or("invalid vault path")?;

    let mut rec_server = McpProcess::spawn(
        rec_bin.to_str().ok_or("invalid rec_bin")?,
        &["mcp", "--vault", vault_str],
        &[("RECOGNITOR_DB", rec_db_str), ("RECOGNITOR_CONFIG", rec_cfg_str)],
    )?;

    println!("Starting Basic Memory MCP server...");
    let mut bm_server = McpProcess::spawn(&bm_cmd, &["mcp", "--transport", "stdio"], &[])?;

    // 2. Measure Startup Cost
    println!("\n--- Step 1: Session Startup Payload ---");
    let t0 = Instant::now();
    let (rec_boot_text, rec_boot_err) = rec_server.call("load_bootstrap", &json!({}))?;
    let rec_boot_ms = ms(t0.elapsed());
    let rec_boot_tok = tokens(&rec_boot_text);
    println!(
        "Recognitor load_bootstrap: {:>7.2} ms, {:>5} tokens (isError={})",
        rec_boot_ms, rec_boot_tok, rec_boot_err
    );

    let t0 = Instant::now();
    let (bm_init_text, bm_init_err) =
        bm_server.call("bm_read", &json!({"project": "common", "identifier": "index"}))?;
    let bm_init_ms = ms(t0.elapsed());
    let bm_init_tok = tokens(&bm_init_text);
    println!(
        "Basic Memory index read:  {:>7.2} ms, {:>5} tokens (isError={})",
        bm_init_ms, bm_init_tok, bm_init_err
    );

    // 3. Question Evaluation Loop
    println!("\n--- Step 2: Per-Question Retrieval & Hydration Comparison ---");
    println!(
        "{:<4} {:<30} | {:^7} {:^7} | {:^9} {:^9} | {:^9} {:^9} | {:^10} {:^10}",
        "ID", "Query", "Rec R@5", "BM R@5", "Rec Srch", "BM Srch", "Rec Hyd", "BM Hyd", "Rec Total", "BM Total"
    );
    println!("{}", "-".repeat(118));

    let mut rec_hits_total = 0usize;
    let mut bm_hits_total = 0usize;
    let mut rec_search_tokens_total = 0usize;
    let mut bm_search_tokens_total = 0usize;
    let mut rec_hyd_tokens_total = 0usize;
    let mut bm_hyd_tokens_total = 0usize;
    let mut rec_latency_total_ms = 0.0f64;
    let mut bm_latency_total_ms = 0.0f64;

    for q in &questions {
        // --- Recognitor execution ---
        let mut rec_args = json!({"query": q.query});
        if let Some(ref ns) = q.namespace {
            rec_args["ns"] = json!(ns);
        }
        let t_rec_start = Instant::now();
        let (rec_srch_text, _) = rec_server.call("search", &rec_args)?;
        let _rec_srch_ms = ms(t_rec_start.elapsed());
        let rec_srch_tok = tokens(&rec_srch_text);
        let rec_hits = parse_recognitor_hits(&rec_srch_text);

        // Check Recognitor Recall@5
        let rec_top5: Vec<String> = rec_hits.iter().take(5).map(|s| norm(s)).collect();
        let mut rec_all_found = true;
        for exp in &q.expected_notes {
            let exp_n = norm(exp);
            let found = rec_top5
                .iter()
                .any(|h| h == &exp_n || h.ends_with(&exp_n) || exp_n.ends_with(h));
            if !found {
                rec_all_found = false;
                break;
            }
        }
        if rec_all_found {
            rec_hits_total += 1;
        }

        // Recognitor Hydration: read expected notes
        let mut rec_hyd_tok = 0usize;
        for exp in &q.expected_notes {
            let (read_text, _) = rec_server.call("read_note", &json!({"note": exp}))?;
            rec_hyd_tok += tokens(&read_text);
        }
        let rec_total_ms = ms(t_rec_start.elapsed());
        let rec_total_tok = rec_srch_tok + rec_hyd_tok;

        // --- Basic Memory execution ---
        let (bm_proj, _) = to_bm_target(&q.expected_notes[0]);
        let bm_search_args = json!({
            "query": q.query,
            "project": bm_proj,
        });
        let t_bm_start = Instant::now();
        let (bm_srch_text, _) = bm_server.call("bm_search", &bm_search_args)?;
        let bm_srch_tok = tokens(&bm_srch_text);
        let bm_hits = parse_bm_hits(&bm_srch_text, &bm_proj);

        // Check Basic Memory Recall@5
        let bm_top5: Vec<String> = bm_hits.iter().take(5).map(|s| norm(s)).collect();
        let mut bm_all_found = true;
        for exp in &q.expected_notes {
            let exp_n = norm(exp);
            let found = bm_top5
                .iter()
                .any(|h| h == &exp_n || h.ends_with(&exp_n) || exp_n.ends_with(h));
            if !found {
                bm_all_found = false;
                break;
            }
        }
        if bm_all_found {
            bm_hits_total += 1;
        }

        // Basic Memory Hydration: read expected notes
        let mut bm_hyd_tok = 0usize;
        for exp in &q.expected_notes {
            let (p, id) = to_bm_target(exp);
            let (read_text, _) = bm_server.call("bm_read", &json!({"project": p, "identifier": id}))?;
            bm_hyd_tok += tokens(&read_text);
        }
        let bm_total_ms = ms(t_bm_start.elapsed());
        let bm_total_tok = bm_srch_tok + bm_hyd_tok;

        rec_search_tokens_total += rec_srch_tok;
        bm_search_tokens_total += bm_srch_tok;
        rec_hyd_tokens_total += rec_hyd_tok;
        bm_hyd_tokens_total += bm_hyd_tok;
        rec_latency_total_ms += rec_total_ms;
        bm_latency_total_ms += bm_total_ms;

        let query_trunc: String = q.query.chars().take(28).collect();
        println!(
            "{:<4} {:<30} | {:^7} {:^7} | {:>8}t {:>8}t | {:>8}t {:>8}t | {:>9}t {:>9}t",
            q.id,
            query_trunc,
            if rec_all_found { "100%" } else { "0%" },
            if bm_all_found { "100%" } else { "0%" },
            rec_srch_tok,
            bm_srch_tok,
            rec_hyd_tok,
            bm_hyd_tok,
            rec_total_tok,
            bm_total_tok
        );
    }

    println!("{}", "-".repeat(118));
    let n = questions.len() as f64;
    println!("\n=== AGGREGATE SUMMARY (n={}) ===", questions.len());
    println!(
        "Overall Recall@5:        Recognitor {:>5.1}% ({}/{})  vs  Basic Memory {:>5.1}% ({}/{})",
        (rec_hits_total as f64 / n) * 100.0,
        rec_hits_total,
        questions.len(),
        (bm_hits_total as f64 / n) * 100.0,
        bm_hits_total,
        questions.len()
    );
    println!(
        "Avg Search Tokens/Query: Recognitor {:>5.1} tokens  vs  Basic Memory {:>5.1} tokens ({:+.1}%)",
        rec_search_tokens_total as f64 / n,
        bm_search_tokens_total as f64 / n,
        ((rec_search_tokens_total as f64 - bm_search_tokens_total as f64) / bm_search_tokens_total as f64) * 100.0
    );
    println!(
        "Avg Hydration Tokens:    Recognitor {:>5.1} tokens  vs  Basic Memory {:>5.1} tokens ({:+.1}%)",
        rec_hyd_tokens_total as f64 / n,
        bm_hyd_tokens_total as f64 / n,
        ((rec_hyd_tokens_total as f64 - bm_hyd_tokens_total as f64) / bm_hyd_tokens_total as f64) * 100.0
    );
    let rec_grand_total = rec_search_tokens_total + rec_hyd_tokens_total;
    let bm_grand_total = bm_search_tokens_total + bm_hyd_tokens_total;
    println!(
        "Total Episode Tokens:    Recognitor {:>5} tokens  vs  Basic Memory {:>5} tokens ({:+.1}%)",
        rec_grand_total,
        bm_grand_total,
        ((rec_grand_total as f64 - bm_grand_total as f64) / bm_grand_total as f64) * 100.0
    );
    println!(
        "Avg Episode Latency:     Recognitor {:>5.1} ms      vs  Basic Memory {:>5.1} ms ({:+.1}%)",
        rec_latency_total_ms / n,
        bm_latency_total_ms / n,
        ((rec_latency_total_ms - bm_latency_total_ms) / bm_latency_total_ms) * 100.0
    );

    println!("\n=== DECISION GATE EVALUATION (Gemini Rubric) ===");
    let rec_acc = (rec_hits_total as f64 / n) * 100.0;
    let bm_acc = (bm_hits_total as f64 / n) * 100.0;
    let token_delta = ((rec_grand_total as f64 - bm_grand_total as f64) / bm_grand_total as f64) * 100.0;

    if rec_acc >= bm_acc && token_delta <= 15.0 {
        println!(
            "Verdict: Recognitor accuracy ({rec_acc:.1}%) >= Basic Memory ({bm_acc:.1}%) and token delta is {token_delta:+.1}%."
        );
        println!(
            "Decision: CLOSE/DEFER recall(query, max_tokens). Two-stage search + read_note is already token-competitive."
        );
        println!("Do NOT pay the 11th-tool MCP schema tax in every session.");
    } else if token_delta > 15.0 {
        println!("Verdict: Recognitor spends {token_delta:+.1}% more tokens than Basic Memory.");
        println!(
            "Decision: Check if excess spend is concentrated in multi-note hydration before opening recall(query, max_tokens)."
        );
    } else {
        println!("Verdict: Recognitor recall ({rec_acc:.1}%) trails Basic Memory ({bm_acc:.1}%).");
        println!("Decision: Check whether natural prompts expose lexical/semantic deficits for vector search.");
    }

    Ok(())
}

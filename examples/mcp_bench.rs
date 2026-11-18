//! Measurements for the MCP server (doc 07 §2.1 budget, §12 parity with the CLI).
//!
//! usage: cargo build --release --bin recognitor --example mcp_bench
//!        target/release/examples/mcp_bench <vault> <scratch-db> [--runs N] [--bin <recognitor binary>]
//!               [--search <query>[@<ns>]]... [--note <name>]...
//!
//! The database is a scratch file and is never inside the vault. The vault is only read. A scratch config file next
//! to the database keeps the user's own config out of it (`RECOGNITOR_CONFIG`). Figures only; no note text is
//! printed. `--search` is a `search` call (`text` or `text@namespace`); `--note` is a `context` and a `read_note`
//! call. `load_bootstrap` and `load_namespace` for every namespace of the index are always run.
//!
//! Reported:
//! - the tool-list cost: the `initialize` instructions and the `tools/list` result, in characters and tokens
//!   (characters ÷ 4, an estimate);
//! - per call: tokens, parity with the CLI twin (stdout byte for byte; for an error, stderr without its `error: `
//!   prefix), and the round trip over stdio against one resident server as min / median / p95 over N runs, with the
//!   gate closed (its time in `meta` set to now before each run) and expired (set to 0);
//! - a cold start: spawn, `initialize`, one `search`, exit;
//! - two servers on one database, the search set from both at once with the gate expired before every call.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use recognitor::db::{self, meta_keys};
use recognitor::index::now_ms;
use recognitor::{Index, SweepOptions};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const DEFAULT_RUNS: usize = 30;

struct Args {
    vault: PathBuf,
    db: PathBuf,
    runs: usize,
    bin: Option<PathBuf>,
    searches: Vec<(String, Option<String>)>,
    notes: Vec<String>,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut runs = DEFAULT_RUNS;
    let mut bin = None;
    let mut searches = Vec::new();
    let mut notes = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().ok_or("--runs needs a number")?.parse()?,
            "--bin" => bin = Some(PathBuf::from(args.next().ok_or("--bin needs a path")?)),
            "--search" => {
                let q = args.next().ok_or("--search needs a query")?;
                searches.push(match q.rsplit_once('@') {
                    Some((text, ns)) => (text.to_string(), Some(ns.to_string())),
                    None => (q, None),
                });
            }
            "--note" => notes.push(args.next().ok_or("--note needs a name")?),
            _ => positional.push(arg),
        }
    }
    if positional.len() != 2 || runs == 0 {
        return Err(
            "usage: mcp_bench <vault> <scratch-db> [--runs N] [--bin path] [--search q[@ns]]... [--note n]...".into(),
        );
    }
    let mut it = positional.into_iter();
    Ok(Args {
        vault: PathBuf::from(it.next().ok_or("vault")?),
        db: PathBuf::from(it.next().ok_or("db")?),
        runs,
        bin,
        searches,
        notes,
    })
}

fn default_binary() -> Res<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot find the target folder")?;
    Ok(dir.join(if cfg!(windows) { "recognitor.exe" } else { "recognitor" }))
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn stats(mut samples: Vec<f64>) -> String {
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    let p95 = samples[(n * 95).div_ceil(100).saturating_sub(1).min(n - 1)];
    format!("{:>7.2} / {:>7.2} / {:>7.2}", samples[0], samples[n / 2], p95)
}

fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// A resident `recognitor mcp` and its pipes.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start(bin: &Path, vault: &Path, db: &Path) -> Res<Server> {
        let mut child = Command::new(bin)
            .arg("mcp")
            .arg("--vault")
            .arg(vault)
            .env("RECOGNITOR_DB", db)
            .env("RECOGNITOR_CONFIG", db.with_extension("config.toml"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(Server {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// One request; the response line, parsed. Anything on stdout that is not JSON fails the run.
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
        if response["id"] != json!(id) {
            return Err(format!("unexpected response: {line}").into());
        }
        Ok(response)
    }

    fn initialize(&mut self) -> Res<Value> {
        let response = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mcp_bench", "version": "1"}}),
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

    /// A `tools/call`: the text and `isError`.
    fn call(&mut self, name: &str, arguments: &Value) -> Res<(String, bool)> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        let result = response
            .get("result")
            .ok_or_else(|| format!("protocol error: {response}"))?;
        let text = result["content"][0]["text"].as_str().ok_or("no text")?.to_string();
        Ok((text, result["isError"].as_bool().unwrap_or(false)))
    }

    fn finish(mut self) -> Res<()> {
        drop(self.stdin.take());
        self.child.wait()?;
        Ok(())
    }
}

/// One call to measure: the tool, its arguments, and the CLI twin's arguments when there is one.
struct Call {
    label: String,
    tool: &'static str,
    arguments: Value,
    cli: Option<Vec<String>>,
}

fn calls(args: &Args, namespaces: &[String]) -> Vec<Call> {
    let mut out = Vec::new();
    for (query, ns) in &args.searches {
        let mut arguments = json!({"query": query});
        let mut cli = vec!["search".to_string(), query.clone()];
        let mut label = format!("search {query:?}");
        if let Some(ns) = ns {
            arguments["ns"] = json!(ns);
            cli.extend(["--ns".to_string(), ns.clone()]);
            label.push_str(&format!(" ns={ns}"));
        }
        out.push(Call {
            label,
            tool: "search",
            arguments,
            cli: Some(cli),
        });
    }
    for note in &args.notes {
        out.push(Call {
            label: format!("context {note:?}"),
            tool: "context",
            arguments: json!({"note": note}),
            cli: Some(vec!["context".to_string(), note.clone()]),
        });
        out.push(Call {
            label: format!("read_note {note:?}"),
            tool: "read_note",
            arguments: json!({"note": note}),
            cli: Some(vec!["read".to_string(), note.clone()]),
        });
    }
    out.push(Call {
        label: "load_bootstrap".to_string(),
        tool: "load_bootstrap",
        arguments: json!({}),
        cli: Some(vec!["bootstrap".to_string()]),
    });
    for ns in namespaces {
        out.push(Call {
            label: format!("load_namespace {ns:?}"),
            tool: "load_namespace",
            arguments: json!({"ns": ns}),
            cli: None,
        });
    }
    out
}

/// What the CLI twin shows for the same call: stdout, or the error message when it failed.
fn cli_twin(bin: &Path, args: &Args, cli: &[String]) -> Res<(String, bool)> {
    let out = Command::new(bin)
        .args(cli)
        .arg("--vault")
        .arg(&args.vault)
        .env("RECOGNITOR_DB", &args.db)
        .env("RECOGNITOR_CONFIG", args.db.with_extension("config.toml"))
        .output()?;
    if out.status.success() {
        return Ok((String::from_utf8(out.stdout)?, false));
    }
    let stderr = String::from_utf8(out.stderr)?;
    let message = stderr.strip_prefix("error: ").unwrap_or(&stderr);
    Ok((message.trim_end_matches('\n').to_string(), true))
}

fn set_gate(conn: &rusqlite::Connection, expired: bool) -> Res<()> {
    let stamp = if expired { 0 } else { now_ms() };
    db::meta_set(conn, meta_keys::LAST_SWEEP_FINISHED_MS, &stamp.to_string())?;
    Ok(())
}

fn main() -> Res<()> {
    let args = parse_args()?;
    let bin = match &args.bin {
        Some(b) => b.clone(),
        None => default_binary()?,
    };
    let mut index = Index::open(&args.vault, Some(&args.db))?;
    let first = index.sync(SweepOptions::default())?;
    println!(
        "{} notes, {} observations, {} edges; first sweep read {} of {} files",
        first.totals.notes, first.totals.observations, first.totals.edges, first.files_read, first.files_seen
    );
    let namespaces: Vec<String> = {
        let mut stmt = index
            .connection()
            .prepare("SELECT DISTINCT namespace FROM notes ORDER BY namespace")?;
        stmt.query_map([], |row| row.get(0))?.collect::<Result<_, _>>()?
    };
    println!("namespaces: {}", namespaces.join(", "));

    // The tool-list cost (doc 07 §2.1, "Budget").
    let mut server = Server::start(&bin, &args.vault, &args.db)?;
    let init = server.initialize()?;
    let instructions = init["result"]["instructions"].as_str().unwrap_or_default().to_string();
    let list = server.request("tools/list", json!({}))?;
    let tool_list = &list["result"];
    let list_json = serde_json::to_string(tool_list)?;
    let mut list_text = String::new();
    for tool in tool_list["tools"].as_array().ok_or("no tools")? {
        list_text.push_str(&format!(
            "{}: {}\n",
            tool["name"].as_str().unwrap_or(""),
            tool["description"].as_str().unwrap_or("")
        ));
        for (name, prop) in tool["inputSchema"]["properties"].as_object().into_iter().flatten() {
            list_text.push_str(&format!("{name}: {}\n", prop["description"].as_str().unwrap_or("")));
        }
    }
    println!("\n=== tool list (doc 07 §2.1 budget: about 400 tokens (estimate))");
    println!(
        "instructions: {} chars, {} tokens (estimate)",
        instructions.chars().count(),
        tokens(&instructions)
    );
    println!(
        "tools/list result as sent (JSON): {} chars, {} tokens (estimate)",
        list_json.chars().count(),
        tokens(&list_json)
    );
    println!(
        "tools/list descriptions only (names, descriptions, argument descriptions): {} chars, {} tokens (estimate)",
        list_text.chars().count(),
        tokens(&list_text)
    );
    println!(
        "instructions + tools/list as sent: {} tokens (estimate); instructions + descriptions only: {} tokens (estimate)",
        tokens(&instructions) + tokens(&list_json),
        tokens(&instructions) + tokens(&list_text)
    );

    let all = calls(&args, &namespaces);
    println!(
        "\n=== calls: tokens (estimate), parity, latency min / median / p95 in ms over {} runs",
        args.runs
    );
    let mut parity = (0usize, 0usize, 0usize);
    let mut mismatches = Vec::new();
    for call in &all {
        let (text, is_error) = server.call(call.tool, &call.arguments)?;
        let parity_note = match &call.cli {
            Some(cli) => {
                let (expected, cli_error) = cli_twin(&bin, &args, cli)?;
                if expected == text && cli_error == is_error {
                    parity.0 += 1;
                    "parity ok"
                } else {
                    parity.1 += 1;
                    mismatches.push(call.label.clone());
                    "PARITY MISMATCH"
                }
            }
            None => {
                parity.2 += 1;
                "no CLI twin"
            }
        };
        let mut closed = Vec::new();
        let mut expired = Vec::new();
        for (samples, gate_expired) in [(&mut closed, false), (&mut expired, true)] {
            for _ in 0..args.runs {
                set_gate(index.connection(), gate_expired)?;
                let t = Instant::now();
                let (again, _) = server.call(call.tool, &call.arguments)?;
                samples.push(ms(t.elapsed()));
                if again != text {
                    return Err(format!("{}: the text changed between runs", call.label).into());
                }
            }
        }
        println!(
            "{:<58} {:>5} tokens{} {}; closed {}; expired {}",
            call.label,
            tokens(&text),
            if is_error { " (isError)" } else { "" },
            parity_note,
            stats(closed),
            stats(expired)
        );
    }
    println!(
        "parity: {} equal, {} different, {} without a CLI twin; different: {:?}",
        parity.0, parity.1, parity.2, mismatches
    );
    server.finish()?;

    // Cold start: spawn, initialize, one search, exit.
    let first_search = all.iter().find(|c| c.tool == "search");
    if let Some(call) = first_search {
        println!("\n=== cold start (spawn, initialize, one search, exit), ms");
        for (label, expired) in [("gate closed ", false), ("gate expired", true)] {
            let mut samples = Vec::new();
            for _ in 0..args.runs.min(10) {
                set_gate(index.connection(), expired)?;
                let t = Instant::now();
                let mut s = Server::start(&bin, &args.vault, &args.db)?;
                s.initialize()?;
                s.call(call.tool, &call.arguments)?;
                s.finish()?;
                samples.push(ms(t.elapsed()));
            }
            println!("{label}  min / median / p95 {}", stats(samples));
        }
    }

    // Two servers on one database.
    let searches: Vec<&Call> = all.iter().filter(|c| c.tool == "search").collect();
    if !searches.is_empty() {
        println!("\n=== two servers, one database, search set from both at once, gate expired before every call");
        let mut handles = Vec::new();
        for who in 0..2 {
            let bin = bin.clone();
            let vault = args.vault.clone();
            let db = args.db.clone();
            let runs = args.runs;
            let inputs: Vec<(String, Value)> = searches
                .iter()
                .map(|c| (c.label.clone(), c.arguments.clone()))
                .collect();
            handles.push(thread::spawn(move || -> Result<(Vec<f64>, Vec<String>), String> {
                let run = || -> Res<(Vec<f64>, Vec<String>)> {
                    let conn = db::open(&db)?;
                    let mut server = Server::start(&bin, &vault, &db)?;
                    server.initialize()?;
                    let mut samples = Vec::new();
                    let mut errors = Vec::new();
                    for _ in 0..runs {
                        for (label, arguments) in &inputs {
                            set_gate(&conn, true)?;
                            let t = Instant::now();
                            match server.call("search", arguments) {
                                Ok((_, false)) => samples.push(ms(t.elapsed())),
                                Ok((text, true)) => errors.push(format!("server {who}: {label}: {text}")),
                                Err(e) => errors.push(format!("server {who}: {label}: {e}")),
                            }
                        }
                    }
                    server.finish()?;
                    Ok((samples, errors))
                };
                run().map_err(|e| e.to_string())
            }));
        }
        let mut all_samples = Vec::new();
        let mut all_errors = Vec::new();
        for h in handles {
            let (samples, errors) = h.join().map_err(|_| "a server thread panicked")??;
            all_samples.extend(samples);
            all_errors.extend(errors);
        }
        println!(
            "{} calls ok, {} errors; min / median / p95 {}",
            all_samples.len(),
            all_errors.len(),
            if all_samples.is_empty() {
                "n/a".to_string()
            } else {
                stats(all_samples)
            }
        );
        for e in all_errors.iter().take(5) {
            println!("error: {e}");
        }
    }
    Ok(())
}

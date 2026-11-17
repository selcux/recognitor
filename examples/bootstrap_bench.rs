//! Timing bench for `bootstrap` (doc 02 §11: `load_bootstrap()` under 50 ms p95 in-process, `recognitor bootstrap`
//! under 100 ms p95 from a cold process start).
//!
//! usage: cargo build --release --bin recognitor --example bootstrap_bench
//!        target/release/examples/bootstrap_bench <vault> <scratch-db> [--runs N] [--bin <recognitor binary>]
//!               [--max-tokens N] [--show]
//!
//! The database is a scratch file and is never inside the vault; the vault is only read. The child process runs with
//! `RECOGNITOR_CONFIG` pointing at a scratch config file next to the database (never the per-user one), and
//! `--max-tokens` writes `bootstrap.max_tokens` into it. A vault without `bootstrap/index.md` is measured too: the
//! error path is timed and its text checked. Output: the notes in load order, tokens (estimate), the result text with
//! `--show`, and min / median / p95 over N runs in four forms:
//! - in-process, gate closed: `bootstrap::load` and the render, on an open connection, no sweep;
//! - in-process, gate expired: `Index::bootstrap`, which runs the no-change sweep first (the gate time in `meta` is
//!   set back to 0 before each run, outside the timed part), and the render;
//! - fresh process, gate closed: `recognitor bootstrap` spawned from here (gate time set to now before each spawn);
//! - fresh process, gate expired: the same with the gate time set to 0.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use recognitor::bootstrap::{self, DEFAULT_MAX_TOKENS};
use recognitor::db::{self, meta_keys};
use recognitor::index::now_ms;
use recognitor::{Index, SweepOptions, config};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const DEFAULT_RUNS: usize = 50;

struct Args {
    vault: PathBuf,
    db: PathBuf,
    runs: usize,
    bin: Option<PathBuf>,
    max_tokens: Option<usize>,
    show: bool,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut runs = DEFAULT_RUNS;
    let mut bin = None;
    let mut max_tokens = None;
    let mut show = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().ok_or("--runs needs a number")?.parse()?,
            "--bin" => bin = Some(PathBuf::from(args.next().ok_or("--bin needs a path")?)),
            "--max-tokens" => max_tokens = Some(args.next().ok_or("--max-tokens needs a number")?.parse()?),
            "--show" => show = true,
            _ => positional.push(arg),
        }
    }
    let [vault, db] = <[String; 2]>::try_from(positional)
        .map_err(|_| "usage: bootstrap_bench <vault> <scratch-db> [--runs N] [--bin path] [--max-tokens N] [--show]")?;
    if runs == 0 {
        return Err("--runs must be at least 1".into());
    }
    Ok(Args {
        vault: PathBuf::from(vault),
        db: PathBuf::from(db),
        runs,
        bin,
        max_tokens,
        show,
    })
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

fn default_binary() -> Res<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot find the target folder")?;
    Ok(dir.join(if cfg!(windows) { "recognitor.exe" } else { "recognitor" }))
}

/// The text the CLI prints on stdout (`Ok`) or its one error line on stderr (`Err`).
fn rendered(result: recognitor::Result<bootstrap::Bootstrap>, max_tokens: usize) -> Result<String, String> {
    match result {
        Ok(b) => Ok(b.render(max_tokens)),
        Err(e) => Err(format!("error: {e}\n")),
    }
}

fn main() -> Res<()> {
    let args = parse_args()?;
    let bin = match &args.bin {
        Some(b) => b.clone(),
        None => default_binary()?,
    };
    // A scratch config for the child process, so the per-user config never matters.
    let config_path = args.db.with_extension("config.toml");
    let _ = std::fs::remove_file(&config_path);
    if let Some(n) = args.max_tokens {
        config::set(&config_path, config::BOOTSTRAP_MAX_TOKENS_KEY, &n.to_string())?;
    }
    let max_tokens = args.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

    let mut index = Index::open(&args.vault, Some(&args.db))?;
    let first = index.sync(SweepOptions::default())?;
    println!(
        "{} notes, {} observations, {} edges; first sweep read {} of {} files",
        first.totals.notes, first.totals.observations, first.totals.edges, first.files_read, first.files_seen
    );
    println!("latency columns: min / median / p95 in ms over {} runs", args.runs);

    let expected = match bootstrap::load(index.connection()) {
        Ok(b) => {
            println!(
                "notes loaded: {}; not loaded: {}",
                b.notes.len(),
                if b.not_loaded.is_empty() {
                    "none".to_string()
                } else {
                    b.not_loaded.join(", ")
                }
            );
            for (i, n) in b.notes.iter().enumerate() {
                println!("  {:>2}. {} ({} chars)", i + 1, n.id, n.body.chars().count());
            }
            let payload = b.payload();
            let tokens = b.tokens();
            println!(
                "payload {} chars, {tokens} tokens (estimate); bootstrap.max_tokens {max_tokens}: {}",
                payload.chars().count(),
                if tokens > max_tokens { "WARNING" } else { "ok" }
            );
            println!("--list:\n{}", b.list());
            Ok(b.render(max_tokens))
        }
        Err(e) => Err(format!("error: {e}\n")),
    };
    match &expected {
        Ok(text) if args.show => println!("--- result ---\n{text}--- end ---"),
        Err(line) => println!("no payload; the CLI prints: {}", line.trim_end()),
        _ => {}
    }

    let mut warm = Vec::new();
    for _ in 0..args.runs {
        let t = Instant::now();
        let r = rendered(bootstrap::load(index.connection()), max_tokens);
        warm.push(ms(t.elapsed()));
        let _ = std::hint::black_box(r);
    }
    println!("in-process, gate closed   {}", stats(warm));

    let mut swept = Vec::new();
    for _ in 0..args.runs {
        db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0")?;
        let t = Instant::now();
        let r = rendered(index.bootstrap(), max_tokens);
        swept.push(ms(t.elapsed()));
        let _ = std::hint::black_box(r);
    }
    println!("in-process, gate expired  {}", stats(swept));

    for (label, expired) in [
        ("fresh process, gate closed ", false),
        ("fresh process, gate expired", true),
    ] {
        let mut wall = Vec::new();
        for _ in 0..args.runs {
            let stamp = if expired { 0 } else { now_ms() };
            db::meta_set(
                index.connection(),
                meta_keys::LAST_SWEEP_FINISHED_MS,
                &stamp.to_string(),
            )?;
            let t = Instant::now();
            let out = Command::new(&bin)
                .arg("bootstrap")
                .arg("--vault")
                .arg(&args.vault)
                .env("RECOGNITOR_DB", &args.db)
                .env("RECOGNITOR_CONFIG", &config_path)
                .env_remove("RECOGNITOR_VAULT")
                .output()?;
            wall.push(ms(t.elapsed()));
            let (stdout, stderr) = (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            );
            match &expected {
                Ok(text) if out.status.success() && &stdout == text => {}
                Err(line) if out.status.code() == Some(1) && stdout.is_empty() && &stderr == line => {}
                _ => {
                    return Err(format!(
                        "the fresh process printed other text than the library (exit {:?}): {stdout}{stderr}",
                        out.status.code()
                    )
                    .into());
                }
            }
        }
        println!("{label}  {}", stats(wall));
    }
    // The exit code of the error path, for the report.
    if expected.is_err() {
        let out = Command::new(&bin)
            .arg("bootstrap")
            .arg("--vault")
            .arg(&args.vault)
            .env("RECOGNITOR_DB", &args.db)
            .env("RECOGNITOR_CONFIG", &config_path)
            .env_remove("RECOGNITOR_VAULT")
            .output()?;
        println!("exit code: {:?}", out.status.code());
    }
    let _ = std::fs::remove_file(&config_path);
    Ok(())
}

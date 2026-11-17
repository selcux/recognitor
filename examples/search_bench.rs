//! Timing bench for `search` (doc 02 §11 targets full-text search under 50 ms p95).
//!
//! usage: cargo build --release --bin recognitor --example search_bench
//!        target/release/examples/search_bench <vault> <scratch-db> [--runs N] [--bin <recognitor binary>] [--show]
//!               <query>[@<ns>]...
//!
//! The database is a scratch file and is never inside the vault. The vault is only read. A query is written
//! `text` or `text@namespace`. Per query, one line of figures and, with `--show`, the text of page 1. Latency is
//! min / median / p95 over N runs, in four forms:
//! - in-process, gate closed: `search::search` on an open connection, no sweep;
//! - in-process, gate expired: `Index::search`, which runs the no-change sweep first (the gate time in `meta` is
//!   set back to 0 before each run, outside the timed part);
//! - fresh process, gate closed: `recognitor search` spawned from here (gate time set to now before each spawn);
//! - fresh process, gate expired: the same with the gate time set to 0.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use recognitor::db::{self, meta_keys};
use recognitor::index::now_ms;
use recognitor::search;
use recognitor::{Index, SweepOptions};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const DEFAULT_RUNS: usize = 30;

struct Args {
    vault: PathBuf,
    db: PathBuf,
    runs: usize,
    bin: Option<PathBuf>,
    show: bool,
    queries: Vec<(String, Option<String>)>,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut runs = DEFAULT_RUNS;
    let mut bin = None;
    let mut show = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().ok_or("--runs needs a number")?.parse()?,
            "--bin" => bin = Some(PathBuf::from(args.next().ok_or("--bin needs a path")?)),
            "--show" => show = true,
            _ => positional.push(arg),
        }
    }
    if positional.len() < 3 || runs == 0 {
        return Err("usage: search_bench <vault> <scratch-db> [--runs N] [--bin path] [--show] <query>[@ns]...".into());
    }
    let mut it = positional.into_iter();
    let vault = PathBuf::from(it.next().ok_or("vault")?);
    let db = PathBuf::from(it.next().ok_or("db")?);
    let queries = it
        .map(|q| match q.rsplit_once('@') {
            Some((text, ns)) => (text.to_string(), Some(ns.to_string())),
            None => (q, None),
        })
        .collect();
    Ok(Args {
        vault,
        db,
        runs,
        bin,
        show,
        queries,
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

fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
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
    println!("latency columns: min / median / p95 in ms over {} runs", args.runs);

    for (query, ns) in &args.queries {
        let ns = ns.as_deref();
        println!("\n=== {query:?} ns={ns:?}");

        // Page 1, then every later page, for the size figures.
        let page1 = search::search(index.connection(), query, ns, 1)?;
        let total = page1.hits.len() + page1.more;
        let pages = total.div_ceil(search::PAGE_SIZE);
        let text1 = search::render(&page1);
        let mut sizes = vec![tokens(&text1)];
        for p in 2..=pages {
            sizes.push(tokens(&search::render(&search::search(
                index.connection(),
                query,
                ns,
                p,
            )?)));
        }
        println!(
            "hits {total}, pages {pages}, tokens per page (estimate) {sizes:?}, page 1: {} chars",
            text1.chars().count()
        );
        if args.show {
            println!("--- page 1 ---\n{text1}--- end ---");
        }

        let mut warm = Vec::new();
        for _ in 0..args.runs {
            let t = Instant::now();
            let page = search::search(index.connection(), query, ns, 1)?;
            warm.push(ms(t.elapsed()));
            std::hint::black_box(page);
        }
        println!("in-process, gate closed   {}", stats(warm));

        let mut swept = Vec::new();
        for _ in 0..args.runs {
            db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0")?;
            let t = Instant::now();
            let page = index.search(query, ns, 1)?;
            swept.push(ms(t.elapsed()));
            std::hint::black_box(page);
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
                let mut cmd = Command::new(&bin);
                cmd.args(["search", query, "--vault"]).arg(&args.vault);
                if let Some(ns) = ns {
                    cmd.args(["--ns", ns]);
                }
                let t = Instant::now();
                let out = cmd.env("RECOGNITOR_DB", &args.db).output()?;
                wall.push(ms(t.elapsed()));
                if !out.status.success() {
                    return Err(format!("search failed: {}", String::from_utf8_lossy(&out.stderr)).into());
                }
                if String::from_utf8_lossy(&out.stdout) != text1 {
                    return Err("the fresh process printed other text than the library".into());
                }
            }
            println!("{label}  {}", stats(wall));
        }
    }
    Ok(())
}

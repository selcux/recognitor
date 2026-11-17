//! Timing bench for `context` (doc 07 §8).
//!
//! usage: cargo build --release --bin recognitor --example context_bench
//!        target/release/examples/context_bench <vault> <scratch-db> [--runs N] [--bin <recognitor binary>] [--show]
//!               <note>...
//!
//! The database is a scratch file and is never inside the vault. The vault is only read. Per note, one line of
//! figures and, with `--show`, the text of page 1. Latency is min / median / p95 over N runs, in four forms:
//! - in-process, gate closed: `context::context` on an open connection, no sweep;
//! - in-process, gate expired: `Index::context`, which runs the no-change sweep first (the gate time in `meta` is
//!   set back to 0 before each run, outside the timed part);
//! - fresh process, gate closed: `recognitor context` spawned from here (gate time set to now before each spawn);
//! - fresh process, gate expired: the same with the gate time set to 0.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use recognitor::context::{self, Context};
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
    show: bool,
    notes: Vec<String>,
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
        return Err("usage: context_bench <vault> <scratch-db> [--runs N] [--bin path] [--show] <note>...".into());
    }
    let mut it = positional.into_iter();
    let vault = PathBuf::from(it.next().ok_or("vault")?);
    let db = PathBuf::from(it.next().ok_or("db")?);
    Ok(Args {
        vault,
        db,
        runs,
        bin,
        show,
        notes: it.collect(),
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

    for note in &args.notes {
        println!("\n=== {note:?}");
        let page1 = match context::context(index.connection(), note, 1) {
            Ok(c) => c,
            Err(e) => {
                println!("error: {e}");
                continue;
            }
        };
        let text1 = context::render(&page1);
        match &page1 {
            Context::Several { ids, .. } => {
                println!("matches {} notes: {} tokens (estimate)", ids.len(), tokens(&text1))
            }
            Context::Page(p) => {
                let pages = p.pages.max(1);
                let mut sizes = vec![tokens(&text1)];
                for n in 2..=pages {
                    sizes.push(tokens(&context::render(&context::context(
                        index.connection(),
                        note,
                        n,
                    )?)));
                }
                let ghosts = text1.matches("(not written yet)").count();
                let archive = text1.matches("  (archive)").count();
                println!(
                    "links {} (out {} + in {} on page 1), pages {pages}, tokens per page (estimate) {sizes:?}, \
                     page 1 ghosts {ghosts}, archive {archive}",
                    p.total,
                    p.out.len(),
                    p.incoming.len()
                );
            }
        }
        if args.show {
            println!("--- page 1 ---\n{text1}--- end ---");
        }

        let mut warm = Vec::new();
        for _ in 0..args.runs {
            let t = Instant::now();
            let c = context::context(index.connection(), note, 1)?;
            warm.push(ms(t.elapsed()));
            std::hint::black_box(c);
        }
        println!("in-process, gate closed   {}", stats(warm));

        let mut swept = Vec::new();
        for _ in 0..args.runs {
            db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0")?;
            let t = Instant::now();
            let c = index.context(note, 1)?;
            swept.push(ms(t.elapsed()));
            std::hint::black_box(c);
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
                cmd.args(["context", note, "--vault"]).arg(&args.vault);
                let t = Instant::now();
                let out = cmd.env("RECOGNITOR_DB", &args.db).output()?;
                wall.push(ms(t.elapsed()));
                if !out.status.success() {
                    return Err(format!("context failed: {}", String::from_utf8_lossy(&out.stderr)).into());
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

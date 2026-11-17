//! Timing bench for the no-change sweep (doc 02 §11 targets under 100 ms on 10,000 files).
//!
//! usage: cargo build --release --bin recognitor --example sweep_bench
//!        target/release/examples/sweep_bench <vault> <scratch-db> [--runs N] [--bin <recognitor binary>]
//!
//! The database is a scratch file and is never inside the vault. The vault is only read.
//! Three measurements, each min / median / p95 over N runs:
//! - in-process sweep: a fresh `Index::open` and a no-change `sync` per run, one process;
//! - walk alone: `walk::walk` on its own;
//! - fresh process: `recognitor sync` spawned from here, wall time per spawn and the "in N ms" it prints.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use recognitor::walk::walk;
use recognitor::{Index, SweepOptions};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const DEFAULT_RUNS: usize = 60;

struct Args {
    vault: PathBuf,
    db: PathBuf,
    runs: usize,
    bin: Option<PathBuf>,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut runs = DEFAULT_RUNS;
    let mut bin = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().ok_or("--runs needs a number")?.parse()?,
            "--bin" => bin = Some(PathBuf::from(args.next().ok_or("--bin needs a path")?)),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let [vault, db] = <[PathBuf; 2]>::try_from(positional).map_err(|_| "usage: sweep_bench <vault> <scratch-db>")?;
    if runs == 0 {
        return Err("--runs must be at least 1".into());
    }
    Ok(Args { vault, db, runs, bin })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Prints `label  min / median / p95` in milliseconds.
fn summarize(label: &str, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    let p95 = samples[(n * 95).div_ceil(100).saturating_sub(1).min(n - 1)];
    println!(
        "{label:<28} min {:>7.2}  median {:>7.2}  p95 {:>7.2}  (n={n}, ms)",
        samples[0],
        samples[n / 2],
        p95
    );
}

/// The figure `recognitor sync` prints: "... files in 7 ms".
fn printed_ms(stdout: &str) -> Option<f64> {
    let line = stdout.lines().next()?;
    let tail = line.rsplit_once(" in ")?.1;
    tail.strip_suffix(" ms")?.trim().parse().ok()
}

fn default_binary() -> Res<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot find the target folder")?;
    Ok(dir.join(if cfg!(windows) { "recognitor.exe" } else { "recognitor" }))
}

fn main() -> Res<()> {
    let args = parse_args()?;
    let opts = SweepOptions { report: false };

    // Build the index once, then measure only no-change sweeps.
    let first = Index::open(&args.vault, Some(&args.db))?.sync(opts)?;
    println!(
        "{} notes, {} observations, {} edges, {} ghosts; first sweep read {} of {} files",
        first.totals.notes,
        first.totals.observations,
        first.totals.edges,
        first.totals.ghosts,
        first.files_read,
        first.files_seen
    );

    let (mut elapsed, mut call, mut open_and_call) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..args.runs {
        let start = Instant::now();
        let mut index = Index::open(&args.vault, Some(&args.db))?;
        let opened = Instant::now();
        let r = index.sync(opts)?;
        let done = Instant::now();
        if r.files_read != 0 {
            return Err(format!("a no-change sweep read {} files", r.files_read).into());
        }
        elapsed.push(ms(r.elapsed));
        call.push(ms(done - opened));
        open_and_call.push(ms(done - start));
    }
    summarize("in-process report.elapsed", elapsed);
    summarize("in-process sync() wall", call);
    summarize("in-process open+sync wall", open_and_call);

    let mut walks = Vec::new();
    for _ in 0..args.runs {
        let start = Instant::now();
        let walked = walk(&args.vault, false)?;
        walks.push(ms(start.elapsed()));
        std::hint::black_box(walked);
    }
    summarize("walk alone", walks);

    let bin = match args.bin {
        Some(b) => b,
        None => default_binary()?,
    };
    let (mut wall, mut printed) = (Vec::new(), Vec::new());
    for _ in 0..args.runs {
        let start = Instant::now();
        let out = Command::new(&bin)
            .args(["sync", "--vault"])
            .arg(&args.vault)
            .env("RECOGNITOR_DB", &args.db)
            .output()?;
        wall.push(ms(start.elapsed()));
        if !out.status.success() {
            return Err(format!("{} failed: {}", bin.display(), String::from_utf8_lossy(&out.stderr)).into());
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        printed.push(printed_ms(&stdout).ok_or("cannot read the 'in N ms' figure")?);
    }
    summarize("fresh process wall", wall);
    summarize("fresh process printed", printed);
    Ok(())
}

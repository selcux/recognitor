//! Timing bench for `rebuild` (doc 02 §11: full rebuild without vectors under 10 s p95 on 10,000 files).
//!
//! usage: cargo build --release --bin recognitor --example rebuild_bench
//!        target/release/examples/rebuild_bench <vault> <scratch-db> [--runs N] [--bin <recognitor binary>]
//!
//! The database is a scratch file and is never inside the vault. The vault is only read.
//! Measures:
//! - in-process rebuild(): drop and re-index whole vault on open connection;
//! - from-scratch rebuild: delete DB files, open fresh Index and rebuild;
//! - fresh process rebuild: spawned `recognitor rebuild` command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use recognitor::{Index, SweepOptions};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const DEFAULT_RUNS: usize = 15;

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
    let [vault, db] = <[PathBuf; 2]>::try_from(positional).map_err(|_| "usage: rebuild_bench <vault> <scratch-db>")?;
    if runs == 0 {
        return Err("--runs must be at least 1".into());
    }
    Ok(Args { vault, db, runs, bin })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

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

fn remove_db_files(db: &Path) -> Res<()> {
    if db.exists() {
        let _ = fs::remove_file(db);
    }
    let wal = db.with_extension("db-wal");
    if wal.exists() {
        let _ = fs::remove_file(wal);
    }
    let shm = db.with_extension("db-shm");
    if shm.exists() {
        let _ = fs::remove_file(shm);
    }
    Ok(())
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

    println!("=== 100x Scale Rebuild Benchmark (doc 02 §11) ===");
    println!("Vault:  {}", args.vault.display());
    println!("DB:     {}", args.db.display());
    println!("Runs:   {}", args.runs);

    // Initial baseline sync
    remove_db_files(&args.db)?;
    let t_init = Instant::now();
    let init_report = Index::open(&args.vault, Some(&args.db))?.sync(opts)?;
    println!(
        "Initial index built in {:>7.2} ms: {} notes, {} observations, {} edges, {} ghosts",
        ms(t_init.elapsed()),
        init_report.totals.notes,
        init_report.totals.observations,
        init_report.totals.edges,
        init_report.totals.ghosts
    );

    // 1. In-process rebuild on open connection
    let mut in_process_samples = Vec::with_capacity(args.runs);
    for _ in 0..args.runs {
        let mut index = Index::open(&args.vault, Some(&args.db))?;
        let start = Instant::now();
        let report = index.rebuild(opts)?;
        in_process_samples.push(ms(start.elapsed()));
        std::hint::black_box(report);
    }
    summarize("in-process rebuild() wall", in_process_samples);

    // 2. From-scratch rebuild (delete DB before each run)
    let mut scratch_samples = Vec::with_capacity(args.runs);
    for _ in 0..args.runs {
        remove_db_files(&args.db)?;
        let start = Instant::now();
        let mut index = Index::open(&args.vault, Some(&args.db))?;
        let report = index.rebuild(opts)?;
        scratch_samples.push(ms(start.elapsed()));
        std::hint::black_box(report);
    }
    summarize("from-scratch rebuild wall", scratch_samples);

    // 3. Fresh process rebuild
    let bin = match args.bin {
        Some(b) => b,
        None => default_binary()?,
    };
    let mut process_samples = Vec::with_capacity(args.runs);
    for _ in 0..args.runs {
        let start = Instant::now();
        let out = Command::new(&bin)
            .args(["rebuild", "--vault"])
            .arg(&args.vault)
            .env("RECOGNITOR_DB", &args.db)
            .output()?;
        process_samples.push(ms(start.elapsed()));
        if !out.status.success() {
            return Err(format!("{} failed: {}", bin.display(), String::from_utf8_lossy(&out.stderr)).into());
        }
    }
    summarize("fresh process rebuild wall", process_samples);

    Ok(())
}

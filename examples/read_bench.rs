//! Timing bench for `read` (doc 07 §7).
//!
//! usage: cargo build --release --bin recognitor --example read_bench
//!        target/release/examples/read_bench <vault> <scratch-db> [--runs N] [--limit N] [--query <text>]
//!               [--bin <recognitor binary>] <note>...
//!
//! The database is a scratch file and is never inside the vault. The vault is only read. `--limit` is passed to the
//! library directly and, for the fresh process, through a scratch config file next to the database (the user's own
//! config is never touched; `RECOGNITOR_CONFIG` points the spawned process at the scratch file). Without `--limit`
//! the default applies. Per note, one line of figures: output bytes, tokens (characters ÷ 4, an estimate), whether
//! the read was cut, the header line and, for a cut read, the closing line. Then the longest notes of the index by
//! file size (ids and figures only). Latency is min / median / p95 over N runs, in four forms:
//! - in-process, gate closed: `read::read` on an open connection, no sweep;
//! - in-process, gate expired: `Index::read`, which runs the no-change sweep first (the gate time in `meta` is set
//!   back to 0 before each run, outside the timed part);
//! - fresh process, gate closed: `recognitor read` spawned from here (gate time set to now before each spawn);
//! - fresh process, gate expired: the same with the gate time set to 0.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use recognitor::config;
use recognitor::db::{self, meta_keys};
use recognitor::index::now_ms;
use recognitor::read::{self, DEFAULT_MAX_TOKENS, Read};
use recognitor::{Index, SweepOptions};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const DEFAULT_RUNS: usize = 30;
const LONGEST: usize = 5;

struct Args {
    vault: PathBuf,
    db: PathBuf,
    runs: usize,
    limit: usize,
    query: Option<String>,
    bin: Option<PathBuf>,
    notes: Vec<String>,
}

fn parse_args() -> Res<Args> {
    let mut positional = Vec::new();
    let mut runs = DEFAULT_RUNS;
    let mut limit = DEFAULT_MAX_TOKENS;
    let mut query = None;
    let mut bin = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => runs = args.next().ok_or("--runs needs a number")?.parse()?,
            "--limit" => limit = args.next().ok_or("--limit needs a number")?.parse()?,
            "--query" => query = Some(args.next().ok_or("--query needs text")?),
            "--bin" => bin = Some(PathBuf::from(args.next().ok_or("--bin needs a path")?)),
            _ => positional.push(arg),
        }
    }
    if positional.len() < 2 || runs == 0 {
        return Err(
            "usage: read_bench <vault> <scratch-db> [--runs N] [--limit N] [--query text] [--bin path] <note>..."
                .into(),
        );
    }
    let mut it = positional.into_iter();
    let vault = PathBuf::from(it.next().ok_or("vault")?);
    let db = PathBuf::from(it.next().ok_or("db")?);
    Ok(Args {
        vault,
        db,
        runs,
        limit,
        query,
        bin,
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
        "{} notes; first sweep read {} of {} files",
        first.totals.notes, first.files_read, first.files_seen
    );

    let mut sizes: Vec<(String, i64)> = index
        .connection()
        .prepare("SELECT path, size FROM notes ORDER BY size DESC, path LIMIT ?1")?
        .query_map([LONGEST as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let (total, count): (i64, i64) =
        index
            .connection()
            .query_row("SELECT SUM(size), COUNT(*) FROM notes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
    println!(
        "average note {} bytes ({} tokens, estimate); the {LONGEST} longest by file size:",
        total / count.max(1),
        total / count.max(1) / 4
    );
    let lib = |index: &Index, note: &str| {
        read::read(
            index.connection(),
            index.vault_root(),
            note,
            None,
            args.query.as_deref(),
            args.limit,
        )
    };
    for (path, size) in sizes.drain(..) {
        let id = recognitor::paths::note_id(&path).to_string();
        let r = lib(&index, &id)?;
        let out = read::render(&r);
        println!(
            "  {id}: {size} bytes, output {} tokens (estimate), cut at {}: {}",
            tokens(&out),
            args.limit,
            matches!(r, Read::Cut(_))
        );
    }
    println!("latency columns: min / median / p95 in ms over {} runs", args.runs);

    // A scratch config for the spawned process: the limit under test, and never the user's own file.
    let scratch_config = args.db.with_extension("config.toml");
    let _ = std::fs::remove_file(&scratch_config);
    if args.limit != DEFAULT_MAX_TOKENS {
        config::set(&scratch_config, "read.max_tokens", &args.limit.to_string())?;
    }

    for note in &args.notes {
        println!(
            "
=== {note:?}"
        );
        let r = match lib(&index, note) {
            Ok(r) => r,
            Err(e) => {
                println!("error: {e}");
                continue;
            }
        };
        let text = read::render(&r);
        println!(
            "output {} bytes, {} tokens (estimate), limit {}, cut: {}",
            text.len(),
            tokens(&text),
            args.limit,
            matches!(r, Read::Cut(_))
        );
        println!("header: {}", text.lines().next().unwrap_or(""));
        if matches!(r, Read::Cut(_)) {
            println!(
                "closing: {}",
                text.lines().rev().find(|l| l.starts_with("[cut at")).unwrap_or("")
            );
        }

        let mut warm = Vec::new();
        for _ in 0..args.runs {
            let t = Instant::now();
            let c = lib(&index, note)?;
            warm.push(ms(t.elapsed()));
            std::hint::black_box(c);
        }
        println!("in-process, gate closed   {}", stats(warm));

        let mut swept = Vec::new();
        for _ in 0..args.runs {
            db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0")?;
            let t = Instant::now();
            let c = index.read(note, None, args.query.as_deref(), args.limit)?;
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
                cmd.args(["read", note, "--vault"]).arg(&args.vault);
                if let Some(q) = &args.query {
                    cmd.args(["--query", q]);
                }
                let t = Instant::now();
                let out = cmd
                    .env("RECOGNITOR_DB", &args.db)
                    .env("RECOGNITOR_CONFIG", &scratch_config)
                    .output()?;
                wall.push(ms(t.elapsed()));
                if !out.status.success() {
                    return Err(format!("read failed: {}", String::from_utf8_lossy(&out.stderr)).into());
                }
                if String::from_utf8_lossy(&out.stdout) != text {
                    return Err("the fresh process printed other text than the library".into());
                }
            }
            println!("{label}  {}", stats(wall));
        }
    }
    Ok(())
}

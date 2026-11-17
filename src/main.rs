//! `recognitor` CLI: a thin layer over the library. Commands are defined in doc 07 §12.
//!
//! Shadow mode builds `sync` and `rebuild` only. The vault is passed with `--vault`: the docs do not say yet where the
//! CLI learns the vault root.

use std::path::PathBuf;
use std::process::ExitCode;

use recognitor::{Index, SweepOptions, report};

const USAGE: &str = "usage: recognitor <command> --vault <path> [--report]

commands:
  sync       sweep the vault and index changed files
  rebuild    drop the index and re-index the whole vault

options:
  --vault <path>   vault root (required)
  --report         list every warning and print shadow-mode diagnostics
  -V, --version    print the version

The database lives in the per-user cache directory; RECOGNITOR_DB overrides its path.";

enum Command {
    Sync,
    Rebuild,
}

struct Args {
    command: Command,
    vault: PathBuf,
    report: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut command = None;
    let mut vault = None;
    let mut report = false;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => return Ok(None),
            "--vault" => vault = Some(PathBuf::from(args.next().ok_or("--vault needs a path")?)),
            "--report" => report = true,
            "sync" if command.is_none() => command = Some(Command::Sync),
            "rebuild" if command.is_none() => command = Some(Command::Rebuild),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let command = command.ok_or("no command given")?;
    let vault = vault.ok_or("--vault is required")?;
    Ok(Some(Args { command, vault, report }))
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("recognitor {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let opts = SweepOptions { report: args.report };
    let result = Index::open(&args.vault, None).and_then(|mut index| match args.command {
        Command::Sync => index.sync(opts),
        Command::Rebuild => index.rebuild(opts),
    });
    match result {
        Ok(r) => {
            print!("{}", report::render(&r, args.report));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

//! `recognitor` CLI: a thin layer over the library. Commands are defined in doc 07 §12.
//!
//! Shadow mode builds `sync`, `rebuild`, `search`, `context`, `read`, `bootstrap` and `config get/set`. The vault comes from
//! `--vault`, else `RECOGNITOR_VAULT`, else `vault.path` in the config file (doc 07 §12, "Which vault").

use std::path::PathBuf;
use std::process::ExitCode;

use recognitor::bootstrap::DEFAULT_MAX_TOKENS;
use recognitor::config::{self, VAULT_ENV};
use recognitor::paths::DB_ENV;
use recognitor::read::DEFAULT_MAX_TOKENS as READ_DEFAULT_MAX_TOKENS;
use recognitor::{Error, Index, SweepOptions, report};

const USAGE: &str = "usage: recognitor <command> [--vault <path>] [--report]
       recognitor search <query> [--vault <path>] [--ns <namespace>] [--page <n>]
       recognitor context <note> [--vault <path>] [--page <n>]
       recognitor read <note> [--section <heading>] [--query <text>] [--vault <path>]
       recognitor bootstrap [--list] [--vault <path>]
       recognitor config get <key>
       recognitor config set <key> <value>

commands:
  sync                      sweep the vault and index changed files
  rebuild                   drop the index and re-index the whole vault
  search <query>            full-text search
  context <note>            the links around one note: an id, a path or a link name
  read <note>               one note under a header line with its checksum; --section reads one part, a long note is cut
  bootstrap                 print the bootstrap payload (the notes linked from bootstrap/index.md)
  config get <key>          print a config value
  config set <key> <value>  set a config value

config keys: vault.path, db.path, bootstrap.max_tokens, read.max_tokens

options:
  --vault <path>   vault root; else RECOGNITOR_VAULT, else vault.path in the config file
  --report         list every warning and print shadow-mode diagnostics
  --ns <name>      search: the namespace as a ranking perspective
  --page <n>       search, context: page number, from 1
  --section <h>    read: only this section, by heading text or a path (`Storage > Notes`)
  --query <text>   read: guides which sections stay when a long note is cut
  --list           bootstrap: list the notes that load, in order, without their content
  -V, --version    print the version

The database lives in the per-user cache directory; RECOGNITOR_DB or db.path in the config file overrides its path.";

enum ConfigCommand {
    Get(String),
    Set(String, String),
}

enum Command {
    Sync,
    Rebuild,
    Search {
        query: String,
        ns: Option<String>,
        page: usize,
    },
    Context {
        note: String,
        page: usize,
    },
    Read {
        note: String,
        section: Option<String>,
        query: Option<String>,
    },
    Bootstrap {
        list: bool,
    },
    Config(ConfigCommand),
}

struct Args {
    command: Command,
    vault: Option<PathBuf>,
    report: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut positional: Vec<String> = Vec::new();
    let mut vault = None;
    let mut report = false;
    let mut ns = None;
    let mut page = 1;
    let mut list = false;
    let mut section = None;
    let mut query = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => return Ok(None),
            "--vault" => vault = Some(PathBuf::from(args.next().ok_or("--vault needs a path")?)),
            "--report" => report = true,
            "--list" => list = true,
            "--section" => section = Some(args.next().ok_or("--section needs a heading")?),
            "--query" => query = Some(args.next().ok_or("--query needs text")?),
            "--ns" => ns = Some(args.next().ok_or("--ns needs a namespace")?),
            "--page" => {
                page = args
                    .next()
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|p| *p >= 1)
                    .ok_or("--page needs a number from 1")?;
            }
            _ => positional.push(arg),
        }
    }
    let mut words = positional.into_iter();
    let name = words.next().ok_or("no command given")?;
    let rest: Vec<String> = words.collect();
    if ns.is_some() && name != "search" {
        return Err("--ns belongs to search".to_string());
    }
    if (section.is_some() || query.is_some()) && name != "read" {
        return Err("--section and --query belong to read".to_string());
    }
    if list && name != "bootstrap" {
        return Err("--list belongs to bootstrap".to_string());
    }
    let command = match (name.as_str(), rest.as_slice()) {
        ("sync", []) => Command::Sync,
        ("rebuild", []) => Command::Rebuild,
        ("search", []) => return Err("search needs a query".to_string()),
        ("search", [query]) => Command::Search {
            query: query.clone(),
            ns,
            page,
        },
        ("context", []) => return Err("context needs a note".to_string()),
        ("context", [note]) => Command::Context {
            note: note.clone(),
            page,
        },
        ("read", []) => return Err("read needs a note".to_string()),
        ("read", [note]) => Command::Read {
            note: note.clone(),
            section,
            query,
        },
        ("bootstrap", []) => Command::Bootstrap { list },
        ("config", [sub, key]) if sub == "get" => Command::Config(ConfigCommand::Get(key.clone())),
        ("config", [sub, key, value]) if sub == "set" => {
            Command::Config(ConfigCommand::Set(key.clone(), value.clone()))
        }
        ("config", _) => return Err("usage: config get <key> | config set <key> <value>".to_string()),
        ("sync" | "rebuild" | "search" | "context" | "read" | "bootstrap", [extra, ..]) => {
            return Err(format!("unexpected argument: {extra}"));
        }
        (other, _) => return Err(format!("unknown command: {other}")),
    };
    if vault.is_some() && matches!(command, Command::Config(_)) {
        return Err("--vault does not apply to config".to_string());
    }
    Ok(Some(Args { command, vault, report }))
}

fn run_config(cmd: &ConfigCommand) -> recognitor::Result<String> {
    let config_file = config::config_path()?;
    match cmd {
        ConfigCommand::Get(key) => config::get(&config_file, key)?
            .map(|value| {
                format!(
                    "{value}
"
                )
            })
            .ok_or_else(|| Error::ConfigNotSet(key.clone())),
        ConfigCommand::Set(key, value) => {
            config::set(&config_file, key, value)?;
            Ok(format!(
                "{key} set in {}
",
                config_file.display()
            ))
        }
    }
}

fn run(args: &Args) -> recognitor::Result<String> {
    let opts = SweepOptions { report: args.report };
    let command = match &args.command {
        Command::Config(cmd) => return run_config(cmd),
        other => other,
    };
    let config_file = config::config_path()?;
    let vault = config::resolve_vault(args.vault.clone(), std::env::var_os(VAULT_ENV), &config_file)?;
    let db = config::resolve_db(std::env::var_os(DB_ENV), &config_file)?;
    let mut index = Index::open(&vault, db.as_deref())?;
    match command {
        Command::Sync => Ok(report::render(&index.sync(opts)?, args.report)),
        Command::Rebuild => Ok(report::render(&index.rebuild(opts)?, args.report)),
        Command::Search { query, ns, page } => Ok(recognitor::search::render(&index.search(
            query,
            ns.as_deref(),
            *page,
        )?)),
        Command::Context { note, page } => Ok(recognitor::context::render(&index.context(note, *page)?)),
        Command::Read { note, section, query } => {
            let max_tokens = config::read_max_tokens(&config_file)?.unwrap_or(READ_DEFAULT_MAX_TOKENS);
            Ok(recognitor::read::render(&index.read(
                note,
                section.as_deref(),
                query.as_deref(),
                max_tokens,
            )?))
        }
        Command::Bootstrap { list } => {
            let bootstrap = index.bootstrap()?;
            if *list {
                return Ok(bootstrap.list());
            }
            let max_tokens = config::bootstrap_max_tokens(&config_file)?.unwrap_or(DEFAULT_MAX_TOKENS);
            Ok(bootstrap.render(max_tokens))
        }
        Command::Config(cmd) => run_config(cmd),
    }
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
    match run(&args) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            if e.is_usage() {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

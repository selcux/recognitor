//! `recognitor` CLI: a thin layer over the library. Commands are defined in doc 07 §12.
//!
//! Built so far: `sync`, `rebuild`, `search`, `context`, `read`, `bootstrap`, `move`, `relink`, `lint`, `ghosts`, `mcp`
//! and `config get/set`. The
//! vault comes from `--vault`, else `RECOGNITOR_VAULT`, else `vault.path` in the config file (doc 07 §12, "Which vault").
//!
//! `move` is the one command that talks to the user (doc 07 §6.1): it moves the note, prints the broken links, asks which
//! linking notes to relink, and relinks them. The write lock is never held while it waits for the answer: the move
//! commits first, and `relink` takes its own lock afterwards.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use recognitor::bootstrap::DEFAULT_MAX_TOKENS;
use recognitor::config::{self, VAULT_ENV};
use recognitor::move_note::MoveNote;
use recognitor::paths::DB_ENV;
use recognitor::read::DEFAULT_MAX_TOKENS as READ_DEFAULT_MAX_TOKENS;
use recognitor::read::Read;
use recognitor::{Error, Index, SweepOptions, report};

const USAGE: &str = "usage: recognitor <command> [--vault <path>] [--report]
       recognitor search <query> [--vault <path>] [--ns <namespace>] [--page <n>]
       recognitor context <note> [--vault <path>] [--page <n>]
       recognitor read <note> [--section <heading>] [--query <text>] [--vault <path>]
       recognitor bootstrap [--list] [--vault <path>]
       recognitor move <note> <new_path> [--relink all|none|ask] [--vault <path>]
       recognitor relink <old> <new> <note>... [--vault <path>]
       recognitor lint [--strict] [--vault <path>]
       recognitor ghosts [<link>] [--vault <path>]
       recognitor mcp [--vault <path>]
       recognitor config get <key>
       recognitor config set <key> <value>

commands:
  sync                      sweep the vault and index changed files
  rebuild                   drop the index and re-index the whole vault
  search <query>            full-text search
  context <note>            the links around one note: an id, a path or a link name
  read <note>               one note under a header line with its checksum; --section reads one part, a long note is cut
  bootstrap                 print the bootstrap payload (the notes linked from bootstrap/index.md)
  move <note> <new_path>    move or rename a note, then ask which linking notes to relink; new_path is from the vault root
  relink <old> <new> <note>...
                            rewrite the links to a moved note in the notes given; old is its id before the move
  lint                      check the vault against doc 02 §8; exits 1 on an error, with --strict on a warning too
  ghosts [<link>]           the links that reach no note, with a hint where a moved note went; <link> lists its linking notes
  mcp                       serve the tools over MCP on stdio (one server per client session)
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
  --relink <how>   move: all, none or ask. Default: ask when stdin is a terminal, else none (the printed relink command finishes it)
  --strict         lint: turn every warning into an error (hints stay hints)
  --list           bootstrap: list the notes that load, in order, without their content
  -V, --version    print the version

The database lives in the per-user cache directory; RECOGNITOR_DB or db.path in the config file overrides its path.";

enum ConfigCommand {
    Get(String),
    Set(String, String),
}

/// How `move` decides which linking notes to relink (doc 07 §6.1 leaves the prompt to the CLI).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RelinkMode {
    /// `ask` when stdin is a terminal, else `none`.
    Auto,
    All,
    None,
    /// Ask on stdin, even when it is not a terminal (`echo 1,2 | recognitor move ... --relink ask`).
    Ask,
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
    Move {
        note: String,
        new_path: String,
        relink: RelinkMode,
    },
    Relink {
        old: String,
        new: String,
        notes: Vec<String>,
    },
    Lint {
        strict: bool,
    },
    Ghosts {
        link: Option<String>,
    },
    Mcp,
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
    let mut relink_mode = None;
    let mut strict = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => return Ok(None),
            "--vault" => vault = Some(PathBuf::from(args.next().ok_or("--vault needs a path")?)),
            "--report" => report = true,
            "--list" => list = true,
            "--strict" => strict = true,
            "--section" => section = Some(args.next().ok_or("--section needs a heading")?),
            "--query" => query = Some(args.next().ok_or("--query needs text")?),
            "--relink" => {
                relink_mode = Some(match args.next().as_deref() {
                    Some("all") => RelinkMode::All,
                    Some("none") => RelinkMode::None,
                    Some("ask") => RelinkMode::Ask,
                    _ => return Err("--relink needs all, none or ask".to_string()),
                });
            }
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
    if relink_mode.is_some() && name != "move" {
        return Err("--relink belongs to move".to_string());
    }
    if list && name != "bootstrap" {
        return Err("--list belongs to bootstrap".to_string());
    }
    if strict && name != "lint" {
        return Err("--strict belongs to lint".to_string());
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
        ("move", [note, new_path]) => Command::Move {
            note: note.clone(),
            new_path: new_path.clone(),
            relink: relink_mode.unwrap_or(RelinkMode::Auto),
        },
        ("move", _) => return Err("move needs a note and a new_path, and takes nothing else".to_string()),
        ("relink", [old, new, notes @ ..]) if !notes.is_empty() => Command::Relink {
            old: old.clone(),
            new: new.clone(),
            notes: notes.to_vec(),
        },
        ("relink", _) => return Err("relink needs old, new and at least one note".to_string()),
        ("lint", []) => Command::Lint { strict },
        ("ghosts", []) => Command::Ghosts { link: None },
        ("ghosts", [link]) => Command::Ghosts {
            link: Some(link.clone()),
        },
        ("ghosts", [_, extra, ..]) => return Err(format!("unexpected argument: {extra}")),
        ("mcp", []) => Command::Mcp,
        ("config", [sub, key]) if sub == "get" => Command::Config(ConfigCommand::Get(key.clone())),
        ("config", [sub, key, value]) if sub == "set" => {
            Command::Config(ConfigCommand::Set(key.clone(), value.clone()))
        }
        ("config", _) => return Err("usage: config get <key> | config set <key> <value>".to_string()),
        ("sync" | "rebuild" | "search" | "context" | "read" | "bootstrap" | "lint" | "mcp", [extra, ..]) => {
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
        Command::Mcp => return run_mcp(args),
        other => other,
    };
    let (mut index, config_file) = open_index(args)?;
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
        Command::Move { note, new_path, relink } => {
            let max_tokens = config::read_max_tokens(&config_file)?.unwrap_or(READ_DEFAULT_MAX_TOKENS);
            run_move(&mut index, args.vault.as_deref(), note, new_path, *relink, max_tokens)
        }
        Command::Relink { old, new, notes } => Ok(recognitor::relink::render(&index.relink(old, new, notes)?)),
        Command::Ghosts { link } => Ok(recognitor::ghosts::render(
            &index.ghosts(link.as_deref())?,
            args.vault.as_deref(),
        )),
        Command::Config(cmd) => run_config(cmd),
        // `main` runs `lint` itself, because its exit code depends on what it finds.
        Command::Lint { .. } => Ok(String::new()),
        Command::Mcp => run_mcp(args),
    }
}

/// Finds the vault and the database like every command (doc 07 §12, doc 05 §5) and opens the index.
fn open_index(args: &Args) -> recognitor::Result<(Index, PathBuf)> {
    let config_file = config::config_path()?;
    let vault = config::resolve_vault(args.vault.clone(), std::env::var_os(VAULT_ENV), &config_file)?;
    let db = config::resolve_db(std::env::var_os(DB_ENV), &config_file)?;
    let index = Index::open(&vault, db.as_deref())?;
    Ok((index, config_file))
}

/// `recognitor lint [--strict]`: the report goes to stdout whatever it finds. The exit code is 1 when it holds an
/// error, which with `--strict` includes every warning (doc 02 §8).
fn run_lint(args: &Args, strict: bool) -> recognitor::Result<(String, ExitCode)> {
    let (mut index, _) = open_index(args)?;
    let report = recognitor::lint::lint(&mut index, strict)?;
    let code = if report.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    Ok((report.render(), code))
}

/// `recognitor move` (doc 07 §6.1, "CLI"): the checksum of the note as it is now, `move_note`, the broken links, the
/// prompt, `relink`.
///
/// - **The checksum.** `move_note` guards the move with the checksum of the version the caller read (doc 07 §5.2). A
///   person at a prompt read nothing, so the CLI reads the note itself, a moment before, and passes that checksum: an
///   editor save in the gap still refuses the move. A name that matches several notes lists them and moves none.
/// - **Output.** The move block goes to stdout, flushed before anything is asked; the prompt and its notes go to
///   stderr, so stdout holds only the Markdown text.
/// - **The lock.** `move_note` commits and releases the write lock before the prompt; `relink` takes its own.
/// - **No answer.** Ctrl-C, the end of input or Enter leave the links as they are. The block already printed the
///   `recognitor relink` command that finishes the job.
fn run_move(
    index: &mut Index,
    vault_flag: Option<&Path>,
    note: &str,
    new_path: &str,
    mode: RelinkMode,
    max_tokens: usize,
) -> recognitor::Result<String> {
    let checksum = match index.read(note, None, None, max_tokens)? {
        Read::Whole(n) | Read::Cut(n) => n.checksum.unwrap_or_default(),
        other => return Ok(recognitor::read::render(&other)),
    };
    let moved = match index.move_note(note, new_path, &checksum, max_tokens)? {
        MoveNote::Refused(text) => return Err(Error::Refused(text)),
        MoveNote::Normal(text) => return Ok(format!("{text}\n")),
        moved @ MoveNote::Moved { .. } => moved,
    };
    let finish =
        |old: &str, new: &str, notes: &[String]| recognitor::move_cli::finish_command(old, new, notes, vault_flag);
    print!("{}", recognitor::move_note::render_cli(&moved, finish));
    let _ = io::stdout().flush();
    let MoveNote::Moved { old_id, id, relink, .. } = &moved else {
        return Ok(String::new());
    };
    if relink.is_empty() {
        return Ok(String::new());
    }
    let ask = match mode {
        RelinkMode::Ask => true,
        RelinkMode::Auto => {
            let terminal = io::stdin().is_terminal();
            if !terminal {
                eprintln!(
                    "stdin is not a terminal, so no links were rewritten. Run the relink command above, or pass --relink all."
                );
            }
            terminal
        }
        RelinkMode::All | RelinkMode::None => false,
    };
    let chosen: Vec<String> = if ask {
        let stdin = io::stdin();
        recognitor::move_cli::ask(relink, &mut stdin.lock(), &mut io::stderr().lock()).map_err(|e| Error::Io {
            path: PathBuf::from("<stdin>"),
            source: e,
        })?
    } else if mode == RelinkMode::All {
        relink.clone()
    } else {
        Vec::new()
    };
    if chosen.is_empty() {
        if ask {
            eprintln!("No links were rewritten. The relink command above does it later.");
        }
        return Ok(String::new());
    }
    let mut text = recognitor::relink::render(&index.relink(old_id, id, &chosen)?);
    // Doc 07 §12.1, "Numbers that leave notes out": the command again, for the notes left out, so the last command on
    // screen is the one that finishes the job.
    let left_out: Vec<String> = relink.iter().filter(|n| !chosen.contains(n)).cloned().collect();
    if !left_out.is_empty() {
        text.push_str(&recognitor::move_cli::finish_command(old_id, id, &left_out, vault_flag));
        text.push('\n');
    }
    Ok(text)
}

/// `recognitor mcp`: finds the vault and the database like every command, then serves until stdin closes (doc 07 §1,
/// §12). Nothing goes to stdout but protocol messages; the text of an error goes to stderr in `main`.
fn run_mcp(args: &Args) -> recognitor::Result<String> {
    let (index, config_file) = open_index(args)?;
    recognitor::mcp::serve_stdio(index, config_file)?;
    Ok(String::new())
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
    let result = match &args.command {
        Command::Lint { strict } => run_lint(&args, *strict),
        _ => run(&args).map(|text| (text, ExitCode::SUCCESS)),
    };
    match result {
        Ok((text, code)) => {
            print!("{text}");
            code
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

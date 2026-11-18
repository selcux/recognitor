//! MCP server tests (doc 07 §1 "Transport", §2.1, §3). Each test starts the built binary with `mcp` and speaks
//! JSON-RPC to it over stdin and stdout. Vaults are synthetic and live in temp folders that delete themselves.
//!
//! The text the server must send word for word is read from doc 07 §2.1 itself, so the tests and the doc cannot
//! drift apart.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

const DOC_07: &str = include_str!("../docs/07-interface-contracts.md");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
    config: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-mcp-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let v = TestVault {
            root,
            db: base.join("index.db"),
            config: base.join("config.toml"),
        };
        for (rel, text) in files {
            v.write(rel, text);
        }
        v
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// The command with a hermetic environment: the scratch database and config file, no vault from outside.
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_recognitor"));
        c.env("RECOGNITOR_DB", &self.db)
            .env("RECOGNITOR_CONFIG", &self.config)
            .env_remove("RECOGNITOR_VAULT");
        c
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .arg("--vault")
            .arg(&self.root)
            .output()
            .unwrap()
    }

    /// The CLI's stdout; the command must have succeeded.
    fn cli_text(&self, args: &[&str]) -> String {
        let out = self.cli(args);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    fn server(&self) -> Server {
        let mut child = self
            .command()
            .arg("mcp")
            .arg("--vault")
            .arg(&self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        };
        server.initialize();
        server
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

/// A running `recognitor mcp` and its pipes. Every line it writes to stdout must be one JSON message.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(20))
            .expect("the server sent nothing");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout has a line that is not JSON ({e}): {line}"))
    }

    /// One request and its response; fails the test on a JSON-RPC error unless `allow_error`.
    fn request_raw(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let response = self.read();
        assert_eq!(response["id"], json!(id), "{response}");
        response
    }

    fn initialize(&mut self) -> Value {
        let response = self.request_raw(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        response
    }

    fn list_tools(&mut self) -> Value {
        let response = self.request_raw("tools/list", json!({}));
        response["result"].clone()
    }

    /// A `tools/call` result as `(text, isError)`. A JSON-RPC error fails the test.
    fn call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let response = self.request_raw("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        assert!(result.is_object(), "not a tool result: {response}");
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "{response}");
        assert_eq!(content[0]["type"], "text", "{response}");
        let is_error = result["isError"].as_bool().unwrap_or(false);
        (content[0]["text"].as_str().unwrap().to_string(), is_error)
    }

    /// A call that must come back as normal text.
    fn ok(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.call(name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        text
    }

    /// A call that must come back with `isError: true`; returns its text.
    fn err(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.call(name, arguments);
        assert!(is_error, "{name} should have failed, but returned: {text}");
        text
    }

    /// Closes stdin, as a client does at the end of a session, and returns the exit status.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        self.child.wait().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// What doc 07 §2.1 says, read from the doc.

fn doc_instructions() -> String {
    let after = DOC_07.split("**Server instructions:**").nth(1).unwrap();
    let block = after.split("```").nth(1).unwrap();
    block.trim_start_matches('\n').trim_end_matches('\n').to_string()
}

struct DocTool {
    name: String,
    description: String,
    args: Vec<(String, String)>,
}

/// The rows of the "Tools" table of doc 07 §2.1.
fn doc_tools() -> Vec<DocTool> {
    let after = DOC_07.split("**Tools:**").nth(1).unwrap();
    let mut tools = Vec::new();
    for line in after.lines().skip_while(|l| !l.starts_with("| :---")).skip(1) {
        if !line.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = line
            .trim_start_matches("| ")
            .trim_end_matches(" |")
            .split(" | ")
            .collect();
        assert_eq!(cells.len(), 3, "{line}");
        tools.push(DocTool {
            name: cells[0].trim_matches('`').to_string(),
            description: cells[1].to_string(),
            args: doc_args(cells[2]),
        });
    }
    tools
}

/// "`query`: plain words. `ns`: from `load_namespace`" becomes `[("query", "plain words"), ("ns", ...)]`.
fn doc_args(cell: &str) -> Vec<(String, String)> {
    if cell == "None" {
        return Vec::new();
    }
    let mut args = Vec::new();
    let mut rest = cell;
    loop {
        let end = rest[1..].find('`').unwrap() + 1;
        let name = &rest[1..end];
        let after = &rest[end + 3..];
        match after.find(". `") {
            Some(i) => {
                args.push((name.to_string(), after[..i].to_string()));
                rest = &after[i + 2..];
            }
            None => {
                args.push((name.to_string(), after.to_string()));
                return args;
            }
        }
    }
}

// The fixture vault.

const INDEX_BACKEND: &str = "---\nsummary: Entry point for backend notes.\n---\n# Backend\n\nStart here.\n\n## Relations\n- indexes [[backend/Auth Flow]]\n";
const AUTH_FLOW: &str = "# Auth Flow\n\nHow sessions are issued and checked.\n\n## Observations\n- [decision] Sessions expire after one hour #security\n\n## Relations\n- relates_to [[backend/Release]]\n";
const RELEASE: &str = "# Release\n\nShips on Fridays.\n\n## Relations\n- part_of [[backend/index]]\n";
const CACHING: &str = "# Caching\n\nA cache keeps answers close.\n";
const LONG: &str = "# Long\n\nSummary line.\n\n## Storage\nStorage text.\n\n## Notes\nSome notes.\n";
const BOOT_INDEX: &str = "# Bootstrap\n\nStanding instructions.\n\n## Relations\n- indexes [[Profile]]\n";
const BOOT_PROFILE: &str = "# Profile\n\nThe user likes short answers.\n";

fn vault() -> TestVault {
    TestVault::new(&[
        ("bootstrap/index.md", BOOT_INDEX),
        ("bootstrap/Profile.md", BOOT_PROFILE),
        ("backend/index.md", INDEX_BACKEND),
        ("backend/Auth Flow.md", AUTH_FLOW),
        ("backend/Release.md", RELEASE),
        ("ops/index.md", "# Ops\n\nOperations entry.\n"),
        ("ops/Caching.md", CACHING),
        ("research/Caching.md", "# Caching\n\nResearch on caching.\n"),
        ("backend/Long.md", LONG),
        ("cog/Index.md", "# Cog\n\nThe cog entry note.\n"),
        (
            "meta/index.md",
            "---\ntitle: Meta hub\n---\n\nPlain text first.\n\n## Relations\n- indexes [[backend/Release]]\n",
        ),
        ("Mixed/index.md", "# Mixed\n\nMixed case folder.\n"),
        ("Shelf/Item.md", "# Item\n\nAn item.\n"),
        ("loose/Note One.md", "# Note One\n\nFirst loose note.\n"),
        ("loose/Two.md", "# The Second\n\nSecond loose note.\n"),
        ("archive/index.md", "# Archive\n\nSession records.\n"),
        (
            "archive/2026-11-01 session.md",
            "# Session\n\nA record of caching work.\n",
        ),
    ])
}

// Initialize and the tool list (doc 07 §2.1, "Word for word").

#[test]
fn initialize_sends_the_instructions_word_for_word() {
    let v = vault();
    let mut server = v.server();
    let response = server.initialize();
    assert_eq!(response["result"]["instructions"].as_str().unwrap(), doc_instructions());
    assert_eq!(response["result"]["serverInfo"]["name"], "recognitor");
    assert!(response["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn tools_list_has_the_five_read_tools_and_no_write_tool() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["load_bootstrap", "load_namespace", "search", "context", "read_note"]
    );
}

#[test]
fn tool_descriptions_and_argument_descriptions_are_the_docs_text() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let sent = list["tools"].as_array().unwrap();
    let doc = doc_tools();
    assert_eq!(sent.len(), doc.len());
    for (tool, expected) in sent.iter().zip(&doc) {
        assert_eq!(tool["name"], expected.name.as_str());
        assert_eq!(tool["description"], expected.description.as_str(), "{}", expected.name);
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        let properties = schema["properties"].as_object().unwrap();
        let sent_args: Vec<(&String, &str)> = properties
            .iter()
            .map(|(k, v)| (k, v["description"].as_str().unwrap()))
            .collect();
        let doc_args: Vec<(&String, &str)> = expected.args.iter().map(|(k, v)| (k, v.as_str())).collect();
        assert_eq!(sent_args, doc_args, "{}", expected.name);
    }
}

#[test]
fn required_arguments_and_types_follow_the_docs() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let schema_of = |name: &str| -> Value {
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["inputSchema"]
            .clone()
    };
    assert_eq!(schema_of("load_bootstrap")["properties"], json!({}));
    assert_eq!(schema_of("load_namespace")["required"], json!(["ns"]));
    assert_eq!(schema_of("search")["required"], json!(["query"]));
    assert_eq!(schema_of("search")["properties"]["page"]["type"], "integer");
    assert_eq!(schema_of("context")["required"], json!(["note"]));
    assert_eq!(schema_of("read_note")["required"], json!(["note"]));
}

// Each tool returns the text of its CLI twin (doc 07 §12).

#[test]
fn search_returns_what_the_cli_prints() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(
        server.ok("search", json!({"query": "caching"})),
        v.cli_text(&["search", "caching"])
    );
    assert_eq!(
        server.ok("search", json!({"query": "caching", "ns": "ops"})),
        v.cli_text(&["search", "caching", "--ns", "ops"])
    );
    assert_eq!(
        server.ok("search", json!({"query": "caching", "ns": "archive", "page": 1})),
        v.cli_text(&["search", "caching", "--ns", "archive", "--page", "1"])
    );
}

#[test]
fn a_page_past_the_end_is_a_normal_result() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(
        server.ok("search", json!({"query": "caching", "page": 9})),
        v.cli_text(&["search", "caching", "--page", "9"])
    );
}

#[test]
fn context_returns_what_the_cli_prints() {
    let v = vault();
    let mut server = v.server();
    let text = server.ok("context", json!({"note": "backend/Auth Flow"}));
    assert_eq!(text, v.cli_text(&["context", "backend/Auth Flow"]));
    assert!(text.contains("backend/Release"), "{text}");
    assert_eq!(
        server.ok("context", json!({"note": "backend/index", "page": 1})),
        v.cli_text(&["context", "backend/index", "--page", "1"])
    );
}

#[test]
fn read_note_returns_what_the_cli_prints() {
    let v = vault();
    let mut server = v.server();
    let text = server.ok("read_note", json!({"note": "backend/Auth Flow"}));
    assert_eq!(text, v.cli_text(&["read", "backend/Auth Flow"]));
    assert!(text.starts_with("backend/Auth Flow  checksum: "), "{text}");
    assert_eq!(
        server.ok("read_note", json!({"note": "backend/Long", "section": "Storage"})),
        v.cli_text(&["read", "backend/Long", "--section", "Storage"])
    );
}

#[test]
fn read_note_cuts_a_long_note_at_the_configured_limit_and_takes_query() {
    let v = vault();
    let mut body = String::from("# Big\n\nSummary.\n");
    for i in 0..40 {
        body.push_str(&format!("\n## Part {i}\n{}\n", "filler words ".repeat(30)));
    }
    v.write("backend/Big.md", &body);
    let out = v
        .command()
        .args(["config", "set", "read.max_tokens", "300"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut server = v.server();
    let text = server.ok("read_note", json!({"note": "backend/Big", "query": "part"}));
    assert_eq!(text, v.cli_text(&["read", "backend/Big", "--query", "part"]));
    assert!(text.len() < body.len(), "the note should be cut");
}

#[test]
fn several_matches_come_back_as_a_normal_result() {
    let v = vault();
    let mut server = v.server();
    let text = server.ok("read_note", json!({"note": "caching"}));
    assert_eq!(text, v.cli_text(&["read", "caching"]));
    assert!(
        text.starts_with("\"caching\" matches 2 notes. Call again with one id:"),
        "{text}"
    );
    assert_eq!(
        server.ok("context", json!({"note": "caching"})),
        v.cli_text(&["context", "caching"])
    );
}

#[test]
fn load_bootstrap_returns_what_the_cli_prints() {
    let v = vault();
    let mut server = v.server();
    let text = server.ok("load_bootstrap", json!({}));
    assert_eq!(text, v.cli_text(&["bootstrap"]));
    assert!(text.contains("The user likes short answers."), "{text}");
}

#[test]
fn load_bootstrap_takes_no_arguments() {
    let v = vault();
    let mut server = v.server();
    // Arguments may be absent altogether.
    let response = server.request_raw("tools/call", json!({"name": "load_bootstrap"}));
    assert_eq!(response["result"]["isError"], json!(false), "{response}");
}

// `load_namespace` (doc 07 §3).

#[test]
fn load_namespace_returns_the_entry_note_and_the_hint_line() {
    let v = vault();
    let mut server = v.server();
    let text = server.ok("load_namespace", json!({"ns": "backend"}));
    assert!(text.contains("Start here."), "{text}");
    assert!(text.ends_with("Pass ns: \"backend\" to search.\n"), "{text}");
    // The hint is the tool's: the file never holds it.
    let file = fs::read_to_string(v.root.join("backend/index.md")).unwrap();
    assert!(!file.contains("Pass ns"));
}

#[test]
fn load_namespace_keeps_relations_strips_frontmatter_and_adds_the_title_heading() {
    let v = vault();
    let mut server = v.server();
    // The note has a level-1 heading: nothing is added, `## Relations` stays, the frontmatter goes.
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "backend"})),
        "# Backend\n\nStart here.\n\n## Relations\n- indexes [[backend/Auth Flow]]\n\nPass ns: \"backend\" to search.\n"
    );
    // No level-1 heading: `# <title>` is added above the content.
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "meta"})),
        "# Meta hub\n\nPlain text first.\n\n## Relations\n- indexes [[backend/Release]]\n\nPass ns: \"meta\" to search.\n"
    );
}

#[test]
fn load_namespace_finds_the_entry_note_whatever_its_case() {
    let v = vault();
    let mut server = v.server();
    // `cog/Index.md` is spelled with a capital; the namespace may be typed in any case.
    for ns in ["cog", "Cog", "COG"] {
        let text = server.ok("load_namespace", json!({"ns": ns}));
        assert!(text.contains("The cog entry note."), "{text}");
    }
}

#[test]
fn load_namespace_spells_the_namespace_as_its_folder_is_on_disk() {
    let v = vault();
    let mut server = v.server();
    for ns in ["mixed", "MIXED", "Mixed"] {
        let text = server.ok("load_namespace", json!({"ns": ns}));
        assert!(text.ends_with("Pass ns: \"Mixed\" to search.\n"), "{text}");
    }
    let text = server.ok("load_namespace", json!({"ns": "shelf"}));
    assert!(text.contains("No index.md in \"Shelf\". Its notes:"), "{text}");
}

#[test]
fn load_namespace_loads_the_archive_but_refuses_the_bootstrap() {
    let v = vault();
    let mut server = v.server();
    let text = server.ok("load_namespace", json!({"ns": "archive"}));
    assert!(text.contains("Session records."), "{text}");
    for ns in ["bootstrap", "Bootstrap"] {
        let text = server.err("load_namespace", json!({"ns": ns}));
        assert_eq!(
            text,
            "bootstrap is not loaded with load_namespace: call load_bootstrap for the standing instructions"
        );
    }
    let cli = v.cli(&["bootstrap"]);
    assert!(cli.status.success());
}

#[test]
fn load_namespace_without_an_index_lists_the_notes_first_line_then_notes() {
    let v = vault();
    let mut server = v.server();
    // The first line says why a list follows; each note is `- <id>`, a title is added as in doc 07 §4.2, and a
    // summary goes on the next line, indented two spaces. Sorted by id ignoring case; no closing line.
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "loose"})),
        "No index.md in \"loose\". Its notes:\n- loose/Note One\n  First loose note.\n- loose/Two, titled \"The Second\"\n  Second loose note.\n"
    );
    assert!(!server.ok("load_namespace", json!({"ns": "shelf"})).contains("Pass ns"));
}

#[test]
fn a_listed_note_without_a_summary_has_no_second_line() {
    let v = TestVault::new(&[
        ("Shelf/Bare.md", "# Bare\n\n## Part\ntext\n"),
        ("Shelf/Item.md", "# Item\n\nAn item.\n"),
    ]);
    let mut server = v.server();
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "Shelf"})),
        "No index.md in \"Shelf\". Its notes:\n- Shelf/Bare\n- Shelf/Item\n  An item.\n"
    );
}

/// A vault whose `Shelf` folder has `n` notes named `N01`, `N02`, ..., each with a summary.
fn shelf(n: usize) -> TestVault {
    let files: Vec<(String, String)> = (1..=n)
        .map(|i| (format!("Shelf/N{i:02}.md"), format!("# N{i:02}\n\nSummary of {i}.\n")))
        .collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    TestVault::new(&refs)
}

fn shelf_lines(count: usize) -> String {
    (1..=count)
        .map(|i| format!("- Shelf/N{i:02}\n  Summary of {i}.\n"))
        .collect()
}

#[test]
fn exactly_ten_notes_have_no_closing_line() {
    let v = shelf(10);
    let mut server = v.server();
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "shelf"})),
        format!("No index.md in \"Shelf\". Its notes:\n{}", shelf_lines(10))
    );
}

#[test]
fn more_than_ten_notes_stop_at_ten_and_close_with_a_search_hint() {
    let v = shelf(11);
    let mut server = v.server();
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "shelf"})),
        format!(
            "No index.md in \"Shelf\". Its notes:\n{}1 more. Search with ns: \"Shelf\" to find them.\n",
            shelf_lines(10)
        )
    );
    let v = shelf(22);
    let mut server = v.server();
    let text = server.ok("load_namespace", json!({"ns": "SHELF"}));
    assert!(
        text.ends_with("12 more. Search with ns: \"Shelf\" to find them.\n"),
        "{text}"
    );
    assert_eq!(text.matches("\n- ").count(), 10, "{text}");
}

#[test]
fn listed_notes_are_sorted_by_id_ignoring_case() {
    let v = TestVault::new(&[
        ("Shelf/b.md", "# b\n"),
        ("Shelf/A.md", "# A\n"),
        ("Shelf/C.md", "# C\n"),
    ]);
    let mut server = v.server();
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "shelf"})),
        "No index.md in \"Shelf\". Its notes:\n- Shelf/A\n- Shelf/b\n- Shelf/C\n"
    );
}

#[test]
fn load_namespace_needs_ns() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(server.err("load_namespace", json!({})), "load_namespace needs ns");
    assert_eq!(
        server.err("load_namespace", json!({"ns": null})),
        "load_namespace needs ns"
    );
}

// Errors (doc 07 §2.1, "Errors").

#[test]
fn a_note_that_is_not_found_is_an_error_with_the_cli_message() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(server.err("read_note", json!({"note": "x"})), "\"x\" not found");
    assert_eq!(server.err("context", json!({"note": "x"})), "\"x\" not found");
    let cli = v.cli(&["read", "x"]);
    assert_eq!(cli.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&cli.stderr), "error: \"x\" not found\n");
}

#[test]
fn a_query_with_no_words_is_an_error() {
    let v = vault();
    let mut server = v.server();
    let text = server.err("search", json!({"query": "+ - *"}));
    let cli = v.cli(&["search", "+ - *"]);
    assert_eq!(format!("error: {text}\n"), String::from_utf8_lossy(&cli.stderr));
}

#[test]
fn no_such_section_is_an_error_with_the_cli_message() {
    let v = vault();
    let mut server = v.server();
    let text = server.err("read_note", json!({"note": "backend/Long", "section": "Nope"}));
    let cli = v.cli(&["read", "backend/Long", "--section", "Nope"]);
    assert_eq!(format!("error: {text}\n"), String::from_utf8_lossy(&cli.stderr));
    assert!(text.starts_with("No section \"Nope\" in backend/Long."), "{text}");
}

// Argument messages (doc 07 §2.1): `<tool> needs <what>` for a missing argument; `<argument> needs text` or
// `page needs a number from 1` for a wrong type.

#[test]
fn a_missing_query_is_an_error_and_not_a_protocol_error() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(server.err("search", json!({})), "search needs a query");
    // No arguments at all is the same.
    let response = server.request_raw("tools/call", json!({"name": "search"}));
    assert_eq!(response["result"]["isError"], json!(true), "{response}");
    assert_eq!(response["result"]["content"][0]["text"], "search needs a query");
    assert!(response.get("error").is_none(), "{response}");
}

#[test]
fn a_missing_note_is_an_error() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(server.err("context", json!({})), "context needs a note");
    assert_eq!(server.err("read_note", json!({})), "read_note needs a note");
}

#[test]
fn a_page_that_is_not_a_number_from_1_is_an_error() {
    let v = vault();
    let mut server = v.server();
    for bad in [json!("two"), json!("2"), json!(0), json!(-1), json!(1.5), json!(true)] {
        assert_eq!(
            server.err("search", json!({"query": "caching", "page": bad})),
            "page needs a number from 1"
        );
        assert_eq!(
            server.err("context", json!({"note": "backend/index", "page": bad})),
            "page needs a number from 1"
        );
    }
}

#[test]
fn arguments_of_the_wrong_type_say_what_they_need() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(server.err("search", json!({"query": 5})), "query needs text");
    assert_eq!(
        server.err("search", json!({"query": "caching", "ns": 5})),
        "ns needs text"
    );
    assert_eq!(server.err("context", json!({"note": ["a"]})), "note needs text");
    assert_eq!(
        server.err("read_note", json!({"note": "backend/Long", "section": 1})),
        "section needs text"
    );
}

#[test]
fn arguments_the_tool_does_not_take_are_errors() {
    let v = vault();
    let mut server = v.server();
    assert_eq!(
        server.err("search", json!({"query": "caching", "limit": 5})),
        "unexpected argument: limit"
    );
    // A misspelled argument must not pass for the default.
    assert_eq!(
        server.err("search", json!({"query": "caching", "pages": 2})),
        "unexpected argument: pages"
    );
    assert_eq!(
        server.err("load_bootstrap", json!({"ns": "x"})),
        "unexpected argument: ns"
    );
}

#[test]
fn no_hits_is_a_normal_result() {
    let v = vault();
    let mut server = v.server();
    let (text, is_error) = server.call("search", json!({"query": "zzzzunmatched"}));
    assert!(!is_error, "{text}");
    assert_eq!(text, v.cli_text(&["search", "zzzzunmatched"]));
}

#[test]
fn a_vault_without_a_bootstrap_is_an_error_without_the_prefix() {
    let v = TestVault::new(&[("backend/Release.md", RELEASE)]);
    let mut server = v.server();
    assert_eq!(
        server.err("load_bootstrap", json!({})),
        "bootstrap/index.md not found. Run `recognitor lint`."
    );
}

#[test]
fn an_unknown_tool_is_a_protocol_error() {
    let v = vault();
    let mut server = v.server();
    let response = server.request_raw("tools/call", json!({"name": "write_note", "arguments": {}}));
    // Doc 07 §2.1, "Unknown tool": a JSON-RPC error, -32602, and no tool result.
    assert_eq!(response["error"]["code"], json!(-32602), "{response}");
    assert!(response.get("result").is_none(), "{response}");
}

// The sweep before each read (doc 02 §4) and the process.

#[test]
fn every_call_sweeps_when_the_gate_has_expired() {
    let v = vault();
    let mut server = v.server();
    // The first call sweeps and opens the gate; a file added now stays unseen until the gate expires.
    server.ok("search", json!({"query": "zebra"}));
    v.write("backend/Zebra.md", "# Zebra\n\nStripes.\n");
    thread::sleep(Duration::from_millis(2200));
    let text = server.ok("search", json!({"query": "zebra"}));
    assert!(text.contains("backend/Zebra"), "{text}");
    let text = server.ok("read_note", json!({"note": "backend/Zebra"}));
    assert!(text.contains("Stripes."), "{text}");
}

#[test]
fn the_server_exits_cleanly_when_stdin_closes_and_writes_nothing_else_to_stdout() {
    let v = vault();
    let mut server = v.server();
    server.ok("search", json!({"query": "caching"}));
    let status = server.finish();
    assert!(status.success());
}

#[test]
fn a_missing_vault_fails_before_serving_and_prints_nothing_to_stdout() {
    let v = vault();
    let out = v.command().arg("mcp").output().unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: no vault given"));
}

#[test]
fn the_server_never_writes_to_the_vault() {
    let v = vault();
    let snapshot = |root: &PathBuf| -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let bytes = fs::read(&path).unwrap();
                    files.push((path, bytes));
                }
            }
        }
        files.sort();
        files
    };
    let before = snapshot(&v.root);
    let mut server = v.server();
    server.ok("load_bootstrap", json!({}));
    server.ok("search", json!({"query": "caching"}));
    server.ok("context", json!({"note": "backend/Auth Flow"}));
    server.ok("read_note", json!({"note": "backend/Auth Flow"}));
    drop(server);
    assert_eq!(snapshot(&v.root), before);
}

// Two servers on one database (doc 05 §4.1, doc 02 §4).

#[test]
fn two_servers_on_one_database_answer_at_the_same_time_without_errors() {
    let v = vault();
    let expected = v.cli_text(&["search", "caching"]);
    let mut handles = Vec::new();
    let mut servers = vec![v.server(), v.server()];
    for mut server in servers.drain(..) {
        let expected = expected.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..25 {
                assert_eq!(server.ok("search", json!({"query": "caching"})), expected);
                thread::sleep(Duration::from_millis(5));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

// "No such namespace" (doc 07 §3): an error that lists the namespaces that exist.

#[test]
fn load_namespace_of_a_namespace_with_no_notes_lists_the_namespaces() {
    let v = vault();
    let mut server = v.server();
    // Every namespace but the bootstrap, as the folders are spelled on disk, sorted by name ignoring case.
    assert_eq!(
        server.err("load_namespace", json!({"ns": "nope"})),
        "No namespace \"nope\". Namespaces: \"archive\", \"backend\", \"cog\", \"loose\", \"meta\", \"Mixed\", \
         \"ops\", \"research\", \"Shelf\"."
    );
}

#[test]
fn load_namespace_with_nothing_to_list_ends_after_the_first_sentence() {
    // Only the bootstrap exists, and it is never listed.
    let v = TestVault::new(&[("bootstrap/index.md", BOOT_INDEX)]);
    let mut server = v.server();
    assert_eq!(
        server.err("load_namespace", json!({"ns": "nope"})),
        "No namespace \"nope\"."
    );
    let empty = TestVault::new(&[]);
    let mut server = empty.server();
    assert_eq!(
        server.err("load_namespace", json!({"ns": "Nope"})),
        "No namespace \"Nope\"."
    );
}

// "No nested namespaces" (doc 07 §3): a name with a `/` is no namespace, even when a nested index.md exists.

#[test]
fn load_namespace_of_a_name_with_a_slash_is_the_no_namespace_error() {
    let v = TestVault::new(&[
        ("backend/index.md", INDEX_BACKEND),
        ("backend/guides/index.md", "# Guides\n\nNested entry note.\n"),
        ("bootstrap/index.md", BOOT_INDEX),
    ]);
    let mut server = v.server();
    for ns in ["backend/guides", "backend/", "/backend", "bootstrap/x"] {
        assert_eq!(
            server.err("load_namespace", json!({"ns": ns})),
            format!("No namespace \"{ns}\". Namespaces: \"backend\".")
        );
    }
    // The nested note itself still reads.
    assert!(
        server
            .ok("read_note", json!({"note": "backend/guides/index"}))
            .contains("Nested entry note.")
    );
    // And the namespace itself still loads.
    assert!(
        server
            .ok("load_namespace", json!({"ns": "backend"}))
            .contains("Start here.")
    );
}

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

// A test input: this path must be in the `.code-paths` list of .gitlab-ci.yml. tests/integration/ci_paths.rs enforces it.
const DOC_07: &str = include_str!("../../docs/07-interface-contracts.md");

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
    block.replace("\r\n", "\n").trim_matches('\n').to_string()
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

const INDEX_BACKEND: &str = "---\nsummary: Entry point for backend notes.\n---\n# Backend\n\nStart here.\n\n## Relations\n- indexes [[Auth Flow]]\n";
const AUTH_FLOW: &str = "# Auth Flow\n\nHow sessions are issued and checked.\n\n## Observations\n- [decision] Sessions expire after one hour #security\n\n## Relations\n- relates_to [[Release]]\n";
const RELEASE: &str = "# Release\n\nShips on Fridays.\n\n## Relations\n- part_of [[index]]\n";
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
            "---\ntitle: Meta hub\n---\n\nPlain text first.\n\n## Relations\n- indexes [[backend::Release]]\n",
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
fn tools_list_has_the_read_tools_and_the_write_tools() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let tools = list["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "load_bootstrap",
            "load_namespace",
            "search",
            "context",
            "read_note",
            "write_note",
            "edit_note",
            "move_note",
            "relink",
            "delete_note"
        ]
    );
    // Doc 07 §2.1: the five reads send readOnlyHint; the writes send no annotations.
    for tool in tools.iter().take(5) {
        assert_eq!(
            tool["annotations"]["readOnlyHint"], true,
            "{} should be read-only",
            tool["name"]
        );
    }
    for tool in tools.iter().skip(5) {
        assert!(
            tool.get("annotations").is_none() || tool["annotations"].is_null(),
            "{} should have no annotations",
            tool["name"]
        );
    }
}

// `relink` (doc 07 §6.1): the tool and its CLI twin return the same text.

#[test]
fn relink_rewrites_over_mcp_and_checks_its_arguments() {
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("ops/Runbook.md", "# Runbook\n\nSee [[ideas::Caching]].\n"),
    ]);
    let mut server = v.server();
    let list = server.list_tools();
    let schema = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "relink")
        .unwrap()["inputSchema"]
        .clone();
    assert_eq!(schema["required"], json!(["old", "new", "notes"]));
    assert_eq!(schema["properties"]["notes"]["type"], "array");
    assert_eq!(schema["properties"]["notes"]["items"]["type"], "string");
    assert_eq!(server.err("relink", json!({})), "relink needs old");
    assert_eq!(
        server.err(
            "relink",
            json!({"old": "ideas/Caching", "new": "design/Caching", "notes": "ops/Runbook"})
        ),
        "notes needs a list of text"
    );
    assert_eq!(
        server.ok(
            "relink",
            json!({"old": "ideas/Caching", "new": "design/Caching", "notes": ["ops/Runbook"]})
        ),
        "relinked: 1 link in 1 note\n"
    );
    assert_eq!(
        fs::read_to_string(v.root.join("ops/Runbook.md")).unwrap(),
        "# Runbook\n\nSee [[design::Caching]].\n"
    );
    drop(server);
    assert_eq!(
        v.cli_text(&["relink", "ideas/Caching", "design/Caching", "ops/Runbook"]),
        "relinked: 0 links in 0 notes\nwarning: skipped, link already gone: ops/Runbook\n"
    );
    let usage = v.cli(&["relink", "ideas/Caching", "design/Caching"]);
    assert_eq!(usage.status.code(), Some(2));
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

// `write_note` (doc 07 §11): MCP only, the CLI has no write command (§12).

#[test]
fn write_note_creates_overwrites_and_refuses_over_mcp() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let schema = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "write_note")
        .unwrap()["inputSchema"]
        .clone();
    assert_eq!(schema["required"], json!(["path", "content"]));
    assert_eq!(
        schema["properties"].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["path", "content", "expected_checksum"]
    );
    assert_eq!(server.err("write_note", json!({})), "write_note needs a path");
    assert_eq!(
        server.err("write_note", json!({"path": "ops/New"})),
        "write_note needs content"
    );
    assert_eq!(
        server.err("write_note", json!({"path": "ops/New", "content": "x", "page": 2})),
        "unexpected argument: page"
    );
    assert_eq!(
        server.err("write_note", json!({"path": "ops/New", "content": 7})),
        "content needs text"
    );
    assert_eq!(
        server.err(
            "write_note",
            json!({"path": "ops/New", "content": "  
"})
        ),
        "write_note needs content"
    );
    assert_eq!(
        server.err("write_note", json!({"path": "ops/New", "content": ""})),
        "write_note needs content"
    );
    assert!(!v.root.join("ops/New.md").exists());

    let note = "# Brand New

A zebra note.
";
    let sum = recognitor::hash::sha256_prefix(note.as_bytes(), 12);
    // Create: `Ops/` lands in the existing `ops/`.
    assert_eq!(
        server.ok("write_note", json!({"path": "Ops/Brand New.md", "content": note})),
        format!(
            "ops/Brand New  checksum: {sum}
"
        )
    );
    assert_eq!(fs::read_to_string(v.root.join("ops/Brand New.md")).unwrap(), note);
    // The same call again is a create on an existing note: an error result with the note in it.
    let refused = server.err(
        "write_note",
        json!({"path": "ops/Brand New", "content": "# Other
"}),
    );
    assert!(refused.starts_with("The note already exists"), "{refused}");
    assert!(
        refused.contains(&format!(
            "ops/Brand New  checksum: {sum}
{note}"
        )),
        "{refused}"
    );
    // Readback and search over the same server see it at once.
    assert!(
        server
            .ok("read_note", json!({"note": "ops/Brand New"}))
            .starts_with(&format!(
                "ops/Brand New  checksum: {sum}
"
            ))
    );
    assert!(
        server
            .ok("search", json!({"query": "zebra"}))
            .starts_with("1. ops/Brand New")
    );
    // Overwrite with the checksum; a stale one is refused.
    let newer = "# Brand New

Now a giraffe note.
";
    let newer_sum = recognitor::hash::sha256_prefix(newer.as_bytes(), 12);
    assert_eq!(
        server.ok(
            "write_note",
            json!({"path": "ops/Brand New", "content": newer, "expected_checksum": sum})
        ),
        format!(
            "ops/Brand New  checksum: {newer_sum}
"
        )
    );
    let stale = server.err(
        "write_note",
        json!({"path": "ops/Brand New", "content": "# X
", "expected_checksum": sum}),
    );
    assert!(stale.starts_with("The note changed since that checksum"), "{stale}");
    assert!(
        stale.contains(&format!(
            "ops/Brand New  checksum: {newer_sum}
{newer}"
        )),
        "{stale}"
    );
    // Missing note with a checksum, the bootstrap notice, the archive.
    assert_eq!(
        server.err(
            "write_note",
            json!({"path": "ops/Nope", "content": "x", "expected_checksum": "000000000000"})
        ),
        "\"ops/Nope\" not found"
    );
    assert_eq!(
        server.ok(
            "write_note",
            json!({"path": "bootstrap/Extra", "content": "# Extra
"})
        ),
        format!(
            "bootstrap/Extra  checksum: {}
notice: bootstrap/Extra loads as instructions at session start
",
            recognitor::hash::sha256_prefix(
                b"# Extra
",
                12
            )
        )
    );
    assert!(
        server
            .err(
                "write_note",
                json!({"path": "archive/2026-11-01 session", "content": "x", "expected_checksum": "000000000000"})
            )
            .starts_with("Archive notes are append only: write_note never overwrites one. To add to it, use edit_note with append.")
    );
    assert!(
        server
            .err("write_note", json!({"path": "top.md", "content": "x"}))
            .contains("no top-level folder")
    );
    drop(server);
    // The CLI has no write command (doc 07 §12): its text is a bad argument.
    assert_eq!(v.cli(&["write_note", "ops/X", "x"]).status.code(), Some(2));
    assert_eq!(v.cli(&["write", "ops/X", "x"]).status.code(), Some(2));
}

// `edit_note` (doc 07 §9): MCP only, the CLI has no edit command (§12).

#[test]
fn edit_note_checks_its_arguments_and_runs_every_op_over_mcp() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let schema = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "edit_note")
        .unwrap()["inputSchema"]
        .clone();
    assert_eq!(schema["required"], json!(["note", "op"]));
    assert_eq!(
        schema["properties"].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["note", "op", "text", "section", "find", "expected_checksum"]
    );
    // Missing arguments follow `<tool> needs <what>`; an argument the op does not take is unexpected.
    assert_eq!(server.err("edit_note", json!({})), "edit_note needs a note");
    assert_eq!(
        server.err("edit_note", json!({"note": "ops/Caching"})),
        "edit_note needs an op"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "prepend", "text": "x"})
        ),
        "op needs one of append, add_observation, add_relation, find_replace, replace_section"
    );
    assert_eq!(
        server.err("edit_note", json!({"note": "ops/Caching", "op": "append"})),
        "edit_note needs text"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "append", "text": "  \n"})
        ),
        "edit_note needs text"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "find_replace", "text": "x"})
        ),
        "edit_note needs find"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "find_replace", "find": "x"})
        ),
        "edit_note needs text"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "replace_section", "text": ""})
        ),
        "edit_note needs section"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "replace_section", "section": "A", "text": ""})
        ),
        "edit_note needs expected_checksum"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "append", "text": "x", "find": "y"})
        ),
        "unexpected argument: find"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "add_observation", "text": "[fact] x", "section": "A"})
        ),
        "unexpected argument: section"
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "ops/Caching", "op": "append", "text": "x", "page": 2})
        ),
        "unexpected argument: page"
    );
    assert_eq!(
        server.err("edit_note", json!({"note": "ops/Caching", "op": "append", "text": 5})),
        "text needs text"
    );
    // Empty strings in arguments an op does not take count as not given.
    let note = "# Caching\n\nA cache keeps answers close.\n";
    let mut expected = format!("{note}\nMore.\n");
    assert_eq!(
        server.ok(
            "edit_note",
            json!({"note": "ops/Caching", "op": "append", "text": "More.", "find": "", "section": "", "expected_checksum": ""})
        ),
        format!("ops/Caching  checksum: {}\n", recognitor::hash::sha256_prefix(expected.as_bytes(), 12))
    );
    assert_eq!(fs::read_to_string(v.root.join("ops/Caching.md")).unwrap(), expected);

    // Each op, with a refusal in the same call as an error result.
    expected.push_str("\n## Observations\n- [fact] Fast.\n");
    server.ok(
        "edit_note",
        json!({"note": "ops/Caching", "op": "add_observation", "text": "[fact] Fast."}),
    );
    assert_eq!(fs::read_to_string(v.root.join("ops/Caching.md")).unwrap(), expected);
    let already = server.ok(
        "edit_note",
        json!({"note": "ops/Caching", "op": "add_observation", "text": "[fact] Fast."}),
    );
    assert!(
        already.starts_with("Already there, nothing was written.\nops/Caching  checksum: "),
        "{already}"
    );
    let bad = server.err(
        "edit_note",
        json!({"note": "ops/Caching", "op": "add_relation", "text": "see also [[X]]"}),
    );
    assert!(
        bad.starts_with("That is not a typed relation line, so nothing was written."),
        "{bad}"
    );
    server.ok(
        "edit_note",
        json!({"note": "ops/Caching", "op": "add_relation", "text": "relates_to [[Release]]"}),
    );
    server.ok(
        "edit_note",
        json!({"note": "ops/Caching", "op": "find_replace", "find": "close.", "text": "near."}),
    );
    let refused = server.err(
        "edit_note",
        json!({"note": "ops/Caching", "op": "find_replace", "find": "zzz", "text": "y"}),
    );
    assert!(
        refused.starts_with(
            "The text to replace was not found, so nothing was written. Current version:\nops/Caching  checksum: "
        ),
        "{refused}"
    );
    // The section checksum comes from read_note and goes back in.
    let read = server.ok("read_note", json!({"note": "ops/Caching", "section": "Observations"}));
    let checksum = read
        .split("checksum: ")
        .nth(1)
        .unwrap()
        .split('\n')
        .next()
        .unwrap()
        .to_string();
    let stale = server.err("edit_note", json!({"note": "ops/Caching", "op": "replace_section", "section": "Observations", "expected_checksum": "000000000000", "text": "## Observations
- [fact] x"}));
    assert!(stale.starts_with("The section changed since that checksum, so nothing was written. Current version:\nops/Caching > Observations  checksum: "), "{stale}");
    server.ok("edit_note", json!({"note": "ops/Caching", "op": "replace_section", "section": "Observations", "expected_checksum": checksum, "text": "## Observations\n- [fact] Faster."}));
    let file = fs::read_to_string(v.root.join("ops/Caching.md")).unwrap();
    assert!(
        file.contains("- [fact] Faster.\n") && !file.contains("- [fact] Fast.\n"),
        "{file}"
    );
    // Several matches, several notes, no note.
    let several = server.ok("edit_note", json!({"note": "Caching", "op": "append", "text": "x"}));
    assert!(
        several.starts_with("\"Caching\" matches 2 notes. Call again with one id:\n"),
        "{several}"
    );
    assert_eq!(
        server.err("edit_note", json!({"note": "ops/Nope", "op": "append", "text": "x"})),
        "\"ops/Nope\" not found"
    );
    // The bootstrap notice and the archive.
    let boot = server.ok(
        "edit_note",
        json!({"note": "bootstrap/Profile", "op": "append", "text": "More."}),
    );
    assert!(
        boot.ends_with("notice: bootstrap/Profile loads as instructions at session start\n"),
        "{boot}"
    );
    assert!(
        server
            .ok(
                "edit_note",
                json!({"note": "archive/2026-11-01 session", "op": "append", "text": "Later."})
            )
            .starts_with("archive/2026-11-01 session  checksum: ")
    );
    assert_eq!(
        server.err(
            "edit_note",
            json!({"note": "archive/2026-11-01 session", "op": "find_replace", "find": "caching", "text": "x"})
        ),
        "Archive notes are append only: use append to add to one."
    );
    drop(server);
    // The CLI has no edit command (doc 07 §12): a bad argument.
    assert_eq!(v.cli(&["edit_note", "ops/Caching"]).status.code(), Some(2));
    assert_eq!(v.cli(&["edit", "ops/Caching"]).status.code(), Some(2));
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
        "# Backend\n\nStart here.\n\n## Relations\n- indexes [[Auth Flow]]\n\nPass ns: \"backend\" to search.\n"
    );
    // No level-1 heading: `# <title>` is added above the content.
    assert_eq!(
        server.ok("load_namespace", json!({"ns": "meta"})),
        "# Meta hub\n\nPlain text first.\n\n## Relations\n- indexes [[backend::Release]]\n\nPass ns: \"meta\" to search.\n"
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
    let response = server.request_raw("tools/call", json!({"name": "no_such_tool", "arguments": {}}));
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

// `delete_note` (doc 07 §10): MCP only, the CLI has no delete command (§12).

/// The checksum in the header line of a `read_note` result.
fn checksum_of(read: &str) -> String {
    read.lines()
        .next()
        .unwrap()
        .rsplit_once("checksum: ")
        .unwrap()
        .1
        .to_string()
}

#[test]
fn delete_note_checks_its_arguments_over_mcp() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let schema = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "delete_note")
        .unwrap()["inputSchema"]
        .clone();
    assert_eq!(schema["required"], json!(["note", "expected_checksum"]));
    assert_eq!(server.err("delete_note", json!({})), "delete_note needs a note");
    assert_eq!(
        server.err("delete_note", json!({"note": "backend/Release"})),
        "delete_note needs expected_checksum"
    );
    // An empty string counts as not given (doc 07 §2.1).
    assert_eq!(
        server.err(
            "delete_note",
            json!({"note": "backend/Release", "expected_checksum": ""})
        ),
        "delete_note needs expected_checksum"
    );
    assert_eq!(
        server.err(
            "delete_note",
            json!({"note": "backend/Release", "expected_checksum": 5})
        ),
        "expected_checksum needs text"
    );
    assert_eq!(
        server.err(
            "delete_note",
            json!({"note": "backend/Release", "expected_checksum": "abc", "section": "x"})
        ),
        "unexpected argument: section"
    );
    // Nothing above deleted anything.
    assert!(v.root.join("backend/Release.md").exists());
}

#[test]
fn delete_note_trashes_a_note_and_lists_the_broken_links_over_mcp() {
    let v = vault();
    let mut server = v.server();
    let checksum = checksum_of(&server.ok("read_note", json!({"note": "backend/Release"})));
    // A stale checksum is an error that returns the current note, and nothing moves.
    let stale = server.err(
        "delete_note",
        json!({"note": "backend/Release", "expected_checksum": "000000000000"}),
    );
    assert_eq!(
        stale,
        format!(
            "The note changed since that checksum, so nothing was deleted. Current version:\n\
             backend/Release  checksum: {checksum}\n{RELEASE}"
        )
    );
    assert!(v.root.join("backend/Release.md").exists());
    // The right one moves it; the checksum is matched ignoring case, and the note by its name.
    let text = server.ok(
        "delete_note",
        json!({"note": "backend::Release", "expected_checksum": checksum.to_uppercase()}),
    );
    let (head, links) = text.split_once("broken links: ").unwrap();
    let mut head = head.lines();
    assert_eq!(head.next(), Some("deleted: backend/Release"));
    let trash = head.next().unwrap().strip_prefix("trash: .recognitor/trash/").unwrap();
    assert!(trash.ends_with("Z/backend/Release.md"), "{trash}");
    assert_eq!(
        links,
        "2 links in 2 notes\n\
         - backend/Auth Flow: relates_to Release\n\
         - meta/index, titled \"Meta hub\": indexes backend::Release\n"
    );
    assert!(!v.root.join("backend/Release.md").exists());
    assert_eq!(
        fs::read_to_string(v.root.join(".recognitor/trash").join(trash)).unwrap(),
        RELEASE
    );
    // The note is gone for every tool, and a second delete is "not found".
    assert_eq!(
        server.err("read_note", json!({"note": "backend/Release"})),
        "\"backend/Release\" not found"
    );
    assert_eq!(
        server.err(
            "delete_note",
            json!({"note": "backend/Release", "expected_checksum": checksum})
        ),
        "\"backend/Release\" not found"
    );
}

#[test]
fn delete_note_refuses_the_archive_and_asks_which_of_several_and_carries_the_bootstrap_notice() {
    let v = vault();
    let mut server = v.server();
    // The archive.
    let record = server.ok("read_note", json!({"note": "archive/2026-11-01 session"}));
    assert_eq!(
        server.err(
            "delete_note",
            json!({"note": "archive/2026-11-01 session", "expected_checksum": checksum_of(&record)})
        ),
        "Archive notes are append only: delete_note never removes one. Only the user can delete the file."
    );
    assert!(v.root.join("archive/2026-11-01 session.md").exists());
    // Several matches are a normal result, and nothing moves.
    assert_eq!(
        server.ok(
            "delete_note",
            json!({"note": "Caching", "expected_checksum": "000000000000"})
        ),
        "\"Caching\" matches 2 notes. Call again with one id:\n  ops/Caching\n  research/Caching\n"
    );
    assert!(v.root.join("ops/Caching.md").exists() && v.root.join("research/Caching.md").exists());
    // A bootstrap note.
    let profile = server.ok("read_note", json!({"note": "bootstrap/Profile"}));
    let text = server.ok(
        "delete_note",
        json!({"note": "bootstrap/Profile", "expected_checksum": checksum_of(&profile)}),
    );
    assert!(
        text.starts_with("deleted: bootstrap/Profile\ntrash: .recognitor/trash/"),
        "{text}"
    );
    assert!(
        text.ends_with(
            "broken links: 1 link in 1 note\n\
             - bootstrap/index, titled \"Bootstrap\": indexes Profile\n\
             notice: bootstrap/Profile no longer loads as instructions at session start\n"
        ),
        "{text}"
    );
}

// `move_note` (doc 07 §6.1, §11): MCP only in this slice; the CLI `move` is a later slice.

#[test]
fn move_note_checks_its_arguments_over_mcp() {
    let v = vault();
    let mut server = v.server();
    let list = server.list_tools();
    let schema = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "move_note")
        .unwrap()["inputSchema"]
        .clone();
    assert_eq!(schema["required"], json!(["note", "new_path", "expected_checksum"]));
    assert_eq!(server.err("move_note", json!({})), "move_note needs a note");
    assert_eq!(
        server.err("move_note", json!({"note": "backend/Release"})),
        "move_note needs new_path"
    );
    assert_eq!(
        server.err(
            "move_note",
            json!({"note": "backend/Release", "new_path": "ops/Release"})
        ),
        "move_note needs expected_checksum"
    );
    // An empty string counts as not given (doc 07 §2.1), for the path as for the checksum.
    assert_eq!(
        server.err(
            "move_note",
            json!({"note": "backend/Release", "new_path": "", "expected_checksum": "abc"})
        ),
        "move_note needs new_path"
    );
    assert_eq!(
        server.err(
            "move_note",
            json!({"note": "backend/Release", "new_path": "ops/Release", "expected_checksum": ""})
        ),
        "move_note needs expected_checksum"
    );
    assert_eq!(
        server.err(
            "move_note",
            json!({"note": "backend/Release", "new_path": 5, "expected_checksum": "abc"})
        ),
        "new_path needs text"
    );
    assert_eq!(
        server.err(
            "move_note",
            json!({"note": "backend/Release", "new_path": "ops/Release", "expected_checksum": "abc", "section": "x"})
        ),
        "unexpected argument: section"
    );
    // Nothing above moved anything.
    assert!(v.root.join("backend/Release.md").exists());
    assert!(!v.root.join("ops/Release.md").exists());
}

#[test]
fn move_note_moves_a_note_and_lists_the_broken_links_over_mcp() {
    let v = vault();
    let mut server = v.server();
    let checksum = checksum_of(&server.ok("read_note", json!({"note": "backend/Release"})));
    // A stale checksum is an error that returns the current note, and nothing moves.
    let stale = server.err(
        "move_note",
        json!({"note": "backend/Release", "new_path": "ops/Release", "expected_checksum": "000000000000"}),
    );
    assert_eq!(
        stale,
        format!(
            "The note changed since that checksum, so nothing was moved. Current version:\n\
             backend/Release  checksum: {checksum}\n{RELEASE}"
        )
    );
    assert!(v.root.join("backend/Release.md").exists());
    // The right one moves it. `meta/index` links `backend::Release`, which names the old namespace; `backend/Auth Flow`
    // links `Release` by name, which still reaches it (step 4).
    let text = server.ok(
        "move_note",
        json!({"note": "backend::Release", "new_path": "Ops/archive-me/Release.md", "expected_checksum": checksum.to_uppercase()}),
    );
    assert_eq!(
        text,
        "moved: backend/Release -> ops/archive-me/Release\n\
         broken links: 2 links in 2 notes\n\
         - meta/index, titled \"Meta hub\": indexes backend::Release\n\
         - ops/archive-me/Release: part_of index  (now ops/index)\n\
         relink(old: \"backend/Release\", new: \"ops/archive-me/Release\", notes: [\"meta/index\"])\n"
    );
    assert!(!v.root.join("backend/Release.md").exists());
    assert_eq!(
        fs::read_to_string(v.root.join("ops/archive-me/Release.md")).unwrap(),
        RELEASE
    );
    // The old id is gone for every tool and the new one reads.
    assert_eq!(
        server.err("read_note", json!({"note": "backend/Release"})),
        "\"backend/Release\" not found"
    );
    assert!(
        server
            .ok("read_note", json!({"note": "ops/archive-me/Release"}))
            .starts_with("ops/archive-me/Release  checksum: ")
    );
    // The printed call works as written, over the same server.
    let relinked = server.ok(
        "relink",
        json!({"old": "backend/Release", "new": "ops/archive-me/Release", "notes": ["meta/index"]}),
    );
    assert_eq!(relinked, "relinked: 1 link in 1 note\n");
    assert!(
        fs::read_to_string(v.root.join("meta/index.md"))
            .unwrap()
            .contains("indexes [[ops::Release]]")
    );
}

#[test]
fn move_note_refuses_the_archive_a_taken_path_and_asks_which_of_several() {
    let v = vault();
    let mut server = v.server();
    // Out of the archive.
    let record = server.ok("read_note", json!({"note": "archive/2026-11-01 session"}));
    assert_eq!(
        server.err(
            "move_note",
            json!({"note": "archive/2026-11-01 session", "new_path": "ops/Session", "expected_checksum": checksum_of(&record)})
        ),
        "Archive notes are append only: move_note never moves one. Only the user can move the file."
    );
    assert!(v.root.join("archive/2026-11-01 session.md").exists());
    // Onto another note: that note comes back.
    let release = server.ok("read_note", json!({"note": "backend/Release"}));
    let taken = server.err(
        "move_note",
        json!({"note": "backend/Release", "new_path": "ops/Caching", "expected_checksum": checksum_of(&release)}),
    );
    assert!(
        taken.starts_with(
            "A note already exists at that path, so nothing was moved. Current version:\nops/Caching  checksum: "
        ),
        "{taken}"
    );
    // Several matches are a normal result, and nothing moves.
    assert_eq!(
        server.ok(
            "move_note",
            json!({"note": "Caching", "new_path": "ops/Other", "expected_checksum": "000000000000"})
        ),
        "\"Caching\" matches 2 notes. Call again with one id:\n  ops/Caching\n  research/Caching\n"
    );
    assert!(v.root.join("ops/Caching.md").exists() && v.root.join("research/Caching.md").exists());
}

#[test]
fn move_note_into_and_out_of_the_bootstrap_carries_its_notice_and_a_long_note_moves() {
    let v = vault();
    // A long note, written before the server starts so that its first sweep indexes it.
    let long = format!(
        "# Big\n\nSummary.\n{}",
        "\n## Part\n\nword word word word\n".repeat(2000)
    );
    v.write("loose/Big.md", &long);
    let mut server = v.server();
    let caching = server.ok("read_note", json!({"note": "ops/Caching"}));
    let text = server.ok(
        "move_note",
        json!({"note": "ops/Caching", "new_path": "bootstrap/Caching", "expected_checksum": checksum_of(&caching)}),
    );
    assert!(
        text.ends_with("notice: bootstrap/Caching loads as instructions at session start\n"),
        "{text}"
    );
    let profile = server.ok("read_note", json!({"note": "bootstrap/Profile"}));
    let text = server.ok(
        "move_note",
        json!({"note": "bootstrap/Profile", "new_path": "loose/Profile", "expected_checksum": checksum_of(&profile)}),
    );
    assert!(
        text.ends_with("notice: bootstrap/Profile no longer loads as instructions at session start\n"),
        "{text}"
    );
    // A long note: the checksum comes from a cut read.
    let cut = server.ok("read_note", json!({"note": "loose/Big"}));
    assert!(cut.contains("[cut at 4000 tokens."), "the note should be cut");
    let text = server.ok(
        "move_note",
        json!({"note": "loose/Big", "new_path": "ops/Big", "expected_checksum": checksum_of(&cut)}),
    );
    assert_eq!(text, "moved: loose/Big -> ops/Big\nbroken links: none\n");
    assert_eq!(fs::read_to_string(v.root.join("ops/Big.md")).unwrap(), long);
}

#[test]
fn move_note_to_its_own_path_is_a_normal_result_not_an_error() {
    let v = vault();
    let mut server = v.server();
    let checksum = checksum_of(&server.ok("read_note", json!({"note": "backend/Release"})));
    // `isError` is false: `ok` fails the test on an error. A path with another case of a folder is the same path, since
    // an existing folder keeps its spelling.
    for path in ["backend/Release", "Backend/Release.md"] {
        assert_eq!(
            server.ok(
                "move_note",
                json!({"note": "backend/Release", "new_path": path, "expected_checksum": checksum})
            ),
            "The note is already at backend/Release, so nothing was moved."
        );
    }
    assert!(v.root.join("backend/Release.md").exists());
}

// Lint lines in write results (doc 07 §5.4): the same text from every surface that runs the tool.

#[test]
fn write_note_shows_its_lint_block_over_mcp_and_move_matches_the_cli() {
    let notes = [
        ("ideas/Caching.md", "# Caching\n\nA cache.\n"),
        ("ideas/Notes.md", "# Notes\n\nSee [[Caching]].\n"),
    ];
    let checksum = recognitor::hash::sha256_prefix(notes[1].1.as_bytes(), 12);
    let mcp_vault = TestVault::new(&notes);
    let mut server = mcp_vault.server();

    let long = "word ".repeat(70);
    let content = format!("# Long\n\n{}\n", long.trim_end());
    let text = server.ok("write_note", json!({"path": "design/Long", "content": content}));
    assert_eq!(
        text,
        format!(
            "design/Long  checksum: {}\nlint: 1 warning; the note is saved\n\
             - line 3: summary paragraph is 349 characters; the limit is 300\n",
            recognitor::hash::sha256_prefix(content.as_bytes(), 12)
        )
    );

    let over_mcp = server.ok(
        "move_note",
        json!({"note": "ideas/Notes", "new_path": "ops/Notes", "expected_checksum": checksum}),
    );
    drop(server);
    let cli_vault = TestVault::new(&notes);
    let over_cli = cli_vault.cli_text(&["move", "ideas/Notes", "ops/Notes", "--relink", "none"]);
    assert_eq!(over_mcp, over_cli);
    assert!(over_cli.contains("lint: 1 warning; the note is saved\n- line 3: link \"Caching\" reaches ideas/Caching"));
}

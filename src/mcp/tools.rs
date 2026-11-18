//! The five read tools of the MCP server (doc 07 §2.1, §3): their text, their schemas and their dispatch.
//!
//! Nothing here knows about the transport. [`call`] takes a tool name and the raw JSON arguments and returns the text
//! a client shows, with the error flag (doc 07 §2.1, "Errors"). Rendering is the library's own: a tool returns what
//! its CLI twin prints.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::bootstrap::DEFAULT_MAX_TOKENS as BOOTSTRAP_DEFAULT_MAX_TOKENS;
use crate::config;
use crate::read::DEFAULT_MAX_TOKENS as READ_DEFAULT_MAX_TOKENS;
use crate::{Error, Index};

/// The server's `instructions` (doc 07 §2.1, "Server instructions"), word for word.
pub const INSTRUCTIONS: &str = "Notes are Markdown files; a note's id is its path without \".md\". Use search to
find notes, context to see what links to a note, and read_note for its text.
Search and context show summaries only: read a note when you need its body.
A line ending in \"page N\" means more results exist.";

/// What an argument holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// A whole number from 1.
    Page,
}

/// One argument of a tool (doc 07 §2.1, "Tools", column "Arguments").
#[derive(Debug, Clone, Copy)]
pub struct Arg {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub required: bool,
}

/// One tool: its name, its description (doc 07 §2.1) and its arguments, in the order the table lists them.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub args: &'static [Arg],
}

const PAGE_NUMBER: &str = "the number from the \"page N\" line";

/// The tools, in the order of doc 07 §2.1. No write tool is here: shadow mode never opens a vault file for writing.
pub const TOOLS: [Tool; 5] = [
    Tool {
        name: "load_bootstrap",
        description: "Load the user's standing instructions: who they are, how to answer, rules for every project. Call once at session start, unless a hook already added them.",
        args: &[],
    },
    Tool {
        name: "load_namespace",
        description: "Load a namespace's entry note. Call once when work enters a namespace, then pass the same `ns` to `search`.",
        args: &[Arg {
            name: "ns",
            description: "the namespace name",
            kind: Kind::Text,
            required: true,
        }],
    },
    Tool {
        name: "search",
        description: "Full-text search. Returns ids, summaries and matching facts, 5 per page. Notes with every word come first; when fewer than 5 do, notes with only some of the words fill the first page. No operators. `ns` ranks that namespace first without filtering; `ns: \"archive\"` also searches session records.",
        args: &[
            Arg {
                name: "query",
                description: "plain words",
                kind: Kind::Text,
                required: true,
            },
            Arg {
                name: "ns",
                description: "from `load_namespace`",
                kind: Kind::Text,
                required: false,
            },
            Arg {
                name: "page",
                description: PAGE_NUMBER,
                kind: Kind::Page,
                required: false,
            },
        ],
    },
    Tool {
        name: "context",
        description: "Show a note's links out and in, each with its summary, without reading the neighbors. Use it to pick what to read next.",
        args: &[
            Arg {
                name: "note",
                description: "an id, path or name",
                kind: Kind::Text,
                required: true,
            },
            Arg {
                name: "page",
                description: PAGE_NUMBER,
                kind: Kind::Page,
                required: false,
            },
        ],
    },
    Tool {
        name: "read_note",
        description: "Return a note's full text with its checksum. `section` returns one section. A note over the token limit is cut to its best sections; the closing line says how to read the rest.",
        args: &[
            Arg {
                name: "note",
                description: "an id, path or name",
                kind: Kind::Text,
                required: true,
            },
            Arg {
                name: "section",
                description: "heading text as written",
                kind: Kind::Text,
                required: false,
            },
            Arg {
                name: "query",
                description: "words that choose which sections to keep when the note is cut",
                kind: Kind::Text,
                required: false,
            },
        ],
    },
];

impl Tool {
    /// The JSON Schema of the arguments: an object with one property per argument, each with its description.
    pub fn input_schema(&self) -> Map<String, Value> {
        let mut properties = Map::new();
        for arg in self.args {
            let kind = match arg.kind {
                Kind::Text => "string",
                Kind::Page => "integer",
            };
            properties.insert(
                arg.name.to_string(),
                json!({ "type": kind, "description": arg.description }),
            );
        }
        let required: Vec<&str> = self.args.iter().filter(|a| a.required).map(|a| a.name).collect();
        let mut schema = Map::new();
        schema.insert("type".to_string(), json!("object"));
        schema.insert("properties".to_string(), Value::Object(properties));
        if !required.is_empty() {
            schema.insert("required".to_string(), json!(required));
        }
        schema
    }
}

/// The result of one call: the text a client shows, and whether the call failed (doc 07 §2.1, "Errors").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub is_error: bool,
}

impl Outcome {
    fn ok(text: String) -> Self {
        Outcome { text, is_error: false }
    }

    fn error(text: impl Into<String>) -> Self {
        Outcome {
            text: text.into(),
            is_error: true,
        }
    }
}

/// The arguments of one call, checked against the tool's table row.
struct Args<'a> {
    tool: &'a Tool,
    given: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    /// Checks the arguments: nothing the tool does not take, nothing of the wrong type, every required one present.
    /// Fails with the message of the first problem, in the order of the table.
    fn check(tool: &'a Tool, given: &'a Map<String, Value>) -> Result<Self, String> {
        if let Some(extra) = given.keys().find(|k| !tool.args.iter().any(|a| a.name == k.as_str())) {
            // Doc 07 §2.1, "Unknown arguments are errors": a misspelled `pages` must not pass for a default.
            return Err(format!("unexpected argument: {extra}"));
        }
        for arg in tool.args {
            match given.get(arg.name) {
                None | Some(Value::Null) if arg.required => return Err(missing_message(tool, arg)),
                None | Some(Value::Null) => {}
                Some(Value::String(_)) if arg.kind == Kind::Text => {}
                Some(Value::Number(n)) if arg.kind == Kind::Page && n.as_u64().is_some_and(|p| p >= 1) => {}
                Some(_) => return Err(wrong_type_message(arg)),
            }
        }
        Ok(Args { tool, given })
    }

    fn text(&self, name: &str) -> Option<&'a str> {
        self.given.get(name).and_then(Value::as_str)
    }

    /// A required text argument; `check` has made sure it is there.
    fn required_text(&self, name: &str) -> Result<&'a str, String> {
        let arg = self.tool.args.iter().find(|a| a.name == name);
        self.text(name)
            .ok_or_else(|| arg.map_or_else(|| format!("{name} is missing"), |a| missing_message(self.tool, a)))
    }

    /// The page, from 1; 1 when it is not given.
    fn page(&self) -> usize {
        self.given
            .get("page")
            .and_then(Value::as_u64)
            .and_then(|p| usize::try_from(p).ok())
            .unwrap_or(1)
    }
}

// The messages of bad arguments (doc 07 §2.1, "Argument messages"): `<tool> needs <what>` for a missing argument,
// `<argument> needs text` or `page needs a number from 1` for a wrong type.

fn missing_message(tool: &Tool, arg: &Arg) -> String {
    let what = match arg.name {
        "query" => "a query".to_string(),
        "note" => "a note".to_string(),
        other => other.to_string(),
    };
    format!("{} needs {what}", tool.name)
}

fn wrong_type_message(arg: &Arg) -> String {
    match arg.kind {
        Kind::Page => format!("{} needs a number from 1", arg.name),
        Kind::Text => format!("{} needs text", arg.name),
    }
}

/// Runs one tool and returns what its CLI twin prints (doc 07 §12), or the CLI's message as an error.
///
/// Every tool runs the stale-gated sweep first, through the same `Index` method the CLI calls (doc 02 §4).
/// `config_file` is where `read.max_tokens` and `bootstrap.max_tokens` come from, as for the CLI.
///
/// Returns `None` for a name that is not a tool: the caller answers that with a protocol error.
pub fn call(index: &mut Index, config_file: &Path, name: &str, arguments: &Map<String, Value>) -> Option<Outcome> {
    let tool = TOOLS.iter().find(|t| t.name == name)?;
    let args = match Args::check(tool, arguments) {
        Ok(args) => args,
        Err(message) => return Some(Outcome::error(message)),
    };
    let mut run = || -> Result<String, String> {
        let failed = |e: Error| e.to_string();
        match tool.name {
            "load_bootstrap" => {
                let bootstrap = index.bootstrap().map_err(failed)?;
                let max_tokens = config::bootstrap_max_tokens(config_file)
                    .map_err(failed)?
                    .unwrap_or(BOOTSTRAP_DEFAULT_MAX_TOKENS);
                Ok(bootstrap.render(max_tokens))
            }
            "load_namespace" => index.load_namespace(args.required_text("ns")?).map_err(failed),
            "search" => {
                let query = args.required_text("query")?;
                let page = index.search(query, args.text("ns"), args.page()).map_err(failed)?;
                Ok(crate::search::render(&page))
            }
            "context" => {
                let note = args.required_text("note")?;
                let context = index.context(note, args.page()).map_err(failed)?;
                Ok(crate::context::render(&context))
            }
            _ => {
                let note = args.required_text("note")?;
                let max_tokens = config::read_max_tokens(config_file)
                    .map_err(failed)?
                    .unwrap_or(READ_DEFAULT_MAX_TOKENS);
                let read = index
                    .read(note, args.text("section"), args.text("query"), max_tokens)
                    .map_err(failed)?;
                Ok(crate::read::render(&read))
            }
        }
    };
    Some(match run() {
        Ok(text) => Outcome::ok(text),
        Err(message) => Outcome::error(message),
    })
}

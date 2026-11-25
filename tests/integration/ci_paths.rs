//! Guard for the `.code-paths` list in `.gitlab-ci.yml`. The Rust CI jobs only run when a commit touches one of
//! those paths, so a file that a test reads at compile time (`include_str!` and friends) must be on the list. If
//! it is not, a commit that changes only that file would skip the tests that depend on it.
//!
//! The scanner is a small lexer, not a regex: it skips comments and string literals, so the macro names written in
//! this file's own comments and test inputs are not mistaken for real calls. It only understands the argument forms
//! below. Any other form fails the test with `file:line`, so an unknown form never passes silently.
//!
//! - a string literal, relative to the directory of the `.rs` file
//! - `concat!(env!("CARGO_MANIFEST_DIR"), "<literal>", ...)`, relative to the repo root
//! - `concat!("<literal>", ...)`, relative to the `.rs` file like a plain literal

use std::fs;
use std::path::Path;

/// The macros that pull a file into the build. Needles are only ever matched as code tokens, never as text.
const INCLUDE_MACROS: [&str; 3] = ["include_str", "include_bytes", "include"];

/// Folders the Rust jobs compile. Matches the `src/**/*`, `tests/**/*`, `benches/**/*` and `examples/**/*` entries.
const SCANNED_DIRS: [&str; 4] = ["src", "tests", "benches", "examples"];

const CI_FILE: &str = ".gitlab-ci.yml";
const CODE_PATHS_KEY: &str = ".code-paths";

/// One include call: the `.rs` file and line it is on, and the repo-relative path it reads.
#[derive(Debug, PartialEq, Eq)]
struct Include {
    file: String,
    line: usize,
    target: String,
}

// ---- Lexer ----------------------------------------------------------------------------------------------------

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Self {
        Cursor {
            b: src.as_bytes(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.b[self.i..].starts_with(s.as_bytes())
    }

    fn line(&self) -> usize {
        1 + self.b[..self.i].iter().filter(|&&c| c == b'\n').count()
    }

    /// Skips whitespace and comments.
    fn skip_trivia(&mut self) -> Result<(), String> {
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_whitespace() => self.i += 1,
                _ if self.starts_with("//") => self.skip_line_comment(),
                _ if self.starts_with("/*") => self.skip_block_comment()?,
                _ => return Ok(()),
            }
        }
    }

    fn skip_line_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            self.i += 1;
        }
    }

    /// Block comments nest in Rust.
    fn skip_block_comment(&mut self) -> Result<(), String> {
        let start = self.line();
        let mut depth = 0usize;
        while self.i < self.b.len() {
            if self.starts_with("/*") {
                depth += 1;
                self.i += 2;
            } else if self.starts_with("*/") {
                depth -= 1;
                self.i += 2;
                if depth == 0 {
                    return Ok(());
                }
            } else {
                self.i += 1;
            }
        }
        Err(format!("line {start}: unterminated block comment"))
    }

    fn ident(&mut self) -> Option<&'a str> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            return None;
        }
        // Idents are ASCII here, so the slice is on char boundaries and is valid UTF-8.
        std::str::from_utf8(&self.b[start..self.i]).ok()
    }

    /// Reads a string literal that starts at the cursor: `"..."` or a raw string `r#"..."#`. The cursor must be on
    /// the opening quote or on the `#`s of a raw string. Returns the raw text between the quotes and whether it had
    /// a backslash escape (which a non-raw string may).
    fn string_body(&mut self, raw: bool) -> Result<(String, bool), String> {
        let line = self.line();
        let mut hashes = 0usize;
        if raw {
            while self.peek() == Some(b'#') {
                hashes += 1;
                self.i += 1;
            }
        }
        if self.peek() != Some(b'"') {
            return Err(format!("line {line}: expected a string literal"));
        }
        self.i += 1;
        let start = self.i;
        let mut escaped = false;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if !raw && c == b'\\' {
                escaped = true;
                self.i += 2;
                continue;
            }
            if c == b'"' && (0..hashes).all(|k| self.b.get(self.i + 1 + k) == Some(&b'#')) {
                let text = String::from_utf8_lossy(&self.b[start..self.i]).into_owned();
                self.i += 1 + hashes;
                return Ok((text, escaped));
            }
            self.i += 1;
        }
        Err(format!("line {line}: unterminated string literal"))
    }

    /// A string literal as an argument: `"..."`, `r"..."`, `r#"..."#`. `None` if the cursor is on something else.
    fn string_arg(&mut self) -> Result<Option<String>, String> {
        let line = self.line();
        let raw = match self.peek() {
            Some(b'"') => false,
            Some(b'r') if matches!(self.b.get(self.i + 1), Some(b'"' | b'#')) => {
                self.i += 1;
                true
            }
            _ => return Ok(None),
        };
        let (text, escaped) = self.string_body(raw)?;
        if escaped {
            return Err(format!(
                "line {line}: a backslash escape in a path literal is not supported"
            ));
        }
        Ok(Some(text))
    }

    /// Skips a char literal or a lifetime at a `'`.
    fn skip_quote(&mut self) {
        self.i += 1;
        if self.peek() == Some(b'\\') {
            // Char literal with an escape: `'\n'`, `'\''`, `'\u{..}'`.
            self.i += 2;
            while let Some(c) = self.peek() {
                self.i += 1;
                if c == b'\'' {
                    break;
                }
            }
        } else if self.b.get(self.i + 1) == Some(&b'\'') {
            self.i += 2; // `'x'`
        } else if let Some(c) = self.peek() {
            // `'é'` is a multi-byte char literal; skip its bytes up to the closing quote if it is close.
            if c >= 0x80
                && let Some(off) = self.b[self.i..].iter().take(5).position(|&x| x == b'\'')
            {
                self.i += off + 1;
            }
            // Otherwise a lifetime: the ident that follows is scanned as an ordinary ident.
        }
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            self.i += s.len();
            true
        } else {
            false
        }
    }
}

// ---- Scanner --------------------------------------------------------------------------------------------------

/// Finds every `include_str!`, `include_bytes!` and `include!` call in `src` (the text of the repo file `file`)
/// and resolves its target to a repo-relative path with `/` separators.
fn scan_source(file: &str, src: &str) -> Result<Vec<Include>, String> {
    let mut c = Cursor::new(src);
    let mut found = Vec::new();
    while let Some(ch) = c.peek() {
        let at = |c: &Cursor, msg: &str| format!("{file}:{}: {msg}", c.line());
        if c.starts_with("//") {
            c.skip_line_comment();
        } else if c.starts_with("/*") {
            c.skip_block_comment().map_err(|e| format!("{file}:{e}"))?;
        } else if ch == b'"' {
            c.string_body(false).map_err(|e| format!("{file}:{e}"))?;
        } else if ch == b'\'' {
            c.skip_quote();
        } else if ch.is_ascii_alphabetic() || ch == b'_' {
            let line = c.line();
            let word = c.ident().unwrap_or("");
            // String prefixes: r"..", r#".."#, b"..", br"..", br#".."#. Not raw identifiers like `r#type`.
            let next = c.peek();
            let is_raw_prefix = matches!(word, "r" | "br") && matches!(next, Some(b'"' | b'#'));
            if is_raw_prefix {
                let save = c.i;
                if c.string_body(true).is_err() {
                    c.i = save; // `r#type`: not a string
                }
            } else if word == "b" && next == Some(b'"') {
                c.string_body(false).map_err(|e| format!("{file}:{e}"))?;
            } else if INCLUDE_MACROS.contains(&word) {
                let save = c.i;
                c.skip_trivia().map_err(|e| format!("{file}:{e}"))?;
                if c.eat("!") {
                    c.skip_trivia().map_err(|e| format!("{file}:{e}"))?;
                    if c.eat("(") {
                        let target =
                            parse_include_args(&mut c, file).map_err(|e| format!("{file}:{line}: {word}!: {e}"))?;
                        found.push(Include {
                            file: file.to_string(),
                            line,
                            target,
                        });
                    } else {
                        return Err(at(&c, &format!("{word}! without parentheses is not supported")));
                    }
                } else {
                    c.i = save; // an ordinary identifier that happens to be named like the macro
                }
            }
        } else {
            c.i += 1;
        }
    }
    Ok(found)
}

/// Parses the argument list of an include macro, the cursor just after `(`. Leaves the cursor after the `)`.
fn parse_include_args(c: &mut Cursor, file: &str) -> Result<String, String> {
    c.skip_trivia()?;
    let (manifest_rooted, literal) = if let Some(lit) = c.string_arg()? {
        (false, lit)
    } else if c.starts_with("concat") {
        c.ident();
        c.skip_trivia()?;
        if !c.eat("!") {
            return Err("unsupported argument form".to_string());
        }
        c.skip_trivia()?;
        if !c.eat("(") {
            return Err("unsupported argument form".to_string());
        }
        parse_concat(c)?
    } else {
        return Err("unsupported argument form (expected a string literal or concat!)".to_string());
    };
    c.skip_trivia()?;
    c.eat(","); // a trailing comma is allowed
    c.skip_trivia()?;
    if !c.eat(")") {
        return Err("unsupported argument form (expected `)` after the path)".to_string());
    }
    resolve(file, &literal, manifest_rooted)
}

/// The inside of `concat!( ... )`, the cursor just after `(`. Returns whether it starts with
/// `env!("CARGO_MANIFEST_DIR")` and the joined literals.
fn parse_concat(c: &mut Cursor) -> Result<(bool, String), String> {
    let mut manifest_rooted = false;
    let mut out = String::new();
    let mut first = true;
    loop {
        c.skip_trivia()?;
        if c.eat(")") {
            break;
        }
        if let Some(lit) = c.string_arg()? {
            out.push_str(&lit);
        } else if c.starts_with("env") && first {
            c.ident();
            c.skip_trivia()?;
            let ok = c.eat("!") && {
                c.skip_trivia()?;
                c.eat("(")
            };
            c.skip_trivia()?;
            let var = if ok { c.string_arg()? } else { None };
            c.skip_trivia()?;
            if var.as_deref() != Some("CARGO_MANIFEST_DIR") || !c.eat(")") {
                return Err("unsupported concat! part (only env!(\"CARGO_MANIFEST_DIR\") is understood)".to_string());
            }
            manifest_rooted = true;
        } else {
            return Err(
                "unsupported concat! part (expected string literals after env!(\"CARGO_MANIFEST_DIR\"))".to_string(),
            );
        }
        first = false;
        c.skip_trivia()?;
        if !c.eat(",") {
            c.skip_trivia()?;
            if !c.eat(")") {
                return Err("unsupported concat! syntax".to_string());
            }
            break;
        }
    }
    Ok((manifest_rooted, out))
}

/// Turns a literal into a repo-relative path with `/` separators and no `.` or `..` parts.
fn resolve(file: &str, literal: &str, manifest_rooted: bool) -> Result<String, String> {
    let mut parts: Vec<&str> = Vec::new();
    if !manifest_rooted {
        if literal.starts_with('/') || literal.starts_with('\\') {
            return Err(format!("absolute path `{literal}` is not supported"));
        }
        parts.extend(file.split('/'));
        parts.pop(); // the file name; the path is relative to its folder
    }
    for seg in literal.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("`{literal}` leaves the repo"));
                }
            }
            s => parts.push(s),
        }
    }
    Ok(parts.join("/"))
}

// ---- Matcher --------------------------------------------------------------------------------------------------

/// `prefix/**/*` matches any path under `prefix/`; any other entry without glob characters is an exact path.
/// Any other glob form is an error, so the matcher is never silently wrong about a pattern it does not know.
fn is_covered(patterns: &[String], path: &str) -> Result<bool, String> {
    let mut covered = false;
    for p in patterns {
        if let Some(prefix) = p.strip_suffix("**/*") {
            if prefix.is_empty() || !prefix.ends_with('/') || prefix.contains(['*', '?', '[', '{']) {
                return Err(format!("unsupported pattern `{p}`, extend the matcher"));
            }
            covered |= path.starts_with(prefix);
        } else if p.contains(['*', '?', '[', ']', '{', '}']) {
            return Err(format!("unsupported pattern `{p}`, extend the matcher"));
        } else {
            covered |= path == p;
        }
    }
    Ok(covered)
}

/// The include targets that no pattern covers, as `target (file:line)`. Sorted and without duplicates.
fn uncovered(patterns: &[String], includes: &[Include]) -> Result<Vec<String>, String> {
    // Check every pattern even when there is nothing to match, so a bad entry always fails.
    is_covered(patterns, "")?;
    let mut missing = Vec::new();
    for inc in includes {
        if !is_covered(patterns, &inc.target)? {
            missing.push(format!("{} ({}:{})", inc.target, inc.file, inc.line));
        }
    }
    missing.sort();
    missing.dedup();
    Ok(missing)
}

// ---- Real repo ------------------------------------------------------------------------------------------------

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn code_paths_from(yaml: &str) -> Result<Vec<String>, String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(|e| format!("{CI_FILE} is not valid YAML: {e}"))?;
    let list = doc
        .get(CODE_PATHS_KEY)
        .and_then(|v| v.as_sequence())
        .ok_or_else(|| format!("{CI_FILE} has no `{CODE_PATHS_KEY}` list"))?;
    list.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("`{CODE_PATHS_KEY}` has a non-string entry: {v:?}"))
        })
        .collect()
}

fn scan_dir(dir: &Path, rel: &str, out: &mut Vec<Include>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {rel}: {e}"))?;
    let mut entries: Vec<_> = entries
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cannot read {rel}: {e}"))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{rel}/{name}");
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, &child, out)?;
        } else if name.ends_with(".rs") {
            let src = fs::read_to_string(&path).map_err(|e| format!("cannot read {child}: {e}"))?;
            out.extend(scan_source(&child, &src)?);
        }
    }
    Ok(())
}

fn scan_repo() -> Result<Vec<Include>, String> {
    let mut out = Vec::new();
    for d in SCANNED_DIRS {
        let path = root().join(d);
        if path.is_dir() {
            scan_dir(&path, d, &mut out)?;
        }
    }
    Ok(out)
}

fn real_code_paths() -> Vec<String> {
    let yaml = fs::read_to_string(root().join(CI_FILE)).unwrap();
    code_paths_from(&yaml).unwrap()
}

// ---- Tests on the real repo -----------------------------------------------------------------------------------

#[test]
fn every_included_file_is_on_the_ci_code_paths_list() {
    let patterns = real_code_paths();
    let includes = scan_repo().unwrap();

    // Guard against a vacuous pass: tests/mcp.rs includes doc 07, so a working scan must see it.
    assert!(
        includes.iter().any(|i| i.target == "docs/07-interface-contracts.md"),
        "the scan did not find the include of docs/07-interface-contracts.md; the scanner is broken: {includes:?}"
    );

    let missing = uncovered(&patterns, &includes).unwrap();
    assert!(
        missing.is_empty(),
        "these files are read at compile time but are not covered by `.code-paths` in {CI_FILE}; add them there so a \
         commit that changes only them still runs the Rust jobs:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn the_scanner_does_not_match_its_own_needles() {
    // This file names the macros in comments and string literals only, so it has no real include call.
    let src = fs::read_to_string(root().join("tests/integration/ci_paths.rs")).unwrap();
    assert_eq!(scan_source("tests/integration/ci_paths.rs", &src).unwrap(), vec![]);
}

#[test]
fn the_ci_anchor_resolves_to_the_list_every_rust_job_uses() {
    let yaml = fs::read_to_string(root().join(CI_FILE)).unwrap();
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let list = code_paths_from(&yaml).unwrap();
    assert!(list.len() > 5, "{list:?}");
    for job in ["fmt-clippy", "test-linux", "test-windows"] {
        let rules = doc
            .get(job)
            .and_then(|j| j.get("rules"))
            .and_then(|r| r.as_sequence())
            .unwrap();
        let changes: Vec<Vec<String>> = rules
            .iter()
            .filter_map(|r| r.get("changes"))
            .map(|c| {
                c.as_sequence()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect()
            })
            .collect();
        assert!(!changes.is_empty(), "{job} has no `changes` rule");
        for c in changes {
            assert_eq!(c, list, "{job}: `changes` is not the shared code-paths list");
        }
    }
}

// ---- Unit tests on in-memory inputs ---------------------------------------------------------------------------

fn pats(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

fn targets(file: &str, src: &str) -> Vec<String> {
    scan_source(file, src).unwrap().into_iter().map(|i| i.target).collect()
}

#[test]
fn a_literal_is_resolved_against_the_folder_of_the_rs_file() {
    let src = "const A: &str = include_str!(\"../docs/07-interface-contracts.md\");";
    assert_eq!(targets("tests/mcp.rs", src), ["docs/07-interface-contracts.md"]);
    assert_eq!(
        targets("src/mcp/tools.rs", "include_bytes!(\"data/x.bin\")"),
        ["src/mcp/data/x.bin"]
    );
    assert_eq!(targets("src/a.rs", "include!(\"./gen/../b.rs\",)"), ["src/b.rs"]);
}

#[test]
fn the_line_of_a_call_is_reported() {
    let found = scan_source("src/a.rs", "fn f() {}\n\nconst A: &str = include_str!(\"x.md\");\n").unwrap();
    assert_eq!(
        found,
        vec![Include {
            file: "src/a.rs".into(),
            line: 3,
            target: "src/x.md".into()
        }]
    );
}

#[test]
fn concat_with_the_manifest_dir_is_relative_to_the_repo_root() {
    let src = "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/docs/x.md\"))";
    assert_eq!(targets("tests/deep/a.rs", src), ["docs/x.md"]);
    let src = "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/docs\", \"/y.md\"))";
    assert_eq!(targets("src/a.rs", src), ["docs/y.md"]);
    // Plain literals in concat! are relative to the file, like a bare literal.
    assert_eq!(
        targets("src/a.rs", "include_str!(concat!(\"a/\", \"b.md\"))"),
        ["src/a/b.md"]
    );
}

#[test]
fn raw_string_arguments_and_windows_separators_are_normalized() {
    assert_eq!(
        targets("tests/a.rs", "include_str!(r\"..\\docs\\x.md\")"),
        ["docs/x.md"]
    );
    assert_eq!(
        targets("tests/a.rs", "include_str!(r#\"../docs/x.md\"#)"),
        ["docs/x.md"]
    );
}

#[test]
fn comments_strings_and_lookalikes_are_not_calls() {
    let src = r##"
// include_str!("a.md")
/* include_str!("b.md") /* nested include_str!("c.md") */ still comment */
/// Docs mention include_bytes!("d.md").
const S: &str = "include_str!(\"e.md\")";
const R: &str = r#"include_str!("f.md")"#;
let c = '"'; let q = '\''; let _l: &'static str = "";
fn include_str_helper() {}
let include = 3;
"##;
    assert_eq!(targets("src/a.rs", src), Vec::<String>::new());
}

#[test]
fn a_call_after_lookalikes_is_still_found() {
    let src = "let c = '\"'; let _x: &'static str = \"s\"; include_str!(\"x.md\"); // include_str!(\"y.md\")";
    assert_eq!(targets("src/a.rs", src), ["src/x.md"]);
}

#[test]
fn an_unknown_argument_form_fails_with_file_and_line() {
    let cases = [
        "include_str!(PATH)",
        "include_str!(env!(\"X\"))",
        "include_str!(concat!(env!(\"OUT_DIR\"), \"/x\"))",
        "include_str!(concat!(\"a\", FOO))",
        "include_str!(concat!(\"a\", env!(\"CARGO_MANIFEST_DIR\")))",
        "include_str!(\"a\\n.md\")",
        "include_str!(\"/abs/x.md\")",
        "include_str!(\"../../../../x.md\")",
        "include_str!(\"a.md\" \"b\")",
        "include_str!",
    ];
    for src in cases {
        let err = scan_source("src/a.rs", &format!("\n{src};")).unwrap_err();
        assert!(err.starts_with("src/a.rs:2"), "{src}: {err}");
    }
}

#[test]
fn an_uncovered_path_fails_and_a_covered_one_passes() {
    let patterns = pats(&["src/**/*", "Cargo.toml", "docs/07-interface-contracts.md"]);
    let inc = |t: &str| Include {
        file: "tests/a.rs".into(),
        line: 7,
        target: t.into(),
    };

    assert_eq!(
        uncovered(&patterns, &[inc("docs/03-notes.md")]).unwrap(),
        ["docs/03-notes.md (tests/a.rs:7)"]
    );
    assert_eq!(
        uncovered(&patterns, &[inc("README.md")]).unwrap(),
        ["README.md (tests/a.rs:7)"]
    );
    assert!(
        uncovered(
            &patterns,
            &[
                inc("src/deep/x.rs"),
                inc("Cargo.toml"),
                inc("docs/07-interface-contracts.md")
            ]
        )
        .unwrap()
        .is_empty()
    );
    // Exact entries are exact, and a glob needs the folder separator.
    assert!(!is_covered(&patterns, "Cargo.toml.bak").unwrap());
    assert!(!is_covered(&patterns, "src2/x.rs").unwrap());
    assert!(!is_covered(&patterns, "docs/07-interface-contracts.md.old").unwrap());
}

#[test]
fn an_unsupported_pattern_fails_even_with_nothing_to_match() {
    for bad in [
        "src/*.rs",
        "**/*.md",
        "docs/0?-x.md",
        "{a,b}/**/*",
        "src/*/x",
        "**/*",
        "src**/*",
    ] {
        let err = uncovered(&pats(&["Cargo.toml", bad]), &[]).unwrap_err();
        assert!(
            err.contains("unsupported pattern") && err.contains("extend the matcher"),
            "{bad}: {err}"
        );
    }
}

#[test]
fn the_code_paths_list_is_read_through_a_yaml_anchor() {
    let yaml = "\
.code-paths: &p
  - src/**/*
  - Cargo.toml  # comment
job:
  rules:
    - changes: *p
";
    assert_eq!(code_paths_from(yaml).unwrap(), ["src/**/*", "Cargo.toml"]);
    assert!(
        code_paths_from("job: {}\n")
            .unwrap_err()
            .contains("no `.code-paths` list")
    );
}

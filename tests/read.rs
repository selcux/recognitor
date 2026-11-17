//! `read` tests (doc 07 §7, §7.1, §6.3, §1.1). Vaults are synthetic and live in temp folders that delete
//! themselves.
//!
//! The long-note cut (§7.2) and the section read (§7.3) are not built: the docs leave open what a section is, what
//! the limit counts and how the closing line shows checksums (see the slice 5 report). These tests cover the whole
//! read, the lookup, the header and the limit check that decides when a note counts as long.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::hash::sha256_prefix;
use recognitor::read::{self, DEFAULT_MAX_TOKENS, Read};

fn tok(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}
use recognitor::{Error, Index, SweepOptions, config};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
    config: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-read-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let v = TestVault {
            root,
            db: base.join("index.db"),
            config: base.join("config.toml"),
        };
        for (rel, text) in files {
            v.write(rel, text.as_bytes());
        }
        v
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn index(&self) -> Index {
        let mut index = Index::open(&self.root, Some(&self.db)).unwrap();
        index.rebuild(SweepOptions::default()).unwrap();
        index
    }
}

fn rd(index: &Index, name: &str, section: Option<&str>, query: Option<&str>, limit: usize) -> recognitor::Result<Read> {
    read::read(index.connection(), index.vault_root(), name, section, query, limit)
}

fn text_of(index: &Index, name: &str) -> String {
    read::render(&rd(index, name, None, None, DEFAULT_MAX_TOKENS).unwrap())
}

/// The note of the doc 07 §7 example.
const DECISIONS: &str = "---\ntype: hub\n---\n# Decisions\n\nDurable decisions for the engine, newest first.\n";

// The header line and the whole file (doc 07 §7).

#[test]
fn prints_the_header_line_and_the_whole_file() {
    let v = TestVault::new(&[("recognitor/Decisions.md", DECISIONS)]);
    let index = v.index();
    let sum = sha256_prefix(DECISIONS.as_bytes(), 12);
    assert_eq!(
        text_of(&index, "recognitor/Decisions"),
        format!("recognitor/Decisions  checksum: {sum}\n{DECISIONS}")
    );
}

/// Doc 07 §7.1: twelve hex characters of the SHA-256 of the file.
#[test]
fn the_checksum_is_twelve_hex_characters_of_the_file_sha256() {
    let v = TestVault::new(&[("a/N.md", "# N\n\nBody.\n")]);
    let index = v.index();
    let text = text_of(&index, "a/N");
    let header = text.lines().next().unwrap();
    let sum = header.strip_prefix("a/N  checksum: ").unwrap();
    assert_eq!(sum.len(), 12);
    assert!(sum.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{sum}");
    assert_eq!(sum, sha256_prefix(b"# N\n\nBody.\n", 12));
}

/// Doc 07 §4.2 "Line 1": every tool that shows an id adds a title that differs from the filename by key.
#[test]
fn the_header_adds_a_title_that_differs_from_the_filename() {
    let body = "# Tools & sourcing\n\nWhat to buy.\n";
    let v = TestVault::new(&[("baseline/Tools and sourcing.md", body)]);
    let index = v.index();
    let sum = sha256_prefix(body.as_bytes(), 12);
    assert_eq!(
        text_of(&index, "baseline/Tools and sourcing"),
        format!("baseline/Tools and sourcing, titled \"Tools & sourcing\"  checksum: {sum}\n{body}")
    );
}

/// Doc 07 §7: "the whole file exactly as on disk". Line endings, a missing final newline and non-ASCII text stay.
#[test]
fn the_file_is_printed_byte_for_byte() {
    let body = "---\r\ntags: [a]\r\n---\r\n# Kırmızı\r\n\r\nSummary.\r\n\r\n- [fact] ünï text\r\nno final newline";
    let v = TestVault::new(&[("a/kirmizi.md", body)]);
    let index = v.index();
    let sum = sha256_prefix(body.as_bytes(), 12);
    assert_eq!(
        text_of(&index, "a/kirmizi"),
        format!("a/kirmizi, titled \"Kırmızı\"  checksum: {sum}\n{body}")
    );
}

/// Doc 07 §7: "the content always comes from that one file". The file is read from disk, so an edit made after the
/// last sweep shows with its own checksum.
#[test]
fn the_content_comes_from_the_file_not_the_index() {
    let v = TestVault::new(&[("a/N.md", "# N\n\nOld.\n")]);
    let index = v.index();
    v.write("a/N.md", b"# N\n\nNew, and longer.\n");
    let sum = sha256_prefix(b"# N\n\nNew, and longer.\n", 12);
    assert_eq!(
        text_of(&index, "a/N"),
        format!("a/N  checksum: {sum}\n# N\n\nNew, and longer.\n")
    );
}

#[test]
fn a_file_that_vanished_after_the_sweep_is_not_found() {
    let v = TestVault::new(&[("a/N.md", "# N\n\nBody.\n")]);
    let index = v.index();
    fs::remove_file(v.root.join("a/N.md")).unwrap();
    let err = rd(&index, "a/N", None, None, DEFAULT_MAX_TOKENS).unwrap_err();
    assert!(matches!(err, Error::NoteNotFound(ref n) if n == "a/N"), "{err}");
}

// Notes named in a tool call (doc 07 §6.3).

#[test]
fn the_note_can_be_named_by_id_path_or_name_in_any_case() {
    let v = TestVault::new(&[
        (
            "common/working-style/ADHD profile.md",
            "# ADHD profile\n\nHow to work.\n",
        ),
        ("compass/Index.md", "# Index\n\nThe compass.\n"),
    ]);
    let index = v.index();
    let expected = text_of(&index, "common/working-style/ADHD profile");
    assert!(expected.starts_with("common/working-style/ADHD profile  checksum: "));
    for name in [
        "common/working-style/adhd profile",
        "common/working-style/ADHD profile.md",
        "ADHD profile",
        "adhd PROFILE",
        "common::adhd profile",
    ] {
        assert_eq!(text_of(&index, name), expected, "{name}");
    }
    // The id as written shows, whatever the case of the argument.
    assert!(text_of(&index, "compass::index").starts_with("compass/Index  checksum: "));
}

/// Doc 07 §6.3: several matches give their ids, sorted, as a normal result.
#[test]
fn several_matches_list_their_ids_and_read_none() {
    let v = TestVault::new(&[
        ("common/index.md", "# Index\n\nCommon.\n"),
        ("compass/Index.md", "# Index\n\nCompass.\n"),
        ("baseline/index.md", "# Index\n\nBaseline.\n"),
    ]);
    let index = v.index();
    let r = rd(&index, "index", None, None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(matches!(r, Read::Several { .. }));
    assert_eq!(
        read::render(&r),
        "\"index\" matches 3 notes. Call again with one id:\n  baseline/index\n  common/index\n  compass/Index\n"
    );
}

#[test]
fn a_name_that_matches_nothing_is_an_error() {
    let v = TestVault::new(&[("a/N.md", "# N\n\nBody.\n")]);
    let index = v.index();
    let err = rd(&index, "Missing Note", None, None, DEFAULT_MAX_TOKENS).unwrap_err();
    assert!(matches!(err, Error::NoteNotFound(_)));
    assert_eq!(err.to_string(), "\"Missing Note\" not found");
}

// Stores (doc 07 §1.1): `read_note` works in all three.

#[test]
fn bootstrap_and_archive_notes_can_be_read() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Boot\n\nLoads first.\n\n## Relations\n- has_part [[Profile]]\n",
        ),
        (
            "archive/recognitor/Session 2026-11-14.md",
            "# Session 2026-11-14\n\nA transcript.\n",
        ),
    ]);
    let index = v.index();
    let boot = text_of(&index, "bootstrap/index");
    // The whole file, Relations included: `read` strips nothing (doc 07 §7).
    assert!(boot.ends_with("## Relations\n- has_part [[Profile]]\n"), "{boot}");
    assert!(
        boot.starts_with("bootstrap/index, titled \"Boot\"  checksum: "),
        "{boot}"
    );
    let arch = text_of(&index, "archive/recognitor/Session 2026-11-14");
    assert!(arch.ends_with("# Session 2026-11-14\n\nA transcript.\n"), "{arch}");
}

// The limit (doc 07 §7.2): `read.max_tokens`, default 4,000, tokens estimated as characters / 4.

#[test]
fn the_default_limit_is_4000_tokens() {
    assert_eq!(DEFAULT_MAX_TOKENS, 4000);
}

/// "A note is cut only when its whole output would be over the limit": the header line is part of the output.
#[test]
fn a_note_is_cut_only_when_its_whole_output_is_over_the_limit() {
    let body = format!(
        "# N

{}

## A

x
",
        "word ".repeat(100)
    );
    let v = TestVault::new(&[("a/N.md", body.as_str())]);
    let index = v.index();
    let whole = text_of(&index, "a/N");
    let tokens = tok(&whole);
    assert!(matches!(rd(&index, "a/N", None, None, tokens).unwrap(), Read::Whole(_)));
    assert!(matches!(
        rd(&index, "a/N", None, None, tokens - 1).unwrap(),
        Read::Cut(_)
    ));
}

// The config key `read.max_tokens` (doc 07 §7.2, doc 05 §5).

#[test]
fn read_max_tokens_is_a_config_key_holding_a_whole_number() {
    let v = TestVault::new(&[]);
    assert!(config::KEYS.contains(&"read.max_tokens"));
    assert_eq!(config::read_max_tokens(&v.config).unwrap(), None);
    config::set(&v.config, "read.max_tokens", "2500").unwrap();
    assert_eq!(
        config::get(&v.config, "read.max_tokens").unwrap().as_deref(),
        Some("2500")
    );
    assert_eq!(config::read_max_tokens(&v.config).unwrap(), Some(2500));
    assert!(fs::read_to_string(&v.config).unwrap().contains("max_tokens = 2500"));
    for bad in ["0", "-3", "many", "1.5", ""] {
        let err = config::set(&v.config, "read.max_tokens", bad).unwrap_err();
        assert!(matches!(err, Error::InvalidConfigValue { .. }), "{bad}: {err}");
    }
    // The two max_tokens keys do not share a value.
    assert_eq!(config::bootstrap_max_tokens(&v.config).unwrap(), None);
}

// The CLI twin (doc 07 §12).

fn cli(v: &TestVault, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(args)
        .arg("--vault")
        .arg(&v.root)
        .env("RECOGNITOR_DB", &v.db)
        .env("RECOGNITOR_CONFIG", &v.config)
        .env_remove("RECOGNITOR_VAULT")
        .output()
        .unwrap()
}

#[test]
fn cli_prints_the_note_as_the_library_does() {
    let v = TestVault::new(&[("recognitor/Decisions.md", DECISIONS)]);
    let sum = sha256_prefix(DECISIONS.as_bytes(), 12);
    let out = cli(&v, &["read", "recognitor/Decisions"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("recognitor/Decisions  checksum: {sum}\n{DECISIONS}")
    );
}

#[test]
fn cli_prints_bytes_exactly() {
    let body = "# N\r\n\r\nline\r\nlast";
    let v = TestVault::new(&[("a/N.md", body)]);
    let out = cli(&v, &["read", "a/N"]);
    assert!(out.status.success());
    let header = format!("a/N  checksum: {}\n", sha256_prefix(body.as_bytes(), 12));
    assert_eq!(out.stdout, [header.as_bytes(), body.as_bytes()].concat());
}

#[test]
fn cli_lists_several_matches_and_fails_on_none() {
    let v = TestVault::new(&[("a/index.md", "# Index\n\nA.\n"), ("b/Index.md", "# Index\n\nB.\n")]);
    let out = cli(&v, &["read", "index"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "\"index\" matches 2 notes. Call again with one id:\n  a/index\n  b/Index\n"
    );
    let out = cli(&v, &["read", "nothing"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&out.stderr), "error: \"nothing\" not found\n");
}

#[test]
fn cli_needs_a_note_and_takes_one() {
    let v = TestVault::new(&[("a/N.md", "# N\n\nBody.\n")]);
    assert_eq!(cli(&v, &["read"]).status.code(), Some(2));
    assert_eq!(cli(&v, &["read", "a/N", "extra"]).status.code(), Some(2));
}

/// The CLI reads `read.max_tokens` from the config file; without it the default applies.
#[test]
fn cli_applies_the_configured_limit() {
    let long = format!("# Long\n\n{}\n", "word ".repeat(2000));
    let v = TestVault::new(&[("a/Long.md", long.as_str())]);
    let out = cli(&v, &["read", "a/Long"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    config::set(&v.config, "read.max_tokens", "1000").unwrap();
    let out = cli(&v, &["read", "a/Long"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with(
            "a/Long
"
        ),
        "no checksum on a cut read: {text}"
    );
    assert!(text.trim_end().ends_with("cannot be read in parts.]"), "{text}");
    assert!(text.contains("[cut at 1000 tokens."), "{text}");
    assert!(tok(&text) <= 1000);
}

/// Shadow mode: no code path opens a vault file for writing.
#[test]
fn reading_leaves_the_vault_untouched() {
    let v = TestVault::new(&[("a/N.md", "# N\n\nBody.\n")]);
    let before = fs::metadata(v.root.join("a/N.md")).unwrap().modified().unwrap();
    let out = cli(&v, &["read", "a/N"]);
    assert!(out.status.success());
    assert_eq!(fs::read(v.root.join("a/N.md")).unwrap(), b"# N\n\nBody.\n");
    assert_eq!(fs::metadata(v.root.join("a/N.md")).unwrap().modified().unwrap(), before);
}

// ---------------------------------------------------------------------------------------------------------------
// The cut (doc 07 §7.2) and one section (§7.3).

fn filler(chars: usize) -> String {
    "filler ".repeat(chars / 7)
}

/// A level-2 section that ends with a blank line.
fn sec(name: &str, lead: &str, chars: usize) -> String {
    format!("## {name}\n\n{lead}{}\n\n", filler(chars))
}

fn sum(s: &str) -> String {
    sha256_prefix(s.as_bytes(), 12)
}

/// The closing line of doc 07 §7.2, written out from its parts (`shown`: name and body of each shown section in
/// file order; `hidden`: names, best match first; `tail`: the part that follows, if any).
fn closing(limit: usize, shown: &[(&str, &str)], hidden: &[&str], tail: &str) -> String {
    let mut line = format!("[cut at {limit} tokens.");
    if !shown.is_empty() {
        let items: Vec<String> = shown.iter().map(|(n, b)| format!("\"{n}\" {}", sum(b))).collect();
        line.push_str(&format!(" Shown, with checksums: {}.", items.join(", ")));
    }
    if !hidden.is_empty() {
        let items: Vec<String> = hidden.iter().map(|n| format!("\"{n}\"")).collect();
        line.push_str(&format!(
            " Not shown: {}. Read one with section: \"{}\"",
            items.join(", "),
            hidden[0]
        ));
        if !tail.is_empty() {
            line.push('.');
        }
    }
    if !tail.is_empty() {
        line.push(' ');
        line.push_str(tail);
    }
    line.push_str("]\n");
    line
}

const HEAD: &str = "---\ntype: hub\n---\n# Decisions\n\nDurable decisions.\n\n";
const PART_OF_HEAD: &str = "Part of the head is not shown; it cannot be read in parts.";

fn vault_with(name: &str, text: &str) -> (TestVault, Index) {
    let v = TestVault::new(&[(name, text)]);
    let index = v.index();
    (v, index)
}

/// The doc 07 §7.2 example, at the default limit: two sections fit, two do not, and the line has no whole-note
/// checksum.
#[test]
fn a_long_note_is_cut_with_the_closing_line_of_the_doc() {
    let s = [
        sec("Storage", "", 6000),
        sec("Search", "", 6000),
        sec("Index", "", 6000),
        sec("Notes", "", 6000),
    ];
    let text = format!("{HEAD}{}", s.concat());
    let (_v, index) = vault_with("recognitor/Decisions.md", &text);
    let out = text_of(&index, "recognitor/Decisions");
    let expected = format!(
        "recognitor/Decisions\n{HEAD}{}{}{}",
        s[0],
        s[1],
        closing(4000, &[("Storage", &s[0]), ("Search", &s[1])], &["Index", "Notes"], "")
    );
    assert_eq!(out, expected);
    assert!(
        out.ends_with(&format!(
            "[cut at 4000 tokens. Shown, with checksums: \"Storage\" {}, \"Search\" {}. Not shown: \"Index\", \"Notes\". Read one with section: \"Index\"]\n",
            sum(&s[0]),
            sum(&s[1])
        ))
    );
    assert!(tok(&out) <= 4000);
    assert!(matches!(
        rd(&index, "recognitor/Decisions", None, None, 4000).unwrap(),
        Read::Cut(_)
    ));
}

/// §7.2 "Fill": a section that does not fit is skipped and the next one is tried.
#[test]
fn a_section_that_does_not_fit_is_skipped_and_a_later_one_is_taken() {
    let s = [
        sec("A", "", 1400),
        sec("B", "", 5000),
        sec("C", "", 1400),
        sec("D", "", 1400),
    ];
    let text = format!("{HEAD}{}", s.concat());
    let (_v, index) = vault_with("a/Decisions.md", &text);
    let out = read::render(&rd(&index, "a/Decisions", None, None, 1000).unwrap());
    let expected = format!(
        "a/Decisions\n{HEAD}{}{}{}",
        s[0],
        s[2],
        closing(1000, &[("A", &s[0]), ("C", &s[2])], &["B", "D"], "")
    );
    assert_eq!(out, expected);
    assert!(tok(&out) <= 1000);
}

/// §7.2 "Ranking": every query word first, then any, then file order; sections show in file order.
#[test]
fn query_ranks_sections_and_they_show_in_file_order() {
    let s = [
        sec("P", "alpha ", 1400),
        sec("Q", "alpha beta ", 1400),
        sec("R", "", 1400),
        sec("S", "", 1400),
    ];
    let text = format!("{HEAD}{}", s.concat());
    let (_v, index) = vault_with("a/Decisions.md", &text);
    let out = read::render(&rd(&index, "a/Decisions", None, Some("alpha beta"), 1000).unwrap());
    // Room for two: Q (every word) and P (one word); R and S follow in file order. P is shown before Q, as in the file.
    assert_eq!(
        out,
        format!(
            "a/Decisions\n{HEAD}{}{}{}",
            s[0],
            s[1],
            closing(1000, &[("P", &s[0]), ("Q", &s[1])], &["R", "S"], "")
        )
    );
    // Room for one: Q alone, and the others are named best match first.
    let out = read::render(&rd(&index, "a/Decisions", None, Some("alpha beta"), 600).unwrap());
    assert_eq!(
        out,
        format!(
            "a/Decisions\n{HEAD}{}{}",
            s[1],
            closing(600, &[("Q", &s[1])], &["P", "R", "S"], "")
        )
    );
    // Without a query, file order.
    let out = read::render(&rd(&index, "a/Decisions", None, None, 600).unwrap());
    assert_eq!(
        out,
        format!(
            "a/Decisions\n{HEAD}{}{}",
            s[0],
            closing(600, &[("P", &s[0])], &["Q", "R", "S"], "")
        )
    );
}

/// §7.2 "Ranking": the query follows search's rules, and a word matches its other forms as in search.
#[test]
fn the_query_matches_word_forms_and_a_query_without_words_counts_as_none() {
    let s = [sec("A", "", 1400), sec("B", "running ", 1400), sec("C", "", 1400)];
    let text = format!("{HEAD}{}", s.concat());
    let (_v, index) = vault_with("a/N.md", &text);
    let stemmed = read::render(&rd(&index, "a/N", None, Some("runs"), 600).unwrap());
    assert!(stemmed.contains(&format!("\"B\" {}", sum(&s[1]))), "{stemmed}");
    let none = read::render(&rd(&index, "a/N", None, None, 600).unwrap());
    for q in ["", "  ", "++ --", "\""] {
        assert_eq!(
            read::render(&rd(&index, "a/N", None, Some(q), 600).unwrap()),
            none,
            "{q:?}"
        );
    }
}

/// §7.2: `query` is used only when the note is too long.
#[test]
fn the_query_does_not_change_a_note_that_fits() {
    let (_v, index) = vault_with("a/N.md", "# N\n\nSum.\n\n## A\n\nalpha\n\n## B\n\nbeta\n");
    assert_eq!(
        read::render(&rd(&index, "a/N", None, Some("beta"), DEFAULT_MAX_TOKENS).unwrap()),
        text_of(&index, "a/N")
    );
}

/// The whole output, header and closing line included, stays within the limit.
#[test]
fn a_cut_read_stays_within_the_limit_at_any_limit() {
    let mut text = String::from(HEAD);
    for i in 0..12 {
        text.push_str(&sec(
            &format!("S{i}"),
            if i % 3 == 0 { "alpha " } else { "" },
            300 + i * 150,
        ));
    }
    let (_v, index) = vault_with("a/N.md", &text);
    for limit in (150..1500).step_by(37) {
        for query in [None, Some("alpha")] {
            let r = rd(&index, "a/N", None, query, limit).unwrap();
            let out = read::render(&r);
            if matches!(r, Read::Cut(_)) {
                assert!(tok(&out) <= limit, "limit {limit}: {} tokens", tok(&out));
                assert!(out.contains(&format!("[cut at {limit} tokens.")), "{out}");
            }
        }
    }
}

/// §7.2 "A head over the limit": the frontmatter stays whole, then block by block; the first block that does not
/// fit ends the head. A note with sections says part of the head is not shown.
#[test]
fn a_head_over_the_limit_is_cut_block_by_block() {
    let big = filler(1600);
    let text = format!(
        "---\ntype: hub\n---\n# T\n\nShort summary.\n\n{big}\n\nLast block.\n\n{}{}",
        sec("A", "", 2000),
        sec("B", "", 2000)
    );
    let (_v, index) = vault_with("a/T.md", &text);
    let out = read::render(&rd(&index, "a/T", None, None, 100).unwrap());
    let expected = format!(
        "a/T\n---\ntype: hub\n---\n# T\n\nShort summary.\n\n{}",
        closing(100, &[], &["A", "B"], PART_OF_HEAD)
    );
    assert_eq!(out, expected);
    assert!(
        out.ends_with("Read one with section: \"A\". Part of the head is not shown; it cannot be read in parts.]\n")
    );
    assert!(tok(&out) <= 100);
}

/// §7.2: a note with no headings cannot be read in parts, and says so.
#[test]
fn a_note_without_headings_says_it_cannot_be_read_in_parts() {
    let text = format!("# N\n\nFirst para short.\n\n{}\n", filler(1500));
    let (_v, index) = vault_with("a/N.md", &text);
    let out = read::render(&rd(&index, "a/N", None, None, 100).unwrap());
    assert_eq!(
        out,
        "a/N\n# N\n\nFirst para short.\n\n[cut at 100 tokens. The rest has no headings, so it cannot be read in parts.]\n"
    );
}

/// §7.2 "A head over the limit": the frontmatter stays whole even when it alone is over the limit.
#[test]
fn the_frontmatter_stays_whole_when_it_is_over_the_limit() {
    let fm = format!(
        "---\ntags: [{}]\n---\n",
        filler(2000).replace(' ', ", ").trim_end_matches(", ")
    );
    let text = format!("{fm}# N\n\nSum.\n\n{}", sec("A", "", 300));
    let (_v, index) = vault_with("a/N.md", &text);
    let out = read::render(&rd(&index, "a/N", None, None, 100).unwrap());
    assert!(out.starts_with(&format!("a/N\n{fm}")), "{out}");
    assert!(!out.contains("# N"), "{out}");
    assert!(out.contains("[cut at 100 tokens."), "{out}");
}

/// §7.2: no whole-note checksum on a cut read; the header carries the id, and the title when it differs.
#[test]
fn a_cut_read_has_no_checksum_in_its_header() {
    let text = format!("# Tools & sourcing\n\nSum.\n\n{}", sec("A", "", 3000));
    let (_v, index) = vault_with("a/Tools.md", &text);
    let out = read::render(&rd(&index, "a/Tools", None, None, 100).unwrap());
    assert!(
        out.starts_with("a/Tools, titled \"Tools & sourcing\"\n# Tools"),
        "{out}"
    );
    assert!(!out.lines().next().unwrap().contains("checksum"));
}

/// §7.2: a heading-shaped line in code is text, not a section.
#[test]
fn headings_in_code_do_not_make_sections() {
    let text = format!("# N\n\nSum.\n\n```\n## Fake\n```\n\n{}", sec("Real", "", 3000));
    let (_v, index) = vault_with("a/N.md", &text);
    let out = read::render(&rd(&index, "a/N", None, None, 100).unwrap());
    assert!(out.contains("Not shown: \"Real\"."), "{out}");
    assert!(!out.contains("Fake\""), "{out}");
}

// One section (§7.3).

const SECTIONS: &str = "# Decisions\n\nDurable decisions.\n\n## Storage\nStorage text.\n### Write lock\nLock text.\n\n## Search\nSearch text.\n### Notes\nSearch notes.\n## Other\n### Notes\nOther notes.\n";

#[test]
fn one_section_is_returned_with_its_checksum_and_its_subsections() {
    let (_v, index) = vault_with("recognitor/Decisions.md", SECTIONS);
    let body = "## Storage\nStorage text.\n### Write lock\nLock text.\n\n";
    let expected = format!("recognitor/Decisions > Storage  checksum: {}\n{body}", sum(body));
    let r = rd(
        &index,
        "recognitor/Decisions",
        Some("Storage"),
        None,
        DEFAULT_MAX_TOKENS,
    )
    .unwrap();
    assert!(matches!(r, Read::Whole(_)));
    assert_eq!(read::render(&r), expected);
    // A subsection ends at the next heading of its level or a higher one; the last section runs to the end.
    let r = rd(
        &index,
        "recognitor/Decisions",
        Some("Storage > Write lock"),
        None,
        DEFAULT_MAX_TOKENS,
    )
    .unwrap();
    let body = "### Write lock\nLock text.\n\n";
    assert_eq!(
        read::render(&r),
        format!(
            "recognitor/Decisions > Storage > Write lock  checksum: {}\n{body}",
            sum(body)
        )
    );
    let r = rd(&index, "recognitor/Decisions", Some("Other"), None, DEFAULT_MAX_TOKENS).unwrap();
    let body = "## Other\n### Notes\nOther notes.\n";
    assert_eq!(
        read::render(&r),
        format!("recognitor/Decisions > Other  checksum: {}\n{body}", sum(body))
    );
}

/// §7.3 "Matching": lowercase, without `#` marks, a name at any level or a path from a top-level section.
#[test]
fn a_section_is_named_by_its_heading_text_in_any_case_without_marks() {
    let (_v, index) = vault_with("a/Decisions.md", SECTIONS);
    let expected = read::render(&rd(&index, "a/Decisions", Some("Storage"), None, DEFAULT_MAX_TOKENS).unwrap());
    for name in ["storage", "STORAGE", "## Storage", "## storage", "  # Storage "] {
        assert_eq!(
            read::render(&rd(&index, "a/Decisions", Some(name), None, DEFAULT_MAX_TOKENS).unwrap()),
            expected,
            "{name}"
        );
    }
    // A heading inside another is found by its own text when that is unique, and by its path always.
    let by_name = read::render(&rd(&index, "a/Decisions", Some("write lock"), None, DEFAULT_MAX_TOKENS).unwrap());
    let by_path = read::render(
        &rd(
            &index,
            "a/Decisions",
            Some("storage > Write lock"),
            None,
            DEFAULT_MAX_TOKENS,
        )
        .unwrap(),
    );
    assert_eq!(by_name, by_path);
    assert!(
        by_name.starts_with("a/Decisions > Storage > Write lock  checksum: "),
        "{by_name}"
    );
}

#[test]
fn several_matching_headings_return_their_paths_and_no_section() {
    let (_v, index) = vault_with("a/Decisions.md", SECTIONS);
    let r = rd(&index, "a/Decisions", Some("Notes"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(matches!(r, Read::Sections { .. }));
    assert_eq!(
        read::render(&r),
        "\"Notes\" matches 2 sections in a/Decisions. Call again with one:\n  Search > Notes\n  Other > Notes\n"
    );
    // The path is the way out.
    let r = rd(&index, "a/Decisions", Some("Other > Notes"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(read::render(&r).starts_with("a/Decisions > Other > Notes  checksum: "));
}

#[test]
fn the_id_in_the_several_sections_line_carries_its_title() {
    let (_v, index) = vault_with("a/N.md", "# Name\n\nSum.\n\n## X\n\n### Y\n\n## Z\n\n### Y\n");
    let text = read::render(&rd(&index, "a/N", Some("y"), None, DEFAULT_MAX_TOKENS).unwrap());
    assert!(
        text.starts_with("\"y\" matches 2 sections in a/N, titled \"Name\". Call again with one:\n"),
        "{text}"
    );
}

/// §7.3 "No match": an error that lists the top-level sections. The title of the note is not a section.
#[test]
fn a_section_that_matches_nothing_is_an_error_listing_the_sections() {
    let (_v, index) = vault_with("a/Decisions.md", SECTIONS);
    for name in ["Storge", "Decisions", "Storage > Nothing", "Write lock > Storage"] {
        let err = rd(&index, "a/Decisions", Some(name), None, DEFAULT_MAX_TOKENS).unwrap_err();
        assert!(matches!(err, Error::NoSection { .. }));
        assert_eq!(
            err.to_string(),
            format!("No section \"{name}\" in a/Decisions. Sections: \"Storage\", \"Search\", \"Other\".")
        );
    }
}

/// §7.3 "Section checksum": the bytes of the body, so an edit inside changes it and an edit outside does not. The
/// section comes from the file, as the whole note does.
#[test]
fn the_section_checksum_follows_the_sections_bytes() {
    let (v, index) = vault_with("a/Decisions.md", SECTIONS);
    let before = text_of_section(&index, "Storage");
    v.write(
        "a/Decisions.md",
        SECTIONS.replace("Search text.", "Search text, edited.").as_bytes(),
    );
    assert_eq!(text_of_section(&index, "Storage"), before);
    v.write(
        "a/Decisions.md",
        SECTIONS.replace("Lock text.", "Lock text, edited.").as_bytes(),
    );
    assert_ne!(text_of_section(&index, "Storage"), before);
}

fn text_of_section(index: &Index, section: &str) -> String {
    read::render(&rd(index, "a/Decisions", Some(section), None, DEFAULT_MAX_TOKENS).unwrap())
}

/// §7.3 "Body": exactly as on disk, line endings included; a heading in code is not a boundary.
#[test]
fn the_section_body_is_exact_bytes() {
    let text = "# N\r\n\r\nSum.\r\n\r\n## A\r\n\r\n```\r\n## not\r\n```\r\ntext\r\n\r\n## B\r\n\r\nb";
    let (_v, index) = vault_with("a/N.md", text);
    let body = "## A\r\n\r\n```\r\n## not\r\n```\r\ntext\r\n\r\n";
    assert_eq!(
        read::render(&rd(&index, "a/N", Some("A"), None, DEFAULT_MAX_TOKENS).unwrap()),
        format!("a/N > A  checksum: {}\n{body}", sum(body))
    );
    assert_eq!(
        read::render(&rd(&index, "a/N", Some("B"), None, DEFAULT_MAX_TOKENS).unwrap()),
        format!("a/N > B  checksum: {}\n## B\r\n\r\nb", sum("## B\r\n\r\nb"))
    );
}

/// §7.3 "A section over the limit": cut like a note, one level down, with subsections named by their paths.
#[test]
fn a_section_over_the_limit_is_cut_into_its_subsections() {
    let day1 = format!("### Day 1\n\n{}\n\n", filler(1400));
    let day2 = format!("### Day 2\n\nbeta {}\n\n", filler(1400));
    let intro = "## Verbatim\n\nIntro text.\n\n";
    let text = format!("# Log\n\nSum.\n\n{intro}{day1}{day2}## After\n\nz\n");
    let (_v, index) = vault_with("a/Log.md", &text);
    let r = rd(&index, "a/Log", Some("Verbatim"), None, 600).unwrap();
    assert!(matches!(r, Read::Cut(_)));
    assert_eq!(
        read::render(&r),
        format!(
            "a/Log > Verbatim\n{intro}{day1}{}",
            closing(600, &[("Verbatim > Day 1", &day1)], &["Verbatim > Day 2"], "")
        )
    );
    // The query ranks the subsections as it ranks top-level sections.
    let r = rd(&index, "a/Log", Some("Verbatim"), Some("beta"), 600).unwrap();
    assert_eq!(
        read::render(&r),
        format!(
            "a/Log > Verbatim\n{intro}{day2}{}",
            closing(600, &[("Verbatim > Day 2", &day2)], &["Verbatim > Day 1"], "")
        )
    );
    // The path in the closing line is the one `section` takes back.
    let r = rd(&index, "a/Log", Some("Verbatim > Day 2"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(read::render(&r).starts_with(&format!("a/Log > Verbatim > Day 2  checksum: {}\n", sum(&day2))));
}

/// A section that fits is whole, with its checksum, even in a note that was cut as a whole.
#[test]
fn a_section_that_fits_is_never_cut() {
    let s = [sec("A", "", 3000), sec("B", "", 3000)];
    let text = format!("{HEAD}{}", s.concat());
    let (_v, index) = vault_with("a/N.md", &text);
    assert!(matches!(rd(&index, "a/N", None, None, 400).unwrap(), Read::Cut(_)));
    let r = rd(&index, "a/N", Some("A"), None, 1000).unwrap();
    assert!(matches!(r, Read::Whole(_)));
}

// The CLI: `read <note> --section <heading> --query <text>` (doc 07 §7, §12).

#[test]
fn cli_reads_one_section() {
    let v = TestVault::new(&[("a/Decisions.md", SECTIONS)]);
    let out = cli(&v, &["read", "a/Decisions", "--section", "Storage > Write lock"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body = "### Write lock\nLock text.\n\n";
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("a/Decisions > Storage > Write lock  checksum: {}\n{body}", sum(body))
    );
}

#[test]
fn cli_reports_a_missing_section_on_stderr() {
    let v = TestVault::new(&[("a/Decisions.md", SECTIONS)]);
    let out = cli(&v, &["read", "a/Decisions", "--section", "Storge"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: No section \"Storge\" in a/Decisions. Sections: \"Storage\", \"Search\", \"Other\".\n"
    );
}

#[test]
fn cli_passes_the_query_to_the_cut() {
    let s = [sec("P", "", 1400), sec("Q", "gamma ", 1400), sec("R", "", 1400)];
    let text = format!("{HEAD}{}", s.concat());
    let v = TestVault::new(&[("a/Decisions.md", text.as_str())]);
    config::set(&v.config, "read.max_tokens", "600").unwrap();
    let out = cli(&v, &["read", "a/Decisions", "--query", "gamma"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "a/Decisions\n{HEAD}{}{}",
            s[1],
            closing(600, &[("Q", &s[1])], &["P", "R"], "")
        )
    );
}

#[test]
fn cli_rejects_section_and_query_elsewhere_and_without_a_value() {
    let v = TestVault::new(&[("a/N.md", SECTIONS)]);
    assert_eq!(cli(&v, &["search", "x", "--section", "A"]).status.code(), Some(2));
    assert_eq!(cli(&v, &["context", "a/N", "--query", "A"]).status.code(), Some(2));
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["read", "a/N", "--section"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

// Repeated section names (doc 07 §7.2, §7.3 "Matching"): siblings that share a name are numbered from 1 in file
// order, `Notes [1]`, `Notes [2]`; a unique name stays plain. Numbered names work in paths and in every list.

const TWINS: &str = "# Decisions\n\nSum.\n\n## Notes\nFirst notes.\n## Notes\nSecond notes.\n## Other\nOther text.\n";

#[test]
fn repeated_top_level_names_are_several_matches_with_numbers() {
    let (_v, index) = vault_with("a/Decisions.md", TWINS);
    let r = rd(&index, "a/Decisions", Some("Notes"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(matches!(r, Read::Sections { .. }));
    assert_eq!(
        read::render(&r),
        "\"Notes\" matches 2 sections in a/Decisions. Call again with one:\n  Notes [1]\n  Notes [2]\n"
    );
}

#[test]
fn a_numbered_name_returns_that_section_and_shows_the_numbered_path() {
    let (_v, index) = vault_with("a/Decisions.md", TWINS);
    let body = "## Notes\nSecond notes.\n";
    for name in ["Notes [2]", "notes [2]", "## Notes [2]"] {
        let r = rd(&index, "a/Decisions", Some(name), None, DEFAULT_MAX_TOKENS).unwrap();
        assert_eq!(
            read::render(&r),
            format!("a/Decisions > Notes [2]  checksum: {}\n{body}", sum(body)),
            "{name}"
        );
    }
    let body = "## Notes\nFirst notes.\n";
    let r = rd(&index, "a/Decisions", Some("Notes [1]"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert_eq!(
        read::render(&r),
        format!("a/Decisions > Notes [1]  checksum: {}\n{body}", sum(body))
    );
    // A unique name stays plain, and there is no third one.
    let r = rd(&index, "a/Decisions", Some("Other"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(read::render(&r).starts_with("a/Decisions > Other  checksum: "));
    assert!(matches!(
        rd(&index, "a/Decisions", Some("Notes [3]"), None, DEFAULT_MAX_TOKENS),
        Err(Error::NoSection { .. })
    ));
}

#[test]
fn repeats_under_one_parent_are_numbered_among_their_siblings_only() {
    let text = "# Decisions\n\nSum.\n\n## Storage\n### Notes\nA.\n### Notes\nB.\n## Search\n### Notes\nC.\n";
    let (_v, index) = vault_with("a/Decisions.md", text);
    let r = rd(&index, "a/Decisions", Some("Notes"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert_eq!(
        read::render(&r),
        "\"Notes\" matches 3 sections in a/Decisions. Call again with one:\n  Storage > Notes [1]\n  Storage > Notes [2]\n  Search > Notes\n"
    );
    let body = "### Notes\nB.\n";
    let r = rd(
        &index,
        "a/Decisions",
        Some("Storage > Notes [2]"),
        None,
        DEFAULT_MAX_TOKENS,
    )
    .unwrap();
    assert_eq!(
        read::render(&r),
        format!("a/Decisions > Storage > Notes [2]  checksum: {}\n{body}", sum(body))
    );
    // The same name under another parent is its own, plain, section.
    let r = rd(&index, "a/Decisions", Some("Search > Notes"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(read::render(&r).starts_with("a/Decisions > Search > Notes  checksum: "));
}

/// Names that match by lowercase share a name, as they do when `section` is matched.
#[test]
fn names_that_differ_only_in_case_are_numbered_too() {
    let text = "# Decisions\n\nSum.\n\n## Notes\nA.\n## notes\nB.\n";
    let (_v, index) = vault_with("a/Decisions.md", text);
    let r = rd(&index, "a/Decisions", Some("NOTES"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert_eq!(
        read::render(&r),
        "\"NOTES\" matches 2 sections in a/Decisions. Call again with one:\n  Notes [1]\n  notes [2]\n"
    );
}

#[test]
fn the_no_match_list_shows_numbered_names() {
    let (_v, index) = vault_with("a/Decisions.md", TWINS);
    let err = rd(&index, "a/Decisions", Some("Nope"), None, DEFAULT_MAX_TOKENS).unwrap_err();
    assert_eq!(
        err.to_string(),
        "No section \"Nope\" in a/Decisions. Sections: \"Notes [1]\", \"Notes [2]\", \"Other\"."
    );
}

/// The closing line names repeated sections with their numbers, and the name it hints is one `section` takes back.
#[test]
fn the_closing_line_shows_numbered_names() {
    let s = [
        sec("Notes", "", 1400),
        sec("Notes", "", 1400),
        sec("Notes", "", 1400),
        sec("Plain", "", 1400),
    ];
    let text = format!("{HEAD}{}", s.concat());
    let (_v, index) = vault_with("a/Decisions.md", &text);
    let out = read::render(&rd(&index, "a/Decisions", None, None, 1000).unwrap());
    assert_eq!(
        out,
        format!(
            "a/Decisions\n{HEAD}{}{}{}",
            s[0],
            s[1],
            closing(
                1000,
                &[("Notes [1]", &s[0]), ("Notes [2]", &s[1])],
                &["Notes [3]", "Plain"],
                ""
            )
        )
    );
    let r = rd(&index, "a/Decisions", Some("Notes [3]"), None, DEFAULT_MAX_TOKENS).unwrap();
    assert!(read::render(&r).starts_with(&format!("a/Decisions > Notes [3]  checksum: {}\n", sum(&s[2]))));
}

/// A section over the limit names repeated subsections by their numbered paths.
#[test]
fn repeated_subsections_are_numbered_in_a_cut_section() {
    let day = |n: &str| format!("### Day\n\n{n} {}\n\n", filler(1400));
    let (d1, d2) = (day("one"), day("two"));
    let intro = "## Verbatim\n\nIntro.\n\n";
    let text = format!("# Log\n\nSum.\n\n{intro}{d1}{d2}");
    let (_v, index) = vault_with("a/Log.md", &text);
    let r = rd(&index, "a/Log", Some("Verbatim"), None, 600).unwrap();
    assert_eq!(
        read::render(&r),
        format!(
            "a/Log > Verbatim\n{intro}{d1}{}",
            closing(600, &[("Verbatim > Day [1]", &d1)], &["Verbatim > Day [2]"], "")
        )
    );
}

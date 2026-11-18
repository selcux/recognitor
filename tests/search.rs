//! Search tests (doc 07 §4, doc 05 §3.7). Vaults are synthetic and live in temp folders that delete themselves.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::db::{self, meta_keys};
use recognitor::search::{self, CANDIDATES, PAGE_SIZE, SearchPage};
use recognitor::{Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-search-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let v = TestVault {
            root,
            db: base.join("index.db"),
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

    /// An index whose sweep has run, so queries see the files.
    fn index(&self) -> Index {
        let mut index = Index::open(&self.root, Some(&self.db)).unwrap();
        index.rebuild(SweepOptions::default()).unwrap();
        index
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

fn run(index: &Index, query: &str, ns: Option<&str>, page: usize) -> String {
    search::render(&search::search(index.connection(), query, ns, page).unwrap())
}

fn page_of(index: &Index, query: &str, ns: Option<&str>, page: usize) -> SearchPage {
    search::search(index.connection(), query, ns, page).unwrap()
}

fn ids(page: &SearchPage) -> Vec<&str> {
    page.hits.iter().map(|h| h.id.as_str()).collect()
}

/// `count` notes `{ns}/N{i}` that each hold `term` once in a body of filler.
fn many(ns: &str, count: usize, term: &str) -> Vec<(String, String)> {
    (0..count)
        .map(|i| {
            (
                format!("{ns}/N{i:03}.md"),
                format!("# N{i:03}\n\nPlain summary {i}.\n\nSome filler words around {term} in note {i}.\n"),
            )
        })
        .collect()
}

fn vault_of(files: &[(String, String)]) -> TestVault {
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    TestVault::new(&refs)
}

/// Doc 07 §4.2, the example block, line for line: id, summary, matching observation, "N more: page 2". The page
/// size is 2 here because the example shows two hits and "3 more".
#[test]
fn the_doc_example_block() {
    let v = TestVault::new(&[
        (
            "recognitor/Decisions.md",
            "# Decisions\n\nDurable decisions for the engine, newest first.\n\n\
             - [decision] Writes take the SQLite write lock before touching a file\n",
        ),
        (
            "compass/concepts/Feedback loops.md",
            "# Feedback loops\n\nWhat you build reacts to its own results.\n\n\
             A change shapes what the next change sees, and each write feeds back into the next write in turn.\n",
        ),
        (
            "compass/concepts/A.md",
            "# A\n\nAlpha.\n\nA short remark about how a write lands.\n\nMore filler text here.\n",
        ),
        (
            "compass/concepts/B.md",
            "# B\n\nBeta.\n\nA short remark about how a write lands.\n\nMore filler text here.\n",
        ),
        (
            "compass/concepts/C.md",
            "# C\n\nGamma.\n\nA short remark about how a write lands.\n\nMore filler text here.\n",
        ),
    ]);
    let index = v.index();
    let page = search::search_with_page_size(index.connection(), "write", None, 1, 2).unwrap();
    assert_eq!(
        search::render(&page),
        "1. recognitor/Decisions\n\
         \x20  Durable decisions for the engine, newest first.\n\
         \x20  - [decision] Writes take the SQLite write lock before touching a file\n\
         2. compass/concepts/Feedback loops\n\
         \x20  What you build reacts to its own results.\n\
         3 more: page 2\n"
    );
}

/// Doc 07 §4.2 line 1: the title is added only when it differs from the filename by key; `Index.md` titled `Index`
/// shows none, and a different case still counts as the same key. The form is `<id>, titled "<title>"`.
#[test]
fn title_shows_only_when_it_differs_by_key() {
    let v = TestVault::new(&[
        ("common/Index.md", "# Index\n\nMarker zebra here.\n"),
        ("common/lowercase name.md", "# Lowercase Name\n\nMarker zebra here.\n"),
        ("common/File name.md", "# A Different Title\n\nMarker zebra here.\n"),
    ]);
    let index = v.index();
    let page = page_of(&index, "zebra", None, 1);
    let mut got: Vec<(String, Option<String>)> = page.hits.iter().map(|h| (h.id.clone(), h.title.clone())).collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("common/File name".to_string(), Some("A Different Title".to_string())),
            ("common/Index".to_string(), None),
            ("common/lowercase name".to_string(), None),
        ]
    );
    let text = search::render(&page);
    assert!(
        text.contains(". common/File name, titled \"A Different Title\"\n"),
        "{text}"
    );
    assert!(text.contains(". common/Index\n"), "{text}");
    assert!(text.contains(". common/lowercase name\n"), "{text}");
}

/// Doc 07 §4.2 line 1: the title is printed as written. Parentheses and quotes inside it are not changed or escaped.
#[test]
fn a_title_is_printed_as_written() {
    let v = TestVault::new(&[
        (
            "common/Note a.md",
            "# Cost (and benefit) of change\n\nMarker zebra here.\n",
        ),
        ("common/Note b.md", "# The \"quoted\" one\n\nMarker zebra here.\n"),
    ]);
    let index = v.index();
    let text = run(&index, "zebra", None, 1);
    assert!(
        text.contains(". common/Note a, titled \"Cost (and benefit) of change\"\n"),
        "{text}"
    );
    assert!(
        text.contains(". common/Note b, titled \"The \"quoted\" one\"\n"),
        "{text}"
    );
}

/// The shared id form, for the commands that print ids.
#[test]
fn titled_id_form() {
    use recognitor::paths::titled_id;
    assert_eq!(titled_id("common/Index", None), "common/Index");
    assert_eq!(titled_id("common/Index", Some("Home")), "common/Index, titled \"Home\"");
}

/// Doc 07 §4.2 line 2: a note without a summary has no summary line.
#[test]
fn no_summary_no_line() {
    let v = TestVault::new(&[("common/Bare.md", "Marker zebra but no heading.\n")]);
    let index = v.index();
    assert_eq!(run(&index, "zebra", None, 1), "1. common/Bare\n");
}

/// Doc 07 §4.2 "Below": up to 3 observation lines that matched the query, best match first. Of four matching lines
/// the shortest (the best BM25 score) comes first and the weakest is dropped.
#[test]
fn up_to_three_matching_observation_lines() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout supplements.\n\n\
         - [fact] Creatine is a supplement #health\n\
         - [fact] Unrelated line\n\
         - [Rule] Take supplements with food\n\
         - [fact] Fish oil supplement notes\n\
         - [fact] Fifth supplement line\n",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "supplement", None, 1),
        "1. common/Obs\n\
         \x20  About supplements.\n\
         \x20  - [fact] Fifth supplement line\n\
         \x20  - [rule] Take supplements with food\n\
         \x20  - [fact] Fish oil supplement notes\n"
    );
}

/// Doc 07 §4.2 "Below": best match first, not file order; equal scores go by line number.
#[test]
fn observation_lines_best_match_first() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout zebras.\n\n\
         - [fact] A zebra stands somewhere among many other plain words that dilute the match a lot\n\
         - [fact] Zebra zebra zebra\n\
         - [fact] Zebra one\n\
         - [fact] Zebra two\n",
    )]);
    let index = v.index();
    let lines: Vec<String> = run(&index, "zebra", None, 1)
        .lines()
        .skip(2)
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        [
            "   - [fact] Zebra zebra zebra",
            "   - [fact] Zebra one",
            "   - [fact] Zebra two"
        ]
    );
}

/// Doc 07 §4.2 "Below": a line reads `- [category] @qualifier text` when the fact has a qualifier, as written, and
/// `- [category] text` when it has none. Tags stay in the text.
#[test]
fn a_qualifier_is_shown_after_the_category() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout rules.\n\n\
         - [decision] @effective:2026-11-11 Zebra rule starts #policy\n\
         - [rule] @valid[2026-01-01,2026-07-01) Zebra rule for the first half\n\
         - [fact] Zebra rule without a date\n",
    )]);
    let index = v.index();
    let text = run(&index, "zebra rule", None, 1);
    assert!(
        text.contains("   - [decision] @effective:2026-11-11 Zebra rule starts #policy\n"),
        "{text}"
    );
    assert!(
        text.contains("   - [rule] @valid[2026-01-01,2026-07-01) Zebra rule for the first half\n"),
        "{text}"
    );
    assert!(text.contains("   - [fact] Zebra rule without a date\n"), "{text}");
}

/// Doc 07 §4.2 "Below": when no line of the note holds every word, the lines that hold any word are shown.
#[test]
fn observation_lines_fall_back_to_any_word() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout two things.\n\n\
         - [fact] Zebra is one thing\n\
         - [fact] Unrelated line\n\
         - [fact] Giraffe is another thing\n",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "zebra giraffe", None, 1),
        "1. common/Obs\n\
         \x20  About two things.\n\
         \x20  - [fact] Zebra is one thing\n\
         \x20  - [fact] Giraffe is another thing\n"
    );
}

/// Doc 07 §4.2 "Below": lines that hold every word win; the lines with only some words are left out.
#[test]
fn lines_with_every_word_are_preferred() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout two things.\n\n\
         - [fact] Zebra alone\n\
         - [fact] Zebra and giraffe together\n\
         - [fact] Giraffe alone\n",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "zebra giraffe", None, 1),
        "1. common/Obs\n\
         \x20  About two things.\n\
         \x20  - [fact] Zebra and giraffe together\n"
    );
}

/// Doc 07 §4.2 "Below": a hit with no matching observation shows none, even when the note has observations.
#[test]
fn observations_that_do_not_match_are_left_out() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout zebras.\n\n- [fact] Something else entirely\n",
    )]);
    let index = v.index();
    assert_eq!(run(&index, "zebras", None, 1), "1. common/Obs\n   About zebras.\n");
}

/// Doc 07 §4 and §1.1: bootstrap notes never match; archive notes match only when `ns` is `archive`.
#[test]
fn bootstrap_never_archive_only_on_request() {
    let v = TestVault::new(&[
        ("bootstrap/Rules.md", "# Rules\n\nMarker zebra.\n"),
        ("archive/Old session.md", "# Old session\n\nMarker zebra.\n"),
        ("common/Live.md", "# Live\n\nMarker zebra.\n"),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "zebra", None, 1)), ["common/Live"]);
    assert_eq!(ids(&page_of(&index, "zebra", Some("common"), 1)), ["common/Live"]);
    let archive = page_of(&index, "zebra", Some("archive"), 1);
    let mut got = ids(&archive);
    got.sort();
    assert_eq!(got, ["archive/Old session", "common/Live"]);
    assert!(run(&index, "zebra", Some("archive"), 1).contains("archive/Old session"));
}

/// Doc 07 §4: `ns` is a perspective, not a filter. Notes of other namespaces still match.
#[test]
fn ns_is_not_a_filter() {
    let v = TestVault::new(&[
        ("alpha/One.md", "# One\n\nMarker zebra.\n"),
        ("beta/Two.md", "# Two\n\nMarker zebra.\n"),
    ]);
    let index = v.index();
    let page = page_of(&index, "zebra", Some("alpha"), 1);
    assert_eq!(page.hits.len(), 2);
}

/// Doc 07 §4.1: notes in `ns` rank higher than the same text outside it.
#[test]
fn ns_boost_lifts_notes_in_the_namespace() {
    let v = TestVault::new(&[
        ("aaa/Plain.md", "# Plain\n\nMarker zebra.\n"),
        ("bbb/Same.md", "# Same\n\nMarker zebra.\n"),
    ]);
    let index = v.index();
    // Without `ns` the tie breaks by key; with `ns` the note in it comes first.
    assert_eq!(ids(&page_of(&index, "zebra", None, 1)), ["aaa/Plain", "bbb/Same"]);
    assert_eq!(
        ids(&page_of(&index, "zebra", Some("bbb"), 1)),
        ["bbb/Same", "aaa/Plain"]
    );
    assert_eq!(
        ids(&page_of(&index, "zebra", Some("BBB"), 1)),
        ["bbb/Same", "aaa/Plain"]
    );
}

/// Doc 07 §4.1: notes that a note in `ns` links to rank higher than the same text elsewhere.
#[test]
fn ns_boost_lifts_notes_linked_from_the_namespace() {
    let v = TestVault::new(&[
        ("aaa/Plain.md", "# Plain\n\nMarker zebra.\n"),
        ("bbb/Linked.md", "# Linked\n\nMarker zebra.\n"),
        ("ccc/Home.md", "# Home\n\nSee [[bbb/Linked]] for more.\n"),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "zebra", None, 1)), ["aaa/Plain", "bbb/Linked"]);
    assert_eq!(
        ids(&page_of(&index, "zebra", Some("ccc"), 1)),
        ["bbb/Linked", "aaa/Plain"]
    );
}

/// Doc 07 §4.2 "Paging": 5 hits per page; the last line says how many more exist.
#[test]
fn paging_five_per_page() {
    let v = vault_of(&many("common", 13, "zebra"));
    let index = v.index();
    assert_eq!(PAGE_SIZE, 5);

    let p1 = page_of(&index, "zebra", None, 1);
    assert_eq!((p1.hits.len(), p1.more), (5, 8));
    assert!(search::render(&p1).ends_with("8 more: page 2\n"));

    let p2 = page_of(&index, "zebra", None, 2);
    assert_eq!((p2.hits.len(), p2.more), (5, 3));
    assert!(search::render(&p2).ends_with("3 more: page 3\n"));

    let p3 = page_of(&index, "zebra", None, 3);
    assert_eq!((p3.hits.len(), p3.more), (3, 0));
    assert!(!search::render(&p3).contains("more: page"));

    // Pages do not overlap and cover every hit once.
    let mut all: Vec<String> = [p1, p2, p3]
        .iter()
        .flat_map(|p| p.hits.iter().map(|h| h.id.clone()))
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 13);
}

const NOTICE_60: &str = "Only the best 50 of 60 matches are kept; narrow the query to reach the rest.";

/// Doc 05 §3.7 and doc 07 §4.2 "Kept matches": `LIMIT 50`, so a query keeps 50 hits. When more notes match, every
/// page ends by saying so, after its "more" line if it has one, as its own line if not.
#[test]
fn over_fifty_matches_say_so_on_every_page() {
    let v = vault_of(&many("common", 60, "zebra"));
    let index = v.index();
    assert_eq!(CANDIDATES, 50);
    let p1 = page_of(&index, "zebra", None, 1);
    assert_eq!((p1.more, p1.total, p1.matches), (45, 50, Some(60)));
    assert!(search::render(&p1).ends_with(&format!("45 more: page 2. {NOTICE_60}\n")));
    let p4 = search::render(&page_of(&index, "zebra", None, 4));
    assert!(p4.ends_with(&format!("30 more: page 5. {NOTICE_60}\n")), "{p4}");
    // The last page has no "more" line, so the notice is a line of its own.
    let p10 = search::render(&page_of(&index, "zebra", None, 10));
    assert!(p10.ends_with(&format!("\n{NOTICE_60}\n")), "{p10}");
    assert!(!p10.contains("more: page"), "{p10}");
    assert_eq!(page_of(&index, "zebra", None, 10).hits.len(), 5);
    assert!(page_of(&index, "zebra", None, 11).hits.is_empty());
}

/// Doc 07 §4.2 "Kept matches": exactly 50 matches are all kept, so there is no notice.
#[test]
fn exactly_fifty_matches_have_no_notice() {
    let v = vault_of(&many("common", 50, "zebra"));
    let index = v.index();
    let p1 = page_of(&index, "zebra", None, 1);
    assert_eq!((p1.more, p1.total, p1.matches), (45, 50, None));
    assert!(search::render(&p1).ends_with("45 more: page 2\n"));
    assert!(!search::render(&page_of(&index, "zebra", None, 10)).contains("Only the best"));
}

/// Doc 07 §4.2 "Kept matches": fewer than 50 matches have no notice and no count.
#[test]
fn fewer_than_fifty_matches_have_no_notice() {
    let v = vault_of(&many("common", 49, "zebra"));
    let index = v.index();
    let p1 = page_of(&index, "zebra", None, 1);
    assert_eq!(p1.matches, None);
    assert!(!search::render(&p1).contains("Only the best"));
    assert!(
        search::render(&page_of(&index, "zebra", None, 10)).ends_with("\n")
            && page_of(&index, "zebra", None, 10).hits.len() == 4
    );
}

/// Doc 07 §4.2 "Kept matches": the count uses the archive condition of the query, so archive notes count only when
/// `ns` is `archive`. Digits are plain, with no thousands separator.
#[test]
fn the_count_follows_the_archive_condition() {
    let mut files = many("common", 55, "zebra");
    files.extend(many("archive", 1100, "zebra"));
    let v = vault_of(&files);
    let index = v.index();
    let plain = search::render(&page_of(&index, "zebra", None, 1));
    assert!(plain.contains("Only the best 50 of 55 matches are kept;"), "{plain}");
    let archive = search::render(&page_of(&index, "zebra", Some("archive"), 1));
    assert!(
        archive.contains("Only the best 50 of 1155 matches are kept;"),
        "{archive}"
    );
}

/// Doc 07 §4.2 "Paging": hit numbers continue across pages, so page 2 starts at `6.`
#[test]
fn numbering_continues_over_pages() {
    let v = vault_of(&many("common", 12, "zebra"));
    let index = v.index();
    let text = run(&index, "zebra", None, 2);
    assert!(text.starts_with("6. common/"), "{text}");
}

/// Doc 07 §4.2 "No hits", one word: `No notes match "<query>".`, no hint.
#[test]
fn no_hits_one_word() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra.
",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "kubernetes", None, 1),
        "No notes match \"kubernetes\".
"
    );
    // The query is quoted as given, trimmed.
    assert_eq!(
        run(&index, "  Kubernetes  ", None, 1),
        "No notes match \"Kubernetes\".
"
    );
}

/// Doc 07 §4.2 "No hits", several words: the line is printed only when not even one word matches, and it has no hint.
#[test]
fn no_hits_several_words() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra.
",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "giraffe kubernetes", None, 1),
        "No notes match \"giraffe kubernetes\".
"
    );
}

/// Doc 07 §4.2 "No hits": a quoted phrase is one part, so it gets no fill and no hint.
#[test]
fn no_hits_phrase() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra.
",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "\"zebra kubernetes\"", None, 1),
        "No notes match \"zebra kubernetes\".
"
    );
    assert_eq!(
        run(&index, "\"kubernetes\"", None, 1),
        "No notes match \"kubernetes\".
"
    );
    // A hyphenated word is one word.
    assert_eq!(
        run(&index, "cost-of-change", None, 1),
        "No notes match \"cost-of-change\".
"
    );
}

/// A normal result: through the CLI it is stdout with exit 0.
#[test]
fn no_hits_through_the_cli() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra.
",
    )]);
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["search", "kubernetes", "--vault"])
        .arg(&v.root)
        .env("RECOGNITOR_DB", &v.db)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "No notes match \"kubernetes\".
"
    );
}

/// Doc 07 §4.2 "Paging": a page past the end prints one line, as a normal result, not an error.
#[test]
fn a_page_past_the_end_prints_one_line() {
    let v = vault_of(&many("common", 13, "zebra"));
    let index = v.index();
    assert_eq!(
        run(&index, "zebra", None, 4),
        "Page 4 is past the end: 13 results on 3 pages.\n"
    );
    assert_eq!(
        run(&index, "zebra", None, 9),
        "Page 9 is past the end: 13 results on 3 pages.\n"
    );
    // The last page itself is not past the end, and has no "more" line.
    assert!(run(&index, "zebra", None, 3).starts_with("11. common/"));
    // Through the CLI: stdout, exit 0.
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["search", "zebra", "--page", "4", "--vault"])
        .arg(&v.root)
        .env("RECOGNITOR_DB", &v.db)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "Page 4 is past the end: 13 results on 3 pages.\n"
    );
}

/// Doc 07 §4.2 "Paging": one result and one page read in the singular.
#[test]
fn past_the_end_singular_forms() {
    let v = vault_of(&many("common", 1, "zebra"));
    let index = v.index();
    assert_eq!(
        run(&index, "zebra", None, 2),
        "Page 2 is past the end: 1 result on 1 page.\n"
    );
    // Five results fill exactly one page.
    let v = vault_of(&many("common", 5, "zebra"));
    let index = v.index();
    assert_eq!(
        run(&index, "zebra", None, 2),
        "Page 2 is past the end: 5 results on 1 page.\n"
    );
    // Six results make two pages.
    let v = vault_of(&many("common", 6, "zebra"));
    let index = v.index();
    assert_eq!(
        run(&index, "zebra", None, 3),
        "Page 3 is past the end: 6 results on 2 pages.\n"
    );
}

/// Doc 07 §4.2 "Paging" and "Kept matches": the notice follows the past-the-end line the way it follows a "more"
/// line, and the counts are the kept hits.
#[test]
fn past_the_end_with_the_kept_matches_notice() {
    let v = vault_of(&many("common", 60, "zebra"));
    let index = v.index();
    assert_eq!(
        run(&index, "zebra", None, 11),
        format!("Page 11 is past the end: 50 results on 10 pages. {NOTICE_60}\n")
    );
}

/// Doc 07 §4.2 "Paging": a query with no hits keeps the no-hits line on every page number.
#[test]
fn a_query_without_hits_keeps_its_line_on_page_two() {
    let v = TestVault::new(&[("common/Live.md", "# Live\n\nMarker zebra.\n")]);
    let index = v.index();
    assert_eq!(run(&index, "kubernetes", None, 2), "No notes match \"kubernetes\".\n");
    assert_eq!(run(&index, "kubernetes", None, 7), "No notes match \"kubernetes\".\n");
}

/// Doc 05 §2.4: the tokenizer is `porter unicode61`, so a stem and a different case match.
#[test]
fn matches_stems_and_ignores_case() {
    let v = TestVault::new(&[("common/Live.md", "# Live\n\nRunning shoes.\n")]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "RUN", None, 1)), ["common/Live"]);
}

/// Doc 05 §2.4: the title, the summary and the content are all searched.
#[test]
fn title_summary_and_content_are_searched() {
    let v = TestVault::new(&[
        ("common/Quokka.md", "# Quokka\n\nPlain.\n"),
        ("common/Two.md", "# Two\n\nAbout the numbat.\n"),
        ("common/Three.md", "# Three\n\nPlain.\n\nBody text mentions a wombat.\n"),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "quokka", None, 1)), ["common/Quokka"]);
    assert_eq!(ids(&page_of(&index, "numbat", None, 1)), ["common/Two"]);
    assert_eq!(ids(&page_of(&index, "wombat", None, 1)), ["common/Three"]);
}

/// Multi-word query: FTS5 reads two bare words as AND.
#[test]
fn two_words_both_must_match() {
    let v = TestVault::new(&[
        ("common/Both.md", "# Both\n\nFeedback and a loop.\n"),
        ("common/One.md", "# One\n\nFeedback only.\n"),
    ]);
    let index = v.index();
    let page = page_of(&index, "feedback loop", None, 1);
    // The note with both words comes first; the note with one word is the fill (doc 07 §4.2, "Some of the words").
    assert_eq!(ids(&page), ["common/Both", "common/One"]);
    assert_eq!(page.fill_start, Some(1));
}

/// Doc 07 §4 "Query text": `cost-of-change` is the phrase "cost of change".
#[test]
fn a_hyphenated_word_is_a_phrase() {
    let v = TestVault::new(&[
        (
            "common/Phrase.md",
            "# Phrase

The cost of change grows late.
",
        ),
        (
            "common/Apart.md",
            "# Apart

The change has a cost. Of course it does.
",
        ),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "cost-of-change", None, 1)), ["common/Phrase"]);
}

/// Doc 07 §4 "Query text": `c++` matches `c`.
#[test]
fn c_plus_plus_matches_c() {
    let v = TestVault::new(&[
        (
            "common/Lang.md",
            "# Lang

Written in C.
",
        ),
        (
            "common/Other.md",
            "# Other

None.
",
        ),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "c++", None, 1)), ["common/Lang"]);
}

/// Doc 07 §4 "Query text": text in double quotes stays one phrase.
#[test]
fn quoted_text_is_one_phrase() {
    let v = TestVault::new(&[
        (
            "common/InOrder.md",
            "# InOrder

A feedback loop here.
",
        ),
        (
            "common/Swapped.md",
            "# Swapped

A loop of feedback here.
",
        ),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "\"feedback loop\"", None, 1)), ["common/InOrder"]);
    assert_eq!(page_of(&index, "feedback loop", None, 1).hits.len(), 2);
}

/// Doc 07 §4 "Query text": an unmatched `"` is ignored, and no input is a syntax error.
#[test]
fn an_unmatched_quote_is_ignored() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra here.
",
    )]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "\"zebra", None, 1)), ["common/Live"]);
    assert_eq!(ids(&page_of(&index, "zebra\"", None, 1)), ["common/Live"]);
    // `a"b` is one word and the note has no "a b", so only the fill finds the note.
    let page = page_of(&index, "a\"b zebra", None, 1);
    assert_eq!(ids(&page), ["common/Live"]);
    assert_eq!(page.fill_start, Some(0));
}

/// Doc 07 §4 "Query text": no operators. `OR`, `NOT`, `*`, `:` and `^` are plain text, and `title:x` is no error.
#[test]
fn operators_are_plain_text() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra and giraffe here.
",
    )]);
    let index = v.index();
    // `OR` is a word, so no note holds every word; the note comes only as the fill, for "zebra".
    let page = page_of(&index, "zebra OR unicorn", None, 1);
    assert_eq!((ids(&page), page.fill_start), (vec!["common/Live"], Some(0)));
    // `NOT` is a word too: no note holds "not", and the note is again only the fill.
    let page = page_of(&index, "zebra NOT unicorn", None, 1);
    assert_eq!((ids(&page), page.fill_start), (vec!["common/Live"], Some(0)));
    // Words that match nothing, operators or not: no hits.
    assert!(page_of(&index, "unicorn OR narwhal", None, 1).hits.is_empty());
    assert!(page_of(&index, "unicorn NOT narwhal", None, 1).hits.is_empty());
    assert_eq!(ids(&page_of(&index, "zebr*", None, 1)), Vec::<&str>::new());
    assert!(page_of(&index, "title:x", None, 1).hits.is_empty());
    // The colon only splits the word into the phrase "title marker", which the note does not hold.
    assert!(page_of(&index, "title:marker", None, 1).hits.is_empty());
    assert!(search::search(index.connection(), r"^ ( ) { } - + ' \ NEAR(a b)", None, 1).is_ok());
}

/// Doc 07 §4 "Query text": the observation lines use the same expression as the hit query.
#[test]
fn observation_lines_use_the_same_expression() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs

About cost.

- [fact] The cost of change grows late
- [fact] Unrelated line
",
    )]);
    let index = v.index();
    assert_eq!(
        run(&index, "cost-of-change", None, 1),
        "1. common/Obs
   About cost.
   - [fact] The cost of change grows late
"
    );
}

/// Doc 07 §4 "Query text": an empty query is refused with an error that says so, not an FTS5 error.
#[test]
fn an_empty_query_is_refused() {
    let v = TestVault::new(&[(
        "common/Live.md",
        "# Live

Marker zebra.
",
    )]);
    let index = v.index();
    for q in ["", "   ", "\"", "\"\"", "++", "\" \""] {
        let err = search::search(index.connection(), q, None, 1).unwrap_err();
        assert!(matches!(err, recognitor::Error::EmptyQuery), "{q:?}: {err}");
        assert!(err.to_string().contains("empty"), "{err}");
    }
}

#[test]
fn match_expression_quotes_every_word() {
    let m = search::match_expression;
    assert_eq!(m("feedback loop").as_deref(), Some("\"feedback\" AND \"loop\""));
    assert_eq!(m("cost-of-change").as_deref(), Some("\"cost-of-change\""));
    assert_eq!(m("c++").as_deref(), Some("\"c++\""));
    assert_eq!(
        m("\"feedback loop\" gain").as_deref(),
        Some("\"feedback loop\" AND \"gain\"")
    );
    assert_eq!(m("\"zebra").as_deref(), Some("\"zebra\""));
    assert_eq!(m("a\"b").as_deref(), Some("\"a\"\"b\""));
    assert_eq!(m("zebra OR x").as_deref(), Some("\"zebra\" AND \"OR\" AND \"x\""));
    assert_eq!(m(""), None);
}

/// A note edited after the last sweep shows up once the 2-second gate has expired (doc 02 §4), not before.
#[test]
fn search_sweeps_only_when_the_gate_expired() {
    let v = TestVault::new(&[("common/Live.md", "# Live\n\nMarker zebra.\n")]);
    let mut index = v.index();
    v.write("common/Fresh.md", "# Fresh\n\nMarker zebra.\n");
    assert_eq!(ids(&index.search("zebra", None, 1).unwrap()), ["common/Live"]);

    db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    let mut got = index.search("zebra", None, 1).unwrap();
    got.hits.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(ids(&got), ["common/Fresh", "common/Live"]);
}

/// The CLI prints the same text as the library: `recognitor search <query> [--ns <namespace>] [--page <n>]`.
#[test]
fn cli_prints_the_page() {
    let v = vault_of(&many("common", 12, "zebra"));
    let run_cli = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(["search", "zebra", "--vault"])
            .arg(&v.root)
            .args(extra)
            .env("RECOGNITOR_DB", &v.db)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let p1 = run_cli(&[]);
    assert!(p1.starts_with("1. common/N"), "{p1}");
    assert!(p1.ends_with("7 more: page 2\n"), "{p1}");
    let p3 = run_cli(&["--page", "3", "--ns", "common"]);
    assert!(p3.starts_with("11. common/N"), "{p3}");
    assert!(!p3.contains("more: page"), "{p3}");
}

/// Doc 07 §4 "Page": `page` starts at 1. Page 0, a negative number, a non-number or a missing value is a usage error:
/// exit code 2, a message that names `--page`, nothing on stdout.
#[test]
fn cli_rejects_a_bad_page() {
    for bad in [
        vec!["--page", "0"],
        vec!["--page", "abc"],
        vec!["--page", "-1"],
        vec!["--page", "1.5"],
        vec!["--page"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(["search", "zebra", "--vault", "."])
            .args(&bad)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("error: --page needs a number from 1"),
            "{bad:?}: {stderr}"
        );
        assert!(out.stdout.is_empty(), "{bad:?}");
    }
}

/// A page built by hand: `lines` is the observation texts of each hit, every one a `fact`.
fn hand_page(lines: &[Vec<String>]) -> SearchPage {
    use recognitor::search::{Hit, ObservationLine};
    SearchPage {
        hits: lines
            .iter()
            .enumerate()
            .map(|(i, texts)| Hit {
                id: format!("common/Note {i}"),
                title: None,
                summary: Some(format!("Summary {i}.")),
                observations: texts
                    .iter()
                    .map(|t| ObservationLine {
                        category: "fact".to_string(),
                        qualifier: None,
                        content: t.clone(),
                    })
                    .collect(),
            })
            .collect(),
        page: 1,
        first_number: 1,
        more: 0,
        total: lines.len(),
        page_size: 10,
        matches: None,
        query: "zebra".to_string(),
        fill_start: None,
    }
}

fn tokens_of(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Doc 07 §4.2 "Budget": a fact line is cut at 200 characters, at a word boundary, and ends with `…`. The 200 counts
/// the whole line without its indent, `…` included.
#[test]
fn a_long_fact_line_is_cut_at_a_word_boundary() {
    assert_eq!(search::LINE_MAX_CHARS, 200);
    let words: Vec<String> = (0..80).map(|i| format!("word{i:02}")).collect();
    let text = words.join(" ");
    let out = search::render(&hand_page(&[vec![text.clone()]]));
    let line = out.lines().nth(2).unwrap().strip_prefix("   ").unwrap();
    assert!(line.ends_with('…'), "{line}");
    assert!(line.chars().count() <= 200, "{}", line.chars().count());
    let kept = line.strip_suffix('…').unwrap().strip_prefix("- [fact] ").unwrap();
    assert!(!kept.ends_with(' '));
    // Only whole words remain, and they are the first words of the text.
    assert!(text.starts_with(kept), "{kept}");
    assert!(text[kept.len()..].starts_with(' '), "{kept}");
    // The line is as long as the limit allows: one more word would not have fit.
    assert!(line.chars().count() + 7 > 200, "{}", line.chars().count());
}

/// A cut after a clause mark drops the mark: `evidence…`, not `evidence.…`.
#[test]
fn punctuation_before_the_ellipsis_is_dropped() {
    for mark in ['.', ',', ';', ':'] {
        // The cut falls right after the word that ends in the mark.
        let header = "- [fact] ";
        let filler = "x".repeat(200 - 1 - header.len() - 10);
        let text = format!("{filler} evidence{mark} and then some more words follow here");
        let out = search::render(&hand_page(&[vec![text]]));
        let line = out.lines().nth(2).unwrap().strip_prefix("   ").unwrap();
        assert!(line.ends_with("evidence…"), "{mark:?}: {line}");
    }
    // Other marks stay.
    let filler = "x".repeat(200 - 1 - 9 - 10);
    let out = search::render(&hand_page(&[vec![format!(
        "{filler} evidence! and then some more words follow here"
    )]]));
    assert!(out.lines().nth(2).unwrap().ends_with("evidence!…"), "{out}");
}

/// A line of exactly 200 characters, or less, is left alone.
#[test]
fn a_line_within_the_limit_is_not_cut() {
    let header = "- [fact] ";
    let text = "x".repeat(200 - header.len());
    let out = search::render(&hand_page(&[vec![text.clone()]]));
    assert_eq!(out.lines().nth(2).unwrap(), format!("   {header}{text}"));
    let out = search::render(&hand_page(&[vec![format!("{text}y")]]));
    assert!(out.lines().nth(2).unwrap().ends_with('…'));
}

/// Doc 07 §4.2 "Budget": a word that does not end within the limit is cut where the limit falls.
#[test]
fn a_word_longer_than_the_cut_is_cut_in_the_word() {
    let out = search::render(&hand_page(&[vec!["y".repeat(500)]]));
    let line = out.lines().nth(2).unwrap().strip_prefix("   ").unwrap();
    assert_eq!(line.chars().count(), 200);
    assert!(line.starts_with("- [fact] yyy") && line.ends_with("y…"), "{line}");
}

/// Doc 07 §4.2 "Budget": the qualifier counts toward the line and stays when the line is cut.
#[test]
fn a_cut_line_keeps_its_qualifier() {
    use recognitor::search::ObservationLine;
    let mut page = hand_page(&[vec![]]);
    page.hits[0].observations.push(ObservationLine {
        category: "decision".to_string(),
        qualifier: Some("@effective:2026-11-11".to_string()),
        content: "word ".repeat(100),
    });
    let out = search::render(&page);
    let line = out.lines().nth(2).unwrap();
    assert!(
        line.starts_with("   - [decision] @effective:2026-11-11 word word"),
        "{line}"
    );
    assert!(line.ends_with("word…"), "{line}");
}

/// Doc 07 §4.2 "Budget": a page stays under 1,000 tokens. Every hit keeps its id and summary and the closing line;
/// fact lines are added best hit first until one does not fit, and then no more are added.
#[test]
fn a_page_that_hits_the_budget_drops_the_lower_fact_lines() {
    assert_eq!(search::PAGE_TOKEN_BUDGET, 1000);
    let long = "word ".repeat(60);
    let mut page = hand_page(&vec![vec![long.clone(), long.clone(), long.clone()]; 10]);
    page.more = 3;
    let out = search::render(&page);
    assert!(tokens_of(&out) <= 1000, "{}", tokens_of(&out));
    // Every hit keeps its id and summary; the closing line stays.
    for i in 0..10 {
        assert!(
            out.contains(&format!("{}. common/Note {i}\n   Summary {i}.\n", i + 1)),
            "{out}"
        );
    }
    assert!(out.ends_with("3 more: page 2\n"), "{out}");
    // Fact lines per hit, in order: full for the first hits, then a partial hit, then none.
    let mut counts = vec![0usize; 10];
    let mut current = 0;
    for line in out.lines() {
        if let Some((n, _)) = line.split_once(". ").filter(|(n, _)| n.parse::<usize>().is_ok()) {
            current = n.parse::<usize>().unwrap() - 1;
        } else if line.starts_with("   - [fact]") {
            counts[current] += 1;
        }
    }
    assert_eq!(counts[0], 3, "{counts:?}");
    assert!(counts[9] == 0, "{counts:?}");
    let first_short = counts.iter().position(|c| *c < 3).unwrap();
    assert!(counts[first_short + 1..].iter().all(|c| *c == 0), "{counts:?}");
    // One more line would have gone over the budget.
    assert!(tokens_of(&out) + 50 > 1000, "{}", tokens_of(&out));
}

/// Doc 07 §4.2 "Budget": under the budget the page is unchanged: every fact line is shown, in full.
#[test]
fn a_page_under_the_budget_is_unchanged() {
    let page = hand_page(&vec![vec!["short one".to_string(), "short two".to_string()]; 3]);
    let out = search::render(&page);
    let expected = "1. common/Note 0\n   Summary 0.\n   - [fact] short one\n   - [fact] short two\n\
                    2. common/Note 1\n   Summary 1.\n   - [fact] short one\n   - [fact] short two\n\
                    3. common/Note 2\n   Summary 2.\n   - [fact] short one\n   - [fact] short two\n";
    assert_eq!(out, expected);
}

/// The batched observation query (one statement for the whole page) gives the same lines as the per-hit one it
/// replaced, in the same order: AND lines, the OR fallback per note, ties by line number, qualifiers, notes with
/// no observations, more than three matching lines.
#[test]
fn batched_observation_lines_equal_the_per_hit_lines() {
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..14 {
        let mut text = format!("# Note {i}\n\nAbout zebra and giraffe number {i}.\n\n");
        match i % 5 {
            // Several lines with both words, of different length and with a qualifier; ties on equal lines.
            0 => text.push_str(
                "- [fact] Zebra and giraffe together\n- [fact] Zebra and giraffe together\n\
                 - [rule] @effective:2026-11-11 Zebra, lion and giraffe all share one long line of other words\n\
                 - [fact] Giraffe zebra\n- [fact] Zebra alone here\n",
            ),
            // Only single-word lines: the OR fallback.
            1 => text.push_str("- [fact] Zebra only\n- [fact] Giraffe only here and there\n- [fact] Lion unrelated\n"),
            // Both kinds: the AND line wins and the OR lines stay out.
            2 => text.push_str("- [fact] Zebra first\n- [fact] Zebra giraffe both\n- [fact] Giraffe last\n"),
            // No observations at all.
            3 => text.push_str("Plain paragraph with zebra and giraffe.\n"),
            // Lines that match no word.
            _ => text.push_str("- [fact] Lion here\n- [fact] Nothing relevant\n"),
        }
        files.push((format!("common/Note {i}.md"), text));
    }
    let v = vault_of(&files);
    let index = v.index();
    for query in [
        "zebra",
        "giraffe",
        "zebra giraffe",
        "giraffe zebra lion",
        "\"zebra and giraffe\" together",
    ] {
        for ns in [None, Some("common")] {
            for page in 1..=3 {
                let got = page_of(&index, query, ns, page);
                for hit in &got.hits {
                    let want =
                        search::reference_observation_lines(index.connection(), query, &hit.id.to_lowercase()).unwrap();
                    assert_eq!(hit.observations, want, "{query:?} {ns:?} page {page}: {}", hit.id);
                }
            }
        }
    }
    // The cases above really cover AND lines, the fallback and a note without lines.
    let both: Vec<_> = (1..=3)
        .flat_map(|page| page_of(&index, "zebra giraffe", None, page).hits)
        .collect();
    let lines = |id: &str| both.iter().find(|h| h.id == id).map(|h| h.observations.len());
    assert_eq!(lines("common/Note 1"), Some(2));
    assert_eq!(lines("common/Note 2"), Some(1));
    assert_eq!(lines("common/Note 3"), Some(0));
    assert_eq!(lines("common/Note 0"), Some(3));
}

/// Guard for the speed of the batched observation query. It is fast only when SQLite builds a bloom filter from the pk
/// list and applies it BEFORE it looks up each matching observation row (`SEARCH o`). With the filter after the
/// lookup, a common word took 16 ms for one page instead of 3 ms.
///
/// A failure here means SQLite chose another plan (another version, another table definition or another statement
/// text): re-measure `alpha` and `alpha omega` on the 10k vault before changing anything, and fix the statement's
/// shape, not this test.
#[test]
fn the_batched_observation_query_keeps_its_fast_plan() {
    let v = TestVault::new(&[(
        "common/Obs.md",
        "# Obs\n\nAbout zebras.\n\n- [fact] Zebra one\n- [fact] Zebra two\n",
    )]);
    let index = v.index();
    let mut stmt = index
        .connection()
        .prepare(&format!("EXPLAIN QUERY PLAN {}", search::OBSERVATION_RANK_SQL))
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map(["\"zebra\"", "[1,2]"], |r| r.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let position = |needle: &str| plan.iter().position(|step| step.contains(needle));
    let (Some(lookup), Some(bloom)) = (position("SEARCH o USING INTEGER PRIMARY KEY"), position("BLOOM FILTER")) else {
        panic!(
            "the plan changed: expected a bloom filter and a rowid lookup of `o`; re-measure the 10k numbers: {plan:?}"
        );
    };
    assert!(
        bloom < lookup,
        "the plan changed: the pk-list bloom filter must come before the lookup of each observation row; re-measure \
         the 10k numbers: {plan:?}"
    );
    // The full-text scan drives the join, filtered by the match.
    assert!(
        plan.iter()
            .any(|step| step.contains("SCAN observations_fts VIRTUAL TABLE")),
        "the plan changed: re-measure the 10k numbers: {plan:?}"
    );
}

// Some of the words (doc 07 §4.2, doc 05 §3.7).

const SEPARATOR: &str = "Notes with only some of the words:";

/// `count` notes `{ns}/{prefix}{i}` that hold both `a` and `b`.
fn both(ns: &str, prefix: &str, count: usize, a: &str, b: &str) -> Vec<(String, String)> {
    (0..count)
        .map(|i| {
            (
                format!("{ns}/{prefix}{i:02}.md"),
                format!("# {prefix}{i:02}\n\nSummary {i}.\n\nText with {a} and {b} together.\n"),
            )
        })
        .collect()
}

/// `count` notes `{ns}/{prefix}{i}` that hold only `term`.
fn only(ns: &str, prefix: &str, count: usize, term: &str) -> Vec<(String, String)> {
    (0..count)
        .map(|i| {
            (
                format!("{ns}/{prefix}{i:02}.md"),
                format!("# {prefix}{i:02}\n\nSummary {i}.\n\nText with {term} alone.\n"),
            )
        })
        .collect()
}

/// Fewer than 5 notes with every word: they come first, then the separator, then the notes with only some words,
/// numbered on from the last hit, until page 1 holds 5 hits.
#[test]
fn fewer_than_five_all_word_hits_get_the_fill() {
    let mut files = both("common", "All", 2, "zebra", "giraffe");
    files.extend(only("common", "Some", 12, "zebra"));
    let v = vault_of(&files);
    let index = v.index();
    let page = page_of(&index, "zebra giraffe", None, 1);
    assert_eq!(page.hits.len(), 5);
    assert_eq!(page.fill_start, Some(2));
    let text = search::render(&page);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("1. common/All"), "{text}");
    assert!(
        lines[2].starts_with("   - ") || lines[2].starts_with("2. common/All"),
        "{text}"
    );
    let at = lines.iter().position(|l| *l == SEPARATOR).expect(&text);
    assert!(lines[at + 1].starts_with("3. common/Some"), "{text}");
    assert!(text.contains("\n5. common/Some"), "{text}");
    assert!(!text.contains("\n6. "), "{text}");
    // The fill is the last thing: no "more" line, no page 2.
    assert!(!text.contains("more: page"), "{text}");
    assert_eq!(text.matches(SEPARATOR).count(), 1);
    // The all-word hits stand above the separator, the fill below it.
    assert!(text.find("common/All00").unwrap() < text.find(SEPARATOR).unwrap());
    assert!(text.find("common/All01").unwrap() < text.find(SEPARATOR).unwrap());
    assert!(text.find(SEPARATOR).unwrap() < text.find("common/Some").unwrap());
}

/// The doc 05 §3.7 case: the fill is a block of its own with exact text.
#[test]
fn the_fill_block_line_for_line() {
    let v = TestVault::new(&[
        ("common/Both.md", "# Both\n\nHas both.\n\nZebra and giraffe.\n"),
        (
            "common/Half.md",
            "# Half\n\nHas one.\n\n- [fact] Only the zebra is here\n",
        ),
    ]);
    let index = v.index();
    assert_eq!(
        run(&index, "zebra giraffe", None, 1),
        "1. common/Both\n\
         \x20  Has both.\n\
         Notes with only some of the words:\n\
         2. common/Half\n\
         \x20  Has one.\n\
         \x20  - [fact] Only the zebra is here\n"
    );
}

/// No note holds every word: the separator is the page's first line.
#[test]
fn without_all_word_hits_the_separator_is_the_first_line() {
    let v = vault_of(&only("common", "Some", 3, "zebra"));
    let index = v.index();
    let page = page_of(&index, "zebra giraffe", None, 1);
    assert_eq!(page.fill_start, Some(0));
    let text = search::render(&page);
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(SEPARATOR), "{text}");
    assert!(lines.next().unwrap().starts_with("1. common/Some"), "{text}");
    assert_eq!(page.hits.len(), 3);
}

/// Five or more notes with every word fill page 1 on their own: no fill, no separator, even though more notes hold
/// some of the words. All-words hits are never cut; the rest pages as usual.
#[test]
fn five_or_more_all_word_hits_get_no_fill() {
    for count in [5, 8, 12] {
        let mut files = both("common", "All", count, "zebra", "giraffe");
        files.extend(only("common", "Some", 5, "zebra"));
        let v = vault_of(&files);
        let index = v.index();
        let page = page_of(&index, "zebra giraffe", None, 1);
        assert_eq!(page.hits.len(), 5);
        assert_eq!(page.more, count - 5);
        assert_eq!(page.fill_start, None);
        assert!(page.hits.iter().all(|h| h.id.starts_with("common/All")));
        let text = search::render(&page);
        assert!(!text.contains(SEPARATOR), "{text}");
        // Every page of the query is all-words hits.
        let last = count.div_ceil(5);
        for n in 2..=last {
            let text = run(&index, "zebra giraffe", None, n);
            assert!(!text.contains(SEPARATOR), "{text}");
            assert!(text.starts_with(&format!("{}. common/All", (n - 1) * 5 + 1)), "{text}");
        }
    }
    // Four is the most that gets a fill: one note is added.
    let mut files = both("common", "All", 4, "zebra", "giraffe");
    files.extend(only("common", "Some", 5, "zebra"));
    let v = vault_of(&files);
    let index = v.index();
    let page = page_of(&index, "zebra giraffe", None, 1);
    assert_eq!((page.hits.len(), page.fill_start, page.more), (5, Some(4), 0));
}

/// A one-word query gets no fill: the OR query is the same query. A quoted phrase is one part and gets none either.
#[test]
fn a_one_word_query_gets_no_fill() {
    let v = TestVault::new(&[
        ("common/Zebra.md", "# Zebra\n\nStripes.\n"),
        ("common/Other.md", "# Other\n\nZebra crossing by the giraffe.\n"),
        ("common/Third.md", "# Third\n\nA zebra and a crossing.\n"),
    ]);
    let index = v.index();
    for query in ["zebra", "\"zebra crossing\"", "cost-of-zebra"] {
        let page = page_of(&index, query, None, 1);
        assert_eq!(page.fill_start, None, "{query}");
        assert!(!search::render(&page).contains(SEPARATOR), "{query}");
    }
    assert_eq!(page_of(&index, "zebra", None, 1).hits.len(), 3);
    assert_eq!(page_of(&index, "\"zebra crossing\"", None, 1).hits.len(), 1);
}

/// Not even one word matches: the no-hits line alone, with no hint, on every page number.
#[test]
fn no_word_matches_at_all() {
    let v = TestVault::new(&[("common/Live.md", "# Live\n\nMarker zebra.\n")]);
    let index = v.index();
    for page in [1, 2, 5] {
        assert_eq!(
            run(&index, "unicorn narwhal", None, page),
            "No notes match \"unicorn narwhal\".\n"
        );
    }
}

/// The fill never adds a "more" line or a page 2. A page past the end counts every result, fill included, as one page.
#[test]
fn the_fill_adds_no_page_two() {
    let mut files = both("common", "All", 3, "zebra", "giraffe");
    files.extend(only("common", "Some", 30, "zebra"));
    let v = vault_of(&files);
    let index = v.index();
    let text = run(&index, "zebra giraffe", None, 1);
    assert!(!text.contains("more"), "{text}");
    assert_eq!(text.lines().filter(|l| l.contains(". common/")).count(), 5);
    assert_eq!(
        run(&index, "zebra giraffe", None, 2),
        "Page 2 is past the end: 5 results on 1 page.\n"
    );
    assert_eq!(
        run(&index, "zebra giraffe", None, 4),
        "Page 4 is past the end: 5 results on 1 page.\n"
    );
    // A fill that does not fill the page: the results are the hits and the fill.
    let v = TestVault::new(&[
        ("common/Both.md", "# Both\n\nZebra giraffe.\n"),
        ("common/Half.md", "# Half\n\nZebra.\n"),
    ]);
    let index = v.index();
    assert_eq!(
        run(&index, "zebra giraffe", None, 2),
        "Page 2 is past the end: 2 results on 1 page.\n"
    );
    // A query with only a fill still has its results on page 2: it is past the end, not "no notes match".
    let v = vault_of(&only("common", "Some", 3, "zebra"));
    let index = v.index();
    assert_eq!(
        run(&index, "zebra giraffe", None, 2),
        "Page 2 is past the end: 3 results on 1 page.\n"
    );
}

/// The `ns` boost orders the fill among themselves, as it does the hits.
#[test]
fn the_ns_boost_orders_the_fill() {
    // Same text, so the same BM25 score; the note in `focus` must win with the boost, and lose the tie without it.
    let v = TestVault::new(&[
        ("common/Both.md", "# Both\n\nZebra giraffe.\n"),
        ("alpha/Plain.md", "# Plain\n\nSummary.\n\nOnly zebra here.\n"),
        ("focus/Boosted.md", "# Boosted\n\nSummary.\n\nOnly zebra here.\n"),
    ]);
    let index = v.index();
    let plain = page_of(&index, "zebra giraffe", None, 1);
    assert_eq!(ids(&plain), ["common/Both", "alpha/Plain", "focus/Boosted"]);
    let boosted = page_of(&index, "zebra giraffe", Some("focus"), 1);
    assert_eq!(ids(&boosted), ["common/Both", "focus/Boosted", "alpha/Plain"]);
    assert_eq!(boosted.fill_start, Some(1));
}

/// The boost never lifts a fill hit above an all-word hit.
#[test]
fn the_fill_stays_below_the_all_word_hits_with_a_boost() {
    let v = TestVault::new(&[
        ("alpha/Both.md", "# Both\n\nZebra giraffe.\n"),
        ("focus/Half.md", "# Half\n\nZebra zebra zebra.\n"),
    ]);
    let index = v.index();
    let page = page_of(&index, "zebra giraffe", Some("focus"), 1);
    assert_eq!(ids(&page), ["alpha/Both", "focus/Half"]);
    assert_eq!(page.fill_start, Some(1));
}

/// Archive notes are in the fill only when `ns` is `archive`, as for the hits (doc 07 §1.1).
#[test]
fn the_fill_follows_the_store_rules() {
    let v = TestVault::new(&[
        ("common/Both.md", "# Both\n\nZebra giraffe.\n"),
        ("archive/Old.md", "# Old\n\nZebra alone.\n"),
    ]);
    let index = v.index();
    assert_eq!(ids(&page_of(&index, "zebra giraffe", None, 1)), ["common/Both"]);
    assert_eq!(
        ids(&page_of(&index, "zebra giraffe", Some("archive"), 1)),
        ["common/Both", "archive/Old"]
    );
}

/// A filled hit shows the lines that hold any word (doc 07 §4.2, "Some of the words": none holds every word).
#[test]
fn filled_hits_show_any_word_fact_lines() {
    let v = TestVault::new(&[
        (
            "common/Both.md",
            "# Both\n\nBoth.\n\n- [fact] Zebra and giraffe\n- [fact] Zebra alone\n",
        ),
        (
            "common/Half.md",
            "# Half\n\nHalf.\n\n- [fact] Zebra at dawn\n- [fact] Zebra at dusk\n- [fact] Unrelated\n",
        ),
    ]);
    let index = v.index();
    assert_eq!(
        run(&index, "zebra giraffe", None, 1),
        "1. common/Both\n\
         \x20  Both.\n\
         \x20  - [fact] Zebra and giraffe\n\
         Notes with only some of the words:\n\
         2. common/Half\n\
         \x20  Half.\n\
         \x20  - [fact] Zebra at dawn\n\
         \x20  - [fact] Zebra at dusk\n"
    );
}

/// The words of the doc's benchmark case: a note that writes "VO2max" is found by the words around it.
#[test]
fn some_of_the_words_finds_a_note_the_other_words_miss() {
    let v = TestVault::new(&[(
        "common/Fitness.md",
        "# Fitness\n\nHow fit.\n\n- [fact] Tested VO2max in the fitness lab\n",
    )]);
    let index = v.index();
    let text = run(&index, "VO2 max fitness test", None, 1);
    assert!(text.starts_with(&format!("{SEPARATOR}\n1. common/Fitness\n")), "{text}");
}

/// The 1,000-token page budget holds with the fill and the separator counted. The summaries are long so that five
/// hits and their fact lines cannot all fit.
#[test]
fn the_budget_holds_with_the_fill() {
    let long = "word ".repeat(40);
    let summary = "Summary ".repeat(40);
    let mut files = Vec::new();
    for i in 0..2 {
        files.push((
            format!("common/All{i}.md"),
            format!(
                "# All{i}\n\n{summary}{i}.\n\n- [fact] zebra giraffe {long}\n- [fact] zebra giraffe {long}\n- [fact] zebra giraffe {long}\n"
            ),
        ));
    }
    for i in 0..12 {
        files.push((
            format!("common/Some{i:02}.md"),
            format!(
                "# Some{i:02}\n\n{summary}{i}.\n\n- [fact] zebra {long}\n- [fact] zebra {long}\n- [fact] zebra {long}\n"
            ),
        ));
    }
    let v = vault_of(&files);
    let index = v.index();
    let page = page_of(&index, "zebra giraffe", None, 1);
    assert_eq!(page.hits.len(), 5);
    let text = search::render(&page);
    assert!(
        tokens_of(&text) <= search::PAGE_TOKEN_BUDGET,
        "{} tokens",
        tokens_of(&text)
    );
    // Every hit keeps its id, and the separator stays, however many fact lines were dropped.
    assert_eq!(text.lines().filter(|l| l.contains(". common/")).count(), 5);
    assert!(text.contains(SEPARATOR));
    let facts = text.lines().filter(|l| l.starts_with("   - [fact]")).count();
    assert!(facts < 15, "{facts} fact lines should not all fit");
}

/// The fill reaches the CLI as the same text.
#[test]
fn the_fill_through_the_cli() {
    let v = TestVault::new(&[
        ("common/Both.md", "# Both\n\nHas both.\n\nZebra and giraffe.\n"),
        ("common/Half.md", "# Half\n\nHas one.\n\nZebra only.\n"),
    ]);
    let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(["search", "zebra giraffe", "--vault"])
        .arg(&v.root)
        .env("RECOGNITOR_DB", &v.db)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("\nNotes with only some of the words:\n2. common/Half\n"),
        "{text}"
    );
}

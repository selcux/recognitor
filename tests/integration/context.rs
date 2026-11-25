//! `context` tests (doc 07 §8, §6.3; doc 05 §3.3-3.4). Vaults are synthetic and live in temp folders that delete
//! themselves.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use recognitor::context::{self, Context, Limits, PAGE_SIZE, PAGE_TOKEN_BUDGET};
use recognitor::{Error, Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-context-{}-{n}", std::process::id()));
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

    /// Sets a file's modification time to `secs` after a fixed base, so "most recently changed" is deterministic.
    fn touch(&self, rel: &str, secs: u64) {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs);
        fs::File::options()
            .write(true)
            .open(self.root.join(rel))
            .unwrap()
            .set_modified(t)
            .unwrap();
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

fn run(index: &Index, note: &str) -> String {
    run_page(index, note, 1)
}

fn run_page(index: &Index, note: &str, page: usize) -> String {
    context::render(&context::context(index.connection(), note, page).unwrap())
}

/// The vault behind the doc 07 §8 example.
fn example_vault() -> TestVault {
    let v = TestVault::new(&[
        (
            "recognitor/Decisions.md",
            "# Decisions\n\nDurable decisions for the engine, newest first.\n\n## Relations\n\
             - part_of [[index]]\n\
             - relates_to [[compass::Feedback loops]]\n\
             - relates_to [[Gall's law]]\n\
             - relates_to [[archive::recognitor/Session 2026-11-14]]\n",
        ),
        (
            "recognitor/index.md",
            "# Index\n\nActive memory project for the Recognitor codebase.\n\n## Relations\n- has_part [[Decisions]]\n",
        ),
        (
            "compass/concepts/Feedback loops.md",
            "# Feedback loops\n\nWhat you build reacts to its own results.\n",
        ),
        (
            "archive/recognitor/Session 2026-11-14.md",
            "# Session 2026-11-14\n\nFull record of the write-lock design session.\n\nSee [[Decisions]].\n",
        ),
        (
            "recognitor/Sync design.md",
            "# Sync design\n\nHow the index stays in step with the files.\n\nThe rules live in [[Decisions]].\n",
        ),
    ]);
    v.touch("recognitor/index.md", 10);
    v.touch("recognitor/Sync design.md", 20);
    v
}

const EXAMPLE: &str = "\
recognitor/Decisions

Links out:
- part_of: recognitor/index
  Active memory project for the Recognitor codebase.
- relates_to: compass/concepts/Feedback loops
  What you build reacts to its own results.
- relates_to: Gall's law  (not written yet)
- relates_to: archive/recognitor/Session 2026-11-14  (archive)
  Full record of the write-lock design session.

Links in:
- links_to, from recognitor/Sync design
  How the index stays in step with the files.
";

#[test]
fn matches_the_doc_example_line_for_line() {
    let v = example_vault();
    assert_eq!(run(&v.index(), "recognitor/Decisions"), EXAMPLE);
}

#[test]
fn an_inverse_pair_is_shown_once_from_either_end() {
    let v = example_vault();
    let text = run(&v.index(), "recognitor/index");
    // `index has_part Decisions` is written in index, so it stays under links out; `Decisions part_of index` is the
    // same fact and does not come back under links in.
    assert_eq!(
        text,
        "recognitor/index\n\nLinks out:\n- has_part: recognitor/Decisions\n  \
         Durable decisions for the engine, newest first.\n"
    );
}

#[test]
fn a_one_sided_hierarchy_link_is_shown_in_both_notes() {
    let v = TestVault::new(&[
        ("a/Parent.md", "# Parent\n\nThe whole.\n"),
        (
            "a/Child.md",
            "# Child\n\nA piece.\n\n## Relations\n- part_of [[Parent]]\n",
        ),
    ]);
    let index = v.index();
    assert_eq!(
        run(&index, "a/Parent"),
        "a/Parent\n\nLinks in:\n- part_of, from a/Child\n  A piece.\n"
    );
    assert_eq!(
        run(&index, "a/Child"),
        "a/Child\n\nLinks out:\n- part_of: a/Parent\n  The whole.\n"
    );
}

#[test]
fn only_has_part_and_part_of_pair_up() {
    // `indexes` and `relates_to` have no inverse in doc 03 §3, so both ends show both links.
    let v = TestVault::new(&[
        (
            "a/index.md",
            "# Index\n\nEntry.\n\n## Relations\n- indexes [[Topic]]\n- relates_to [[Topic]]\n",
        ),
        (
            "a/Topic.md",
            "# Topic\n\nA topic.\n\n## Relations\n- part_of [[index]]\n- relates_to [[index]]\n",
        ),
    ]);
    let text = run(&v.index(), "a/index");
    assert!(text.contains("- indexes: a/Topic\n"), "{text}");
    assert!(text.contains("- relates_to: a/Topic\n"), "{text}");
    assert!(text.contains("- part_of, from a/Topic\n"), "{text}");
    assert!(text.contains("- relates_to, from a/Topic\n"), "{text}");
}

#[test]
fn bootstrap_notes_are_never_listed() {
    let v = TestVault::new(&[
        (
            "a/Note.md",
            "# Note\n\nA note.\n\n## Relations\n- relates_to [[bootstrap::Rules]]\n- relates_to [[Other]]\n",
        ),
        ("a/Other.md", "# Other\n\nAnother note.\n"),
        (
            "bootstrap/index.md",
            "# Bootstrap\n\nEntry.\n\n## Relations\n- indexes [[Rules]]\n\nSee [[a::Note]].\n",
        ),
        ("bootstrap/Rules.md", "# Rules\n\nGlobal rules.\n"),
    ]);
    let text = run(&v.index(), "a/Note");
    assert_eq!(text, "a/Note\n\nLinks out:\n- relates_to: a/Other\n  Another note.\n");
}

#[test]
fn a_bootstrap_note_can_be_asked_about_but_its_bootstrap_neighbors_stay_hidden() {
    let v = TestVault::new(&[
        ("a/Note.md", "# Note\n\nA note.\n"),
        (
            "bootstrap/index.md",
            "# Bootstrap\n\nEntry.\n\n## Relations\n- indexes [[Rules]]\n",
        ),
        (
            "bootstrap/Rules.md",
            "# Rules\n\nGlobal rules.\n\n## Relations\n- relates_to [[a::Note]]\n- relates_to [[index]]\n",
        ),
    ]);
    assert_eq!(
        run(&v.index(), "bootstrap/Rules"),
        "bootstrap/Rules\n\nLinks out:\n- relates_to: a/Note\n  A note.\n"
    );
}

#[test]
fn links_in_come_most_recently_changed_first() {
    let v = TestVault::new(&[
        ("a/Target.md", "# Target\n\nThe target.\n"),
        ("a/Old.md", "# Old\n\nOld note.\n\nSee [[Target]].\n"),
        ("a/New.md", "# New\n\nNew note.\n\nSee [[Target]].\n"),
        ("a/Mid.md", "# Mid\n\nMid note.\n\nSee [[Target]].\n"),
    ]);
    v.touch("a/Old.md", 1);
    v.touch("a/Mid.md", 2);
    v.touch("a/New.md", 3);
    let text = run(&v.index(), "a/Target");
    assert_eq!(
        text,
        "a/Target\n\nLinks in:\n\
         - links_to, from a/New\n  New note.\n\
         - links_to, from a/Mid\n  Mid note.\n\
         - links_to, from a/Old\n  Old note.\n"
    );
}

#[test]
fn links_out_keep_written_order() {
    let v = TestVault::new(&[
        (
            "a/Note.md",
            "# Note\n\nA note that mentions [[Zeta]] first.\n\nThen [[Alpha]].\n\n## Relations\n- relates_to [[Mid]]\n",
        ),
        ("a/Zeta.md", "# Zeta\n\nZ.\n"),
        ("a/Alpha.md", "# Alpha\n\nA.\n"),
        ("a/Mid.md", "# Mid\n\nM.\n"),
    ]);
    let text = run(&v.index(), "a/Note");
    assert_eq!(
        text,
        "a/Note\n\nLinks out:\n- links_to: a/Zeta\n  Z.\n- links_to: a/Alpha\n  A.\n- relates_to: a/Mid\n  M.\n"
    );
}

#[test]
fn ids_get_their_title_when_it_differs_from_the_filename() {
    let v = TestVault::new(&[
        (
            "baseline/Tools and sourcing.md",
            "---\ntitle: Tools & sourcing\n---\n# Tools & sourcing\n\nWhere things come from.\n\n## Relations\n\
             - relates_to [[Kit]]\n",
        ),
        ("baseline/Kit.md", "# Kit list\n\nWhat to pack.\n"),
    ]);
    let text = run(&v.index(), "baseline/Tools and sourcing");
    assert_eq!(
        text,
        "baseline/Tools and sourcing, titled \"Tools & sourcing\"\n\nLinks out:\n\
         - relates_to: baseline/Kit, titled \"Kit list\"\n  What to pack.\n"
    );
}

#[test]
fn a_ghost_shows_the_link_as_written() {
    let v = TestVault::new(&[(
        "backend/Release.md",
        "# Release\n\nShips it.\n\nFollow [the checklist](../ops/Deploy%20Checklist.md).\n",
    )]);
    let text = run(&v.index(), "backend/Release");
    assert_eq!(
        text,
        "backend/Release\n\nLinks out:\n- links_to: ../ops/Deploy%20Checklist.md  (not written yet)\n"
    );
}

#[test]
fn a_note_without_a_summary_gets_no_summary_line() {
    let v = TestVault::new(&[
        ("a/Note.md", "# Note\n\nA note.\n\nSee [[Bare]].\n"),
        ("a/Bare.md", "# Bare\n\n- just a list\n"),
    ]);
    assert_eq!(run(&v.index(), "a/Note"), "a/Note\n\nLinks out:\n- links_to: a/Bare\n");
}

#[test]
fn two_spellings_of_one_link_are_one_line() {
    let v = TestVault::new(&[
        (
            "a/Note.md",
            "# Note\n\nA note.\n\nSee [[Target]], and again [[a::Target]].\n",
        ),
        ("a/Target.md", "# Target\n\nThe target.\n"),
    ]);
    let index = v.index();
    assert_eq!(
        run(&index, "a/Note"),
        "a/Note\n\nLinks out:\n- links_to: a/Target\n  The target.\n"
    );
    assert_eq!(
        run(&index, "a/Target"),
        "a/Target\n\nLinks in:\n- links_to, from a/Note\n  A note.\n"
    );
}

#[test]
fn a_self_link_is_shown_once() {
    let v = TestVault::new(&[("a/Loop.md", "# Loop\n\nPoints at itself.\n\nSee [[Loop]].\n")]);
    assert_eq!(
        run(&v.index(), "a/Loop"),
        "a/Loop\n\nLinks out:\n- links_to: a/Loop\n  Points at itself.\n"
    );
}

#[test]
fn an_archive_note_has_only_links_in() {
    let v = example_vault();
    assert_eq!(
        run(&v.index(), "archive/recognitor/Session 2026-11-14"),
        "archive/recognitor/Session 2026-11-14\n\nLinks in:\n- relates_to, from recognitor/Decisions\n  \
         Durable decisions for the engine, newest first.\n"
    );
}

#[test]
fn a_note_without_links_is_one_line_on_every_page() {
    let v = TestVault::new(&[("a/Alone.md", "# Alone\n\nNothing around it.\n")]);
    let index = v.index();
    for page in [1, 2, 7] {
        assert_eq!(run_page(&index, "a/Alone", page), "No links to or from a/Alone.\n");
    }
}

#[test]
fn a_titled_note_without_links_shows_its_title() {
    let v = TestVault::new(&[("baseline/index.md", "# Baseline\n\nHealth and lifestyle.\n")]);
    assert_eq!(
        run(&v.index(), "baseline/index"),
        "No links to or from baseline/index, titled \"Baseline\".\n"
    );
}

#[test]
fn links_only_to_and_from_bootstrap_count_as_no_links() {
    let v = TestVault::new(&[
        (
            "a/Note.md",
            "# Note\n\nA note.\n\n## Relations\n- relates_to [[bootstrap::Rules]]\n",
        ),
        (
            "bootstrap/index.md",
            "# Bootstrap\n\nEntry.\n\n## Relations\n- indexes [[Rules]]\n\nSee [[a::Note]].\n",
        ),
        ("bootstrap/Rules.md", "# Rules\n\nGlobal rules.\n"),
    ]);
    assert_eq!(run(&v.index(), "a/Note"), "No links to or from a/Note.\n");
}

#[test]
fn cli_prints_no_links_as_a_normal_result() {
    let v = TestVault::new(&[("a/Alone.md", "# Alone\n\nNothing around it.\n")]);
    let out = cli(&v, &["context", "a/Alone"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "No links to or from a/Alone.\n");
}

// Doc 07 §6.3: an id, a path or a link name, matched by key, with steps 1, 2 and 4.

#[test]
fn the_note_can_be_named_by_id_path_prefixed_name_or_unique_name() {
    let v = example_vault();
    let index = v.index();
    for name in [
        "recognitor/Decisions",
        "recognitor/Decisions.md",
        "RECOGNITOR/decisions",
        "recognitor::Decisions",
        "Decisions",
        "decisions",
    ] {
        assert_eq!(run(&index, name), EXAMPLE, "{name}");
    }
}

#[test]
fn several_matches_return_their_ids_sorted() {
    let v = TestVault::new(&[
        ("common/index.md", "# Index\n\nCommon.\n"),
        ("compass/Index.md", "# Index\n\nCompass.\n"),
        ("baseline/index.md", "# Index\n\nBaseline.\n"),
    ]);
    let index = v.index();
    let found = context::context(index.connection(), "index", 1).unwrap();
    assert_eq!(
        found,
        Context::Several {
            name: "index".to_string(),
            ids: vec![
                "baseline/index".to_string(),
                "common/index".to_string(),
                "compass/Index".to_string()
            ],
        }
    );
    assert_eq!(
        context::render(&found),
        "\"index\" matches 3 notes. Call again with one id:\n  baseline/index\n  common/index\n  compass/Index\n"
    );
    // The prefixed name and the id each match one note.
    assert_eq!(run(&index, "compass::index"), "No links to or from compass/Index.\n");
    assert_eq!(run(&index, "common/index"), "No links to or from common/index.\n");
}

#[test]
fn step_3_is_skipped_so_a_name_in_two_namespaces_is_ambiguous() {
    let v = TestVault::new(&[
        ("research/Caching.md", "# Caching\n\nResearch.\n"),
        ("personal/Caching.md", "# Caching\n\nPersonal.\n"),
    ]);
    let text = run(&v.index(), "Caching");
    assert_eq!(
        text,
        "\"Caching\" matches 2 notes. Call again with one id:\n  personal/Caching\n  research/Caching\n"
    );
}

#[test]
fn an_unknown_note_is_not_found() {
    let v = example_vault();
    let index = v.index();
    for name in [
        "Nothing here",
        "recognitor/Missing",
        "./Decisions",
        "../recognitor/Decisions",
        "zz::Decisions",
    ] {
        let err = context::context(index.connection(), name, 1).unwrap_err();
        assert!(
            matches!(err, Error::NoteNotFound(ref n) if n == name),
            "{name}: {err:?}"
        );
        assert_eq!(err.to_string(), format!("\"{name}\" not found"));
    }
}

// Paging (doc 07 §8, "Limit"): 20 links per page, the last line says how many more exist.

fn wide_vault(out_links: usize, in_links: usize) -> TestVault {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut hub = String::from("# Hub\n\nThe hub.\n\n");
    for i in 0..out_links {
        hub.push_str(&format!("Line [[O{i:02}]].\n\n"));
        files.push((format!("a/O{i:02}.md"), format!("# O{i:02}\n\nOut {i}.\n")));
    }
    for i in 0..in_links {
        files.push((
            format!("a/I{i:02}.md"),
            format!("# I{i:02}\n\nIn {i}.\n\nSee [[Hub]].\n"),
        ));
    }
    files.push(("a/Hub.md".to_string(), hub));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let v = TestVault::new(&refs);
    for i in 0..in_links {
        v.touch(&format!("a/I{i:02}.md"), 100 - i as u64);
    }
    v
}

#[test]
fn twenty_links_per_page_over_both_lists() {
    let v = wide_vault(15, 10);
    let index = v.index();
    let page1 = run_page(&index, "a/Hub", 1);
    assert_eq!(page1.matches("\n- ").count(), PAGE_SIZE);
    assert!(page1.contains("Links out:\n- links_to: a/O00\n"), "{page1}");
    assert!(
        page1.contains("- links_to: a/O14\n  Out 14.\n\nLinks in:\n- links_to, from a/I00\n"),
        "{page1}"
    );
    assert!(
        page1.contains("- links_to, from a/I04\n  In 4.\n5 more: page 2\n"),
        "{page1}"
    );
    assert!(page1.ends_with("5 more: page 2\n"), "{page1}");

    let page2 = run_page(&index, "a/Hub", 2);
    assert_eq!(
        page2,
        "a/Hub\n\nLinks in:\n\
         - links_to, from a/I05\n  In 5.\n\
         - links_to, from a/I06\n  In 6.\n\
         - links_to, from a/I07\n  In 7.\n\
         - links_to, from a/I08\n  In 8.\n\
         - links_to, from a/I09\n  In 9.\n"
    );
    assert_eq!(
        run_page(&index, "a/Hub", 3),
        "Page 3 is past the end: 25 links on 2 pages.\n"
    );
}

#[test]
fn exactly_one_full_page_has_no_more_line() {
    let v = wide_vault(20, 0);
    let text = run_page(&v.index(), "a/Hub", 1);
    assert_eq!(text.matches("\n- ").count(), 20);
    assert!(text.ends_with("- links_to: a/O19\n  Out 19.\n"), "{text}");
}

#[test]
fn page_size_splits_inside_links_out() {
    let v = wide_vault(3, 2);
    let index = v.index();
    let limits = Limits {
        links: 2,
        tokens: PAGE_TOKEN_BUDGET,
    };
    let page = |p| context::render(&context::context_with_limits(index.connection(), "a/Hub", p, limits).unwrap());
    assert_eq!(
        page(2),
        "a/Hub\n\nLinks out:\n- links_to: a/O02\n  Out 2.\n\nLinks in:\n- links_to, from a/I00\n  In 0.\n1 more: page 3\n"
    );
    assert_eq!(page(3), "a/Hub\n\nLinks in:\n- links_to, from a/I01\n  In 1.\n");
}

// The token budget (doc 07 §8, "Limit"): a page stays under 1,000 tokens, and a link is never cut.

/// A summary of 278 characters: one sentence, under the 300-character cut of doc 03 §2.
fn long_summary(i: usize) -> String {
    format!("Note {i:02} {}end.", "filler ".repeat(38))
}

/// A hub with `count` links out, each to a note with a long summary.
fn long_vault(count: usize) -> TestVault {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut hub = String::from("# Hub\n\nThe hub.\n\n");
    for i in 0..count {
        hub.push_str(&format!("Line [[L{i:02}]].\n\n"));
        files.push((format!("a/L{i:02}.md"), format!("# L{i:02}\n\n{}\n", long_summary(i))));
    }
    files.push(("a/Hub.md".to_string(), hub));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    TestVault::new(&refs)
}

#[test]
fn a_page_of_long_summaries_holds_fewer_links() {
    let v = long_vault(20);
    let index = v.index();
    // Worked out by hand from the rule: the id line, the heading, k link blocks and the "more" line must stay within
    // 1,000 tokens, which is 4,000 characters.
    let block = format!("- links_to: a/L00\n  {}\n", long_summary(0)).chars().count();
    let fits = |k: usize| {
        let closing = format!("{} more: page 2\n", 20 - k).chars().count();
        "a/Hub\n".len() + "\nLinks out:\n".len() + k * block + closing <= 4 * PAGE_TOKEN_BUDGET
    };
    let expected = (1..=20).take_while(|&k| fits(k)).last().unwrap();
    assert!(expected < PAGE_SIZE, "the fixture must overflow the budget");

    let page1 = run_page(&index, "a/Hub", 1);
    let page2 = run_page(&index, "a/Hub", 2);
    assert_eq!(page1.matches("\n- ").count(), expected, "{page1}");
    assert!(page1.ends_with(&format!("{} more: page 2\n", 20 - expected)), "{page1}");
    assert!(page2.ends_with(&format!("  {}\n", long_summary(19))), "{page2}");
    for page in [&page1, &page2] {
        assert!(page.chars().count().div_ceil(4) <= PAGE_TOKEN_BUDGET, "{page}");
    }
    // Every link appears once, in written order, each with its whole summary right below it.
    let all = format!("{page1}{page2}");
    for i in 0..20 {
        let block = format!("- links_to: a/L{i:02}\n  {}\n", long_summary(i));
        assert_eq!(all.matches(&block).count(), 1, "L{i:02}");
    }
    assert_eq!(
        run_page(&index, "a/Hub", 3),
        "Page 3 is past the end: 20 links on 2 pages.\n"
    );
}

#[test]
fn a_link_is_never_cut_even_alone_over_the_budget() {
    let v = long_vault(3);
    let index = v.index();
    let limits = Limits { links: 20, tokens: 10 };
    let page = |p| context::render(&context::context_with_limits(index.connection(), "a/Hub", p, limits).unwrap());
    for (p, more) in [(1, "2 more: page 2\n"), (2, "1 more: page 3\n"), (3, "")] {
        let i = p - 1;
        assert_eq!(
            page(p),
            format!(
                "a/Hub\n\nLinks out:\n- links_to: a/L{i:02}\n  {}\n{more}",
                long_summary(i)
            )
        );
    }
    assert_eq!(page(4), "Page 4 is past the end: 3 links on 3 pages.\n");
}

// The CLI twin (doc 07 §12) prints the same text.

fn cli(v: &TestVault, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_recognitor"))
        .args(args)
        .arg("--vault")
        .arg(&v.root)
        .env("RECOGNITOR_DB", &v.db)
        .output()
        .unwrap()
}

#[test]
fn cli_prints_the_context() {
    let v = example_vault();
    let out = cli(&v, &["context", "recognitor/Decisions"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXAMPLE);
}

#[test]
fn cli_pages_with_the_page_flag() {
    let v = wide_vault(15, 10);
    let index = v.index();
    let expected = run_page(&index, "a/Hub", 2);
    drop(index);
    let out = cli(&v, &["context", "a/Hub", "--page", "2"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}

#[test]
fn cli_lists_several_matches_as_a_normal_result() {
    let v = TestVault::new(&[
        ("research/Caching.md", "# Caching\n\nResearch.\n"),
        ("personal/Caching.md", "# Caching\n\nPersonal.\n"),
    ]);
    let out = cli(&v, &["context", "Caching"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("\"Caching\" matches 2 notes."));
}

#[test]
fn cli_fails_on_an_unknown_note() {
    let v = example_vault();
    let out = cli(&v, &["context", "Nothing here"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "error: \"Nothing here\" not found"
    );
}

#[test]
fn cli_needs_one_note() {
    let v = example_vault();
    for args in [vec!["context"], vec!["context", "a", "b"]] {
        let out = cli(&v, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

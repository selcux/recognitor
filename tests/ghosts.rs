//! `recognitor ghosts [<link>]` tests (doc 07 §2, §6.2, doc 05 §3.5, doc 03 §4). The docs give the summary of §6.2 and
//! the hint rule. The detail of `ghosts <link>` and the cases the docs leave open are the provisional shapes of
//! `src/ghosts.rs`. Vaults are synthetic and live in temp folders that delete themselves.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    base: PathBuf,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-ghosts-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let vault = TestVault { root, base };
        for (rel, text) in files {
            vault.write(rel, text);
        }
        vault
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn file(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn run(&self, args: &[&str]) -> Run {
        let out = Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .arg("--vault")
            .arg(&self.root)
            .env("RECOGNITOR_DB", self.base.join("index.db"))
            .env("RECOGNITOR_CONFIG", self.base.join("config.toml"))
            .env_remove("RECOGNITOR_VAULT")
            .output()
            .unwrap();
        Run {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn ghosts(&self) -> Run {
        self.run(&["ghosts"])
    }

    fn detail(&self, link: &str) -> Run {
        self.run(&["ghosts", link])
    }

    /// Runs a `recognitor relink` line as `ghosts` printed it: the quoted arguments, then `--vault "<path>"`. The test
    /// harness adds the vault itself, so the printed one is checked and dropped.
    fn run_printed(&self, line: &str) -> Run {
        let mut quoted: Vec<String> = Vec::new();
        let mut rest = line.strip_prefix("recognitor relink").expect("a relink line");
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            let end = after.find('"').expect("a closing quote");
            quoted.push(after[..end].to_string());
            rest = &after[end + 1..];
        }
        assert!(line.contains("--vault"), "the line repeats --vault: {line}");
        assert_eq!(
            quoted.pop().as_deref(),
            self.root.to_str(),
            "the vault is the last quoted part: {line}"
        );
        let mut args: Vec<&str> = vec!["relink"];
        args.extend(quoted.iter().map(String::as_str));
        self.run(&args)
    }

    /// Every file under the vault with its bytes, to prove a command wrote nothing.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push((p.clone(), fs::read(&p).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

const INDEX: &str = "# Index\n\nThe entry point.\n";

fn note(title: &str, body: &str) -> String {
    format!("# {title}\n\nA summary.\n\n{body}\n")
}

fn ok(r: &Run) -> &str {
    assert_eq!(r.code, Some(0), "{}{}", r.stdout, r.stderr);
    assert_eq!(r.stderr, "");
    &r.stdout
}

/// The state after Caching moved from `ideas/` to `design/` outside Recognitor (doc 07 §6.2): three links to the old
/// place, and an idea nobody wrote down yet.
fn moved_outside() -> TestVault {
    TestVault::new(&[
        ("bootstrap/index.md", INDEX),
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "A cache keeps answers close.")),
        ("ops/Runbook.md", &note("Runbook", "Reads [[ideas::Caching]] first.")),
        (
            "ops/Deploy.md",
            &note("Deploy", "See [[ideas::Caching]] and [[Gall's law]]."),
        ),
        (
            "ops/Rollback.md",
            &note("Rollback", "Needs [[ideas::Caching]] and [[Gall's Law]]."),
        ),
    ])
}

#[test]
fn the_summary_is_the_example_of_the_doc() {
    // Doc 07 §6.2: name, link count, and the hint only when one note has the filename. Counts descend. The names are
    // as written (doc 05 §3.5), and the columns line up.
    let v = moved_outside();
    let r = v.ghosts();
    assert_eq!(
        ok(&r),
        "ideas::Caching   3 links   \u{2192} maybe moved to design/Caching\n\
         Gall's Law       2 links\n"
    );
}

#[test]
fn spellings_that_differ_in_case_are_one_ghost() {
    let v = moved_outside();
    // `Gall's law` and `Gall's Law` are one row; MIN of the spellings is shown, as doc 05 §3.5 says.
    let r = v.ghosts();
    assert_eq!(ok(&r).matches("Gall's").count(), 1);
}

#[test]
fn a_singular_count_and_ties_in_name_order() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/A.md", &note("A", "Reads [[Zed]] and [[Alpha]] and [[Mid]].")),
        ("design/B.md", &note("B", "Reads [[Mid]].")),
    ]);
    assert_eq!(
        ok(&v.ghosts()),
        "Mid     2 links\n\
         Alpha   1 link\n\
         Zed     1 link\n"
    );
}

#[test]
fn a_vault_without_ghosts_says_so() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Reads [[index]].")),
    ]);
    assert_eq!(ok(&v.ghosts()), "No ghosts.\n");
}

#[test]
fn a_filename_shared_by_several_notes_gives_no_hint() {
    // Doc 07 §6.2: "for a shared name such as `index`, it shows nothing".
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("ops/index.md", INDEX),
        ("ops/Runbook.md", &note("Runbook", "Reads [[ideas::index]].")),
    ]);
    assert_eq!(ok(&v.ghosts()), "ideas::index   1 link\n");
}

#[test]
fn a_hint_looks_at_the_last_part_after_a_slash_or_a_prefix() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("design/Queue.md", &note("Queue", "Text.")),
        (
            "ops/Runbook.md",
            &note(
                "Runbook",
                "[[ideas::b/Caching]] and [[old/Queue]] and [[../ideas/Caching]].",
            ),
        ),
    ]);
    assert_eq!(
        ok(&v.ghosts()),
        "/ideas/caching     1 link   \u{2192} maybe moved to design/Caching\n\
         ideas::b/Caching   1 link   \u{2192} maybe moved to design/Caching\n\
         old/Queue          1 link   \u{2192} maybe moved to design/Queue\n"
    );
}

#[test]
fn a_hint_shows_the_title_when_it_differs_from_the_filename() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching strategies", "Text.")),
        ("ops/Runbook.md", &note("Runbook", "Reads [[ideas::Caching]].")),
    ]);
    assert_eq!(
        ok(&v.ghosts()),
        "ideas::Caching   1 link   \u{2192} maybe moved to design/Caching, titled \"Caching strategies\"\n"
    );
}

#[test]
fn an_ambiguous_link_is_listed_marked_and_has_no_hint() {
    // `a/Caching` has the filename and `b/x` the title, so `[[Caching]]` from `c` matches two notes (doc 03 §4, step
    // 5). Exactly one note has the filename, but the link names no moved note, so there is no hint.
    let v = TestVault::new(&[
        ("a/index.md", INDEX),
        ("a/Caching.md", &note("Other", "Text.")),
        ("b/x.md", &note("Caching", "Text.")),
        ("c/Note.md", &note("Note", "Reads [[Caching]].")),
    ]);
    let r = v.ghosts();
    assert_eq!(ok(&r), "Caching   1 link   (ambiguous)\n");
    let d = v.detail("Caching");
    assert_eq!(
        ok(&d),
        "Caching   1 link   (ambiguous)\n\
         matches 2 notes: a/Caching, b/x\n\
         - c/Note: line 5: links_to Caching\n"
    );
}

#[test]
fn the_detail_lists_the_linking_notes_and_the_command_that_fixes_them() {
    let v = moved_outside();
    let r = v.detail("ideas::Caching");
    let vault = v.root.display();
    assert_eq!(
        ok(&r),
        format!(
            "ideas::Caching   3 links   \u{2192} maybe moved to design/Caching\n\
             - ops/Deploy: line 5: links_to ideas::Caching\n\
             - ops/Rollback: line 5: links_to ideas::Caching\n\
             - ops/Runbook: line 5: links_to ideas::Caching\n\
             recognitor relink \"ideas/Caching\" \"design/Caching\" \"ops/Deploy\" \"ops/Rollback\" \"ops/Runbook\" \
             --vault \"{vault}\"\n"
        )
    );
}

#[test]
fn the_link_argument_is_matched_by_key_so_case_and_the_displayed_name_both_work() {
    let v = moved_outside();
    let by_key = v.detail("IDEAS::caching");
    let by_name = v.detail("ideas::Caching");
    assert_eq!(ok(&by_key), ok(&by_name));
    // A bare name with a quote in it, as the summary prints it.
    assert!(ok(&v.detail("gall's law")).starts_with("Gall's Law   2 links\n- ops/Deploy: line 5"));
}

#[test]
fn a_ghost_with_no_hint_has_no_command() {
    // Nothing moved: the idea is not written yet. A bare name never says where a note was, so no `relink` can be
    // built for it either.
    let v = moved_outside();
    let r = v.detail("Gall's law");
    assert_eq!(
        ok(&r),
        "Gall's Law   2 links\n\
         - ops/Deploy: line 5: links_to Gall's law\n\
         - ops/Rollback: line 5: links_to Gall's Law\n"
    );
}

#[test]
fn a_link_that_is_no_ghost_is_a_normal_result() {
    let v = moved_outside();
    let r = v.detail("Caching");
    assert_eq!(ok(&r), "No ghost matches \"Caching\".\n");
    assert_eq!(
        ok(&v.detail("nothing at all")),
        "No ghost matches \"nothing at all\".\n"
    );
}

#[test]
fn a_line_number_and_a_predicate_come_from_the_link() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        (
            "ops/Runbook.md",
            "# Runbook\n\nSummary.\n\nSome text.\n\n## Relations\n- part_of [[ideas::Caching]]\n",
        ),
    ]);
    assert_eq!(
        ok(&v.detail("ideas::Caching")),
        format!(
            "ideas::Caching   1 link   \u{2192} maybe moved to design/Caching\n\
             - ops/Runbook: line 8: part_of ideas::Caching\n\
             recognitor relink \"ideas/Caching\" \"design/Caching\" \"ops/Runbook\" --vault \"{}\"\n",
            v.root.display()
        )
    );
}

#[test]
fn a_linking_note_with_another_title_is_shown_as_everywhere() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("ops/Runbook.md", &note("Runbook v2", "Reads [[ideas::Caching]].")),
    ]);
    let d = v.detail("ideas::Caching");
    assert!(ok(&d).contains("- ops/Runbook, titled \"Runbook v2\": line 5: links_to ideas::Caching\n"));
    // The command names the id alone.
    assert!(d.stdout.contains(" \"ops/Runbook\" --vault"));
}

#[test]
fn a_relative_link_is_one_ghost_whatever_folder_wrote_it() {
    // `../ideas/Caching` from `ops/` and `../../ideas/Caching` from `ops/sub/` name the same file, so they are one
    // ghost, shown with its path from the vault root (doc 05 §2.2) because no one spelling fits both notes.
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("ops/Runbook.md", &note("Runbook", "Reads [[../ideas/Caching]].")),
        ("ops/sub/Deep.md", &note("Deep", "Reads [[../../ideas/Caching.md]].")),
        (
            "ops/sub/Other.md",
            &note("Other", "Reads [the cache](../../ideas/Caching.md)."),
        ),
    ]);
    let r = v.ghosts();
    assert_eq!(
        ok(&r),
        "/ideas/caching   3 links   \u{2192} maybe moved to design/Caching\n"
    );
    let d = v.detail("/ideas/caching");
    assert!(ok(&d).contains(
        "recognitor relink \"ideas/caching\" \"design/Caching\" \"ops/Runbook\" \"ops/sub/Deep\" \"ops/sub/Other\""
    ));
}

#[test]
fn a_link_written_as_text_may_name_several_ghosts() {
    // The same text from two folders is two ghosts, each with its own command.
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("ops/Runbook.md", &note("Runbook", "Reads [[../ideas/Caching]].")),
        ("ops/sub/Deep.md", &note("Deep", "Reads [[../ideas/Caching]].")),
    ]);
    let r = v.detail("../ideas/Caching");
    assert_eq!(
        ok(&r),
        format!(
            "/ideas/caching   1 link   \u{2192} maybe moved to design/Caching\n\
             - ops/Runbook: line 5: links_to ../ideas/Caching\n\
             recognitor relink \"ideas/caching\" \"design/Caching\" \"ops/Runbook\" --vault \"{v}\"\n\
             \n\
             /ops/ideas/caching   1 link   \u{2192} maybe moved to design/Caching\n\
             - ops/sub/Deep: line 5: links_to ../ideas/Caching\n\
             recognitor relink \"ops/ideas/caching\" \"design/Caching\" \"ops/sub/Deep\" --vault \"{v}\"\n",
            v = v.root.display()
        )
    );
}

#[test]
fn a_path_without_a_prefix_starts_at_the_linking_notes_namespace() {
    // `[[b/Caching]]` is `ops/b/Caching` in an `ops` note and `qa/b/Caching` in a `qa` note, so one ghost has two
    // `old` ids and two commands.
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("ops/Runbook.md", &note("Runbook", "Reads [[b/Caching]].")),
        ("qa/Plan.md", &note("Plan", "Reads [[b/Caching]].")),
    ]);
    assert_eq!(
        ok(&v.ghosts()),
        "b/Caching   2 links   \u{2192} maybe moved to design/Caching\n"
    );
    let d = v.detail("b/caching");
    let out = ok(&d);
    assert!(out.contains("recognitor relink \"ops/b/Caching\" \"design/Caching\" \"ops/Runbook\" --vault"));
    assert!(out.contains("recognitor relink \"qa/b/Caching\" \"design/Caching\" \"qa/Plan\" --vault"));
}

#[test]
fn the_printed_commands_fix_every_kind_of_ghost_they_hint_at() {
    // The printed `relink` line must do something: after it runs, nothing is left to list. One ghost of each kind that
    // gets a hint.
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("design/Queue.md", &note("Queue", "Text.")),
        ("design/Locks.md", &note("Locks", "Text.")),
        ("design/Pools.md", &note("Pools", "Text.")),
        ("ops/Prefixed.md", &note("Prefixed", "Reads [[ideas::Caching]].")),
        (
            "ops/PrefixedPath.md",
            &note("PrefixedPath", "Reads [[ideas::b/Queue]]."),
        ),
        ("ops/Relative.md", &note("Relative", "Reads [[../ideas/Locks]].")),
        (
            "ops/Markdown.md",
            &note("Markdown", "Reads [the pools](/ideas/Pools.md)."),
        ),
        ("ops/Plain.md", &note("Plain", "Reads [[b/Queue]] and [[x/Locks]].")),
        ("qa/Plain.md", &note("Plain", "Reads [[x/Locks]].")),
    ]);
    let before = v.snapshot();
    let mut commands: Vec<String> = Vec::new();
    for link in [
        "ideas::Caching",
        "ideas::b/Queue",
        "/ideas/locks",
        "/ideas/pools",
        "b/Queue",
        "x/Locks",
    ] {
        let r = v.detail(link);
        commands.extend(
            ok(&r)
                .lines()
                .filter(|l| l.starts_with("recognitor relink"))
                .map(str::to_string),
        );
    }
    assert_eq!(before, v.snapshot(), "ghosts writes nothing");
    assert_eq!(commands.len(), 7, "{commands:#?}");
    for line in &commands {
        let r = v.run_printed(line);
        assert!(ok(&r).starts_with("relinked: "), "{line}: {}", r.stdout);
        assert!(!r.stdout.contains("warning"), "{line}: {}", r.stdout);
    }
    assert_eq!(ok(&v.ghosts()), "No ghosts.\n", "{}", v.file("ops/Prefixed.md"));
    assert!(v.file("ops/Prefixed.md").contains("[[design::Caching]]"));
}

#[test]
fn bootstrap_notes_count_because_their_links_load() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Index\n\nEntry.\n\n## Relations\n- relates_to [[Profile]]\n",
        ),
        ("common/Profile2.md", &note("Profile2", "Text.")),
    ]);
    assert_eq!(ok(&v.ghosts()), "Profile   1 link\n");
    assert!(ok(&v.detail("Profile")).contains("- bootstrap/index: line 6: relates_to Profile\n"));
}

#[test]
fn archive_notes_have_no_links_so_no_ghosts() {
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("archive/design/Session.md", &note("Session", "Reads [[Nothing here]].")),
    ]);
    assert_eq!(ok(&v.ghosts()), "No ghosts.\n");
}

#[test]
fn a_note_edited_outside_is_seen_by_the_next_run() {
    let v = moved_outside();
    assert!(ok(&v.ghosts()).contains("ideas::Caching"));
    std::thread::sleep(std::time::Duration::from_millis(2100));
    v.write("ops/Runbook.md", &note("Runbook", "Reads nothing."));
    v.write("ops/Deploy.md", &note("Deploy", "Reads nothing."));
    v.write("ops/Rollback.md", &note("Rollback", "Reads nothing."));
    assert_eq!(
        ok(&v.ghosts()),
        "No ghosts.
"
    );
}

#[test]
fn bad_arguments_exit_2() {
    let v = moved_outside();
    let r = v.run(&["ghosts", "a", "b"]);
    assert_eq!(r.code, Some(2));
    assert!(r.stderr.starts_with("error: unexpected argument: b\n"), "{}", r.stderr);
    assert!(
        r.stderr.contains("recognitor ghosts [<link>]"),
        "the usage lists the command"
    );
    assert_eq!(r.stdout, "");
    // An option another command owns is no use here.
    let r = v.run(&["ghosts", "--strict"]);
    assert_eq!(r.code, Some(2));
    assert!(
        r.stderr.starts_with("error: --strict belongs to lint\n"),
        "{}",
        r.stderr
    );
}

#[test]
fn old_keeps_the_spelling_of_the_link() {
    // Doc 07 §12.3, "Spelling of `old`": a prefixed link and an unprefixed path keep the case the link wrote, and the
    // namespace of an unprefixed path is the linking note's folder as spelled on disk. Links that differ only in case
    // use the spelling the summary line shows (the smallest, as doc 05 §3.5 picks it). A relative link has only its
    // key.
    let v = TestVault::new(&[
        ("design/index.md", INDEX),
        ("design/Caching.md", &note("Caching", "Text.")),
        ("design/Ghost target.md", &note("Ghost target", "Text.")),
        ("design/Queue.md", &note("Queue", "Text.")),
        ("Tmp/Note.md", &note("Note", "Reads [[old/Ghost target]].")),
        ("ops/Runbook.md", &note("Runbook", "Reads [[Ideas::Old/Caching]].")),
        ("ops/Deploy.md", &note("Deploy", "Reads [[IDEAS::old/Caching]].")),
        ("ops/Relative.md", &note("Relative", "Reads [[../Ideas/Queue]].")),
    ]);
    let vault = v.root.display();
    assert_eq!(
        ok(&v.detail("old/ghost target")),
        format!(
            "old/Ghost target   1 link   \u{2192} maybe moved to design/Ghost target\n\
             - Tmp/Note: line 5: links_to old/Ghost target\n\
             recognitor relink \"Tmp/old/Ghost target\" \"design/Ghost target\" \"Tmp/Note\" --vault \"{vault}\"\n"
        )
    );
    // `IDEAS::old/Caching` sorts before `Ideas::Old/Caching`, so the summary shows it, and `old` follows.
    let line = format!(
        "recognitor relink \"IDEAS/old/Caching\" \"design/Caching\" \"ops/Deploy\" \"ops/Runbook\" --vault \"{vault}\""
    );
    let d = v.detail("ideas::old/caching");
    assert!(ok(&d).contains(&format!("{line}\n")), "{}", d.stdout);
    let relative = format!("recognitor relink \"ideas/queue\" \"design/Queue\" \"ops/Relative\" --vault \"{vault}\"");
    assert!(ok(&v.detail("/ideas/queue")).contains(&format!("{relative}\n")));
    // Whatever the spelling, the printed lines clear the ghosts.
    let first =
        format!("recognitor relink \"Tmp/old/Ghost target\" \"design/Ghost target\" \"Tmp/Note\" --vault \"{vault}\"");
    for line in [first, line, relative] {
        let r = v.run_printed(&line);
        assert!(ok(&r).starts_with("relinked: "), "{line}: {}", r.stdout);
        assert!(!r.stdout.contains("warning"), "{line}: {}", r.stdout);
    }
    assert_eq!(ok(&v.ghosts()), "No ghosts.\n");
}

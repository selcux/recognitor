//! Bootstrap tests (doc 04 §2.2, doc 05 §3.1, doc 07 §12). Vaults are synthetic and live in temp folders that delete
//! themselves.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::bootstrap::{self, Bootstrap, DEFAULT_MAX_TOKENS};
use recognitor::config;
use recognitor::db::{self, meta_keys};
use recognitor::{Error, Index, SweepOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
    config: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-bootstrap-{}-{n}", std::process::id()));
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

    /// An index whose sweep has run, so queries see the files.
    fn index(&self) -> Index {
        let mut index = Index::open(&self.root, Some(&self.db)).unwrap();
        index.rebuild(SweepOptions::default()).unwrap();
        index
    }

    /// The bootstrap of the vault as the walk finds it.
    fn walk(&self) -> Bootstrap {
        bootstrap::load(self.index().connection()).unwrap()
    }

    /// The binary with a hermetic environment: a config file in the temp folder, the scratch database, no vault
    /// setting from outside.
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_recognitor"))
            .args(args)
            .arg("--vault")
            .arg(&self.root)
            .env("RECOGNITOR_DB", &self.db)
            .env("RECOGNITOR_CONFIG", &self.config)
            .env_remove("RECOGNITOR_VAULT")
            .output()
            .unwrap()
    }

    /// Makes the vault root a place where `A` and `a` are two files, when the platform allows it (as in
    /// tests/indexer.rs). Returns false when it cannot, and the test skips.
    fn case_sensitive(&self) -> bool {
        if cfg!(windows) {
            let _ = Command::new("fsutil.exe")
                .args(["file", "setCaseSensitiveInfo"])
                .arg(&self.root)
                .arg("enable")
                .output();
        }
        let (a, b) = (self.root.join("probe_a"), self.root.join("PROBE_A"));
        fs::write(&a, "1").unwrap();
        fs::write(&b, "2").unwrap();
        let both = fs::read_dir(&self.root).unwrap().count() == 2;
        let _ = fs::remove_file(&a);
        let _ = fs::remove_file(&b);
        if !both {
            eprintln!("skipped: this file system folds case, so two files cannot be case twins here");
        }
        both
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The ids of the loaded notes, in load order.
fn ids(b: &Bootstrap) -> Vec<&str> {
    b.notes.iter().map(|n| n.id.as_str()).collect()
}

/// A bootstrap note: an H1, a summary paragraph, then `rest`.
fn note(title: &str, rest: &str) -> String {
    format!("# {title}\n\nSummary of {title}.\n\n{rest}")
}

/// The layout of doc 04 §2.1, with an index that links to three sections, each of which links on.
fn layout_vault() -> TestVault {
    TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- has_part [[Profile]]\n- has_part [[Style]]\n- has_part [[Global rules]]\n",
            ),
        ),
        ("bootstrap/profile/Profile.md", &note("Profile", "Works alone.\n")),
        (
            "bootstrap/behavior/Style.md",
            &note("Style", "## Relations\n- has_part [[Mentor]]\n"),
        ),
        ("bootstrap/behavior/personas/Mentor.md", &note("Mentor", "Patient.\n")),
        ("bootstrap/behavior/personas/Critic.md", &note("Critic", "Sharp.\n")),
        ("bootstrap/rules/Global rules.md", &note("Global rules", "Be brief.\n")),
    ])
}

// ---------------------------------------------------------------------------------------------------------------
// The walk (doc 04 §2.2 steps 1-4, doc 05 §3.1)
// ---------------------------------------------------------------------------------------------------------------

/// Steps 1-3: the index first, then its links depth-first in written order; every note reached loads before its own
/// links are followed. Selection is linking: `Critic` has no link to it, so it does not load.
#[test]
fn the_walk_is_depth_first_in_written_order_and_only_linked_notes_load() {
    let v = layout_vault();
    let b = v.walk();
    assert_eq!(
        ids(&b),
        [
            "bootstrap/index",
            "bootstrap/profile/Profile",
            "bootstrap/behavior/Style",
            "bootstrap/behavior/personas/Mentor",
            "bootstrap/rules/Global rules",
        ]
    );
    assert!(b.not_loaded.is_empty());
}

/// Doc 04 §2.2 "Selection is linking": switching persona means editing one link.
#[test]
fn switching_persona_means_editing_one_link() {
    let v = layout_vault();
    v.write(
        "bootstrap/behavior/Style.md",
        &note("Style", "## Relations\n- has_part [[Critic]]\n"),
    );
    let ids_now: Vec<String> = v.walk().notes.iter().map(|n| n.id.clone()).collect();
    assert!(ids_now.contains(&"bootstrap/behavior/personas/Critic".to_string()));
    assert!(!ids_now.contains(&"bootstrap/behavior/personas/Mentor".to_string()));
}

/// The order is the order written, not alphabetical and not by predicate.
#[test]
fn links_follow_the_order_they_are_written() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- has_part [[Zeta]]\n- indexes [[Alpha]]\n- has_part [[Mid]]\n",
            ),
        ),
        ("bootstrap/Alpha.md", &note("Alpha", "")),
        ("bootstrap/Mid.md", &note("Mid", "")),
        ("bootstrap/Zeta.md", &note("Zeta", "")),
    ]);
    assert_eq!(
        ids(&v.walk()),
        ["bootstrap/index", "bootstrap/Zeta", "bootstrap/Alpha", "bootstrap/Mid"]
    );
}

/// Depth first: the whole subtree of the first link loads before the second link is followed.
#[test]
fn a_subtree_loads_before_the_next_sibling() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[Hub]]\n- has_part [[Last]]\n"),
        ),
        (
            "bootstrap/Hub.md",
            &note("Hub", "## Relations\n- has_part [[One]]\n- has_part [[Two]]\n"),
        ),
        ("bootstrap/One.md", &note("One", "## Relations\n- has_part [[Deep]]\n")),
        ("bootstrap/Two.md", &note("Two", "")),
        ("bootstrap/Deep.md", &note("Deep", "")),
        ("bootstrap/Last.md", &note("Last", "")),
    ]);
    assert_eq!(
        ids(&v.walk()),
        [
            "bootstrap/index",
            "bootstrap/Hub",
            "bootstrap/One",
            "bootstrap/Deep",
            "bootstrap/Two",
            "bootstrap/Last"
        ]
    );
}

/// Step 4, "notes already visited": a link cycle and a link back to the index load each note once.
#[test]
fn a_cycle_loads_each_note_once() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[A]]\n- has_part [[B]]\n"),
        ),
        (
            "bootstrap/A.md",
            &note("A", "## Relations\n- has_part [[B]]\n- has_part [[index]]\n"),
        ),
        ("bootstrap/B.md", &note("B", "## Relations\n- has_part [[A]]\n")),
    ]);
    assert_eq!(ids(&v.walk()), ["bootstrap/index", "bootstrap/A", "bootstrap/B"]);
}

/// A note that links to itself.
#[test]
fn a_self_link_is_harmless() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[index]]\n- has_part [[A]]\n"),
        ),
        ("bootstrap/A.md", &note("A", "## Relations\n- has_part [[A]]\n")),
    ]);
    assert_eq!(ids(&v.walk()), ["bootstrap/index", "bootstrap/A"]);
}

/// Step 4, second bullet: inline `[[mentions]]` in prose are never followed, not even in the index.
#[test]
fn inline_mentions_are_not_followed() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "See also [[Prose target]] for more.\n\n- Mother [[Bullet target]]\n",
            ),
        ),
        ("bootstrap/Prose target.md", &note("Prose target", "")),
        ("bootstrap/Bullet target.md", &note("Bullet target", "")),
    ]);
    assert_eq!(ids(&v.walk()), ["bootstrap/index"]);
}

/// Step 4, first bullet: a `supersedes` link points at a retired version, which does not load, while a `has_part`
/// link next to it does.
#[test]
fn supersedes_links_are_not_followed() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- supersedes [[Old rules]] (replaced in 2026)\n- has_part [[New rules]]\n",
            ),
        ),
        ("bootstrap/Old rules.md", &note("Old rules", "")),
        ("bootstrap/New rules.md", &note("New rules", "")),
    ]);
    assert_eq!(ids(&v.walk()), ["bootstrap/index", "bootstrap/New rules"]);
}

/// Doc 04 §2.2 steps 2 and 4: only `indexes` and `has_part` point down the tree. Any other relation word, in a loaded
/// note, leaves its target unloaded, and a missing target behind such a word is not a ghost line either.
#[test]
fn only_indexes_and_has_part_are_followed() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n\
                 - relates_to [[Related]]\n\
                 - loads [[Loaded]]\n\
                 - part_of [[Parent]]\n\
                 - supersedes [[Retired]]\n\
                 - depends_on [[Depended]]\n\
                 - relates_to [[Missing one]]\n\
                 - loads [[Missing two]]\n\
                 - indexes [[Hub]]\n\
                 - has_part [[Leaf]]\n",
            ),
        ),
        ("bootstrap/Related.md", &note("Related", "")),
        ("bootstrap/Loaded.md", &note("Loaded", "")),
        ("bootstrap/Parent.md", &note("Parent", "")),
        ("bootstrap/Retired.md", &note("Retired", "")),
        ("bootstrap/Depended.md", &note("Depended", "")),
        (
            "bootstrap/Hub.md",
            &note(
                "Hub",
                "## Relations\n- relates_to [[Hub side]]\n- has_part [[Hub leaf]]\n",
            ),
        ),
        ("bootstrap/Hub side.md", &note("Hub side", "")),
        ("bootstrap/Hub leaf.md", &note("Hub leaf", "")),
        ("bootstrap/Leaf.md", &note("Leaf", "")),
    ]);
    let b = v.walk();
    assert_eq!(
        ids(&b),
        [
            "bootstrap/index",
            "bootstrap/Hub",
            "bootstrap/Hub leaf",
            "bootstrap/Leaf"
        ]
    );
    assert!(b.not_loaded.is_empty());
    assert!(!b.payload().contains("not loaded"));
}

/// The relation word is compared in lowercase: `Has_Part` and `INDEXES` are followed. The ghost line is the same.
#[test]
fn the_relation_word_is_compared_in_lowercase() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- Has_Part [[A]]\n- INDEXES [[B]]\n- Indexes [[Gone]]\n- RELATES_TO [[C]]\n",
            ),
        ),
        ("bootstrap/A.md", &note("A", "")),
        ("bootstrap/B.md", &note("B", "")),
        ("bootstrap/C.md", &note("C", "")),
    ]);
    let b = v.walk();
    assert_eq!(ids(&b), ["bootstrap/index", "bootstrap/A", "bootstrap/B"]);
    assert_eq!(b.not_loaded, ["Gone"]);
}

/// A cross-reference with `relates_to` does not change the load order: the note loads where its `has_part` or
/// `indexes` parent puts it, after the sibling that comes first, and not where the cross-reference sits.
#[test]
fn a_cross_reference_does_not_change_the_load_order() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- relates_to [[Shared]]\n- has_part [[A]]\n- indexes [[Hub]]\n",
            ),
        ),
        ("bootstrap/A.md", &note("A", "## Relations\n- relates_to [[Shared]]\n")),
        (
            "bootstrap/Hub.md",
            &note("Hub", "## Relations\n- has_part [[Shared]]\n"),
        ),
        ("bootstrap/Shared.md", &note("Shared", "")),
    ]);
    assert_eq!(
        ids(&v.walk()),
        ["bootstrap/index", "bootstrap/A", "bootstrap/Hub", "bootstrap/Shared"]
    );
}

/// Step 4, third bullet: a `part_of` written in the target is not an edge out of the source. The index does not
/// link to `Child`, so `Child` does not load, although it points at the index.
#[test]
fn reverse_direction_links_are_not_followed() {
    let v = TestVault::new(&[
        ("bootstrap/index.md", &note("Bootstrap", "")),
        (
            "bootstrap/Child.md",
            &note("Child", "## Relations\n- part_of [[Bootstrap]]\n"),
        ),
    ]);
    assert_eq!(ids(&v.walk()), ["bootstrap/index"]);
}

/// Step 4, fourth bullet: links that leave the bootstrap are not followed, and they are not listed as not loaded
/// either (only ghosts are). Archive and knowledge targets alike.
#[test]
fn links_out_of_the_bootstrap_are_not_followed() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- has_part [[knowledge::Deploy]]\n- indexes [[archive::Session 1]]\n\
                 - has_part [[Inside]]\n",
            ),
        ),
        (
            "bootstrap/Inside.md",
            &note("Inside", "## Relations\n- has_part [[Deploy]]\n"),
        ),
        ("knowledge/Deploy.md", &note("Deploy", "")),
        ("archive/Session 1.md", "# Session 1\n\nRaw.\n"),
    ]);
    let b = v.walk();
    assert_eq!(ids(&b), ["bootstrap/index", "bootstrap/Inside"]);
    assert!(b.not_loaded.is_empty());
    assert_eq!(b.payload().matches("not loaded").count(), 0);
}

/// Step 3 of link resolution (doc 03 §4) prefers the source note's namespace: `[[Rules]]` in the bootstrap reaches
/// the bootstrap's `Rules`, not a knowledge note of the same name.
#[test]
fn a_name_link_prefers_the_bootstrap_note_of_that_name() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[Rules]]\n"),
        ),
        ("bootstrap/Rules.md", &note("Rules", "Global.\n")),
        ("project/Rules.md", &note("Rules", "Project.\n")),
    ]);
    assert_eq!(ids(&v.walk()), ["bootstrap/index", "bootstrap/Rules"]);
}

/// Doc 04 §2.2 step 4: a line under `## Relations` without a relation word (`- [[Mentor]]`) counts as a plain link
/// (doc 03 §3). It is not followed, and the payload does not mention it: no note, no `not loaded` line.
#[test]
fn a_relations_line_of_another_shape_is_not_followed() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- [[Bare]]\n- see also [[Spaced]]\n- has_part [[Typed]]\n",
            ),
        ),
        ("bootstrap/Bare.md", &note("Bare", "")),
        ("bootstrap/Spaced.md", &note("Spaced", "")),
        ("bootstrap/Typed.md", &note("Typed", "")),
    ]);
    let b = v.walk();
    assert_eq!(ids(&b), ["bootstrap/index", "bootstrap/Typed"]);
    assert!(b.not_loaded.is_empty());
    assert!(!b.payload().contains("not loaded"));
    // A plain link to a note that does not exist is not listed either.
    v.write(
        "bootstrap/index.md",
        &note("Bootstrap", "## Relations\n- [[Nowhere]]\n- has_part [[Typed]]\n"),
    );
    let b = v.walk();
    assert!(b.not_loaded.is_empty());
}

/// A bootstrap note without frontmatter, H1 or summary still loads; its title is the filename.
#[test]
fn casual_notes_load_too() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[casual]]\n"),
        ),
        ("bootstrap/casual.md", "Just a line of text.\n"),
    ]);
    let b = v.walk();
    assert_eq!(b.notes[1].title, "casual");
    assert_eq!(b.notes[1].body, "Just a line of text.");
}

/// The entry point is found whatever its case, and so is the namespace folder (doc 03 §4).
#[test]
fn the_entry_point_is_found_whatever_its_case() {
    let v = TestVault::new(&[
        (
            "Bootstrap/Index.md",
            &note("Bootstrap", "## Relations\n- has_part [[Rules]]\n"),
        ),
        ("Bootstrap/Rules.md", &note("Rules", "")),
    ]);
    assert_eq!(ids(&v.walk()), ["Bootstrap/Index", "Bootstrap/Rules"]);
}

/// A long chain does not overflow the stack: the walk keeps its own stack.
#[test]
fn a_long_chain_loads() {
    let mut files = vec![(
        "bootstrap/index.md".to_string(),
        note("Bootstrap", "## Relations\n- has_part [[N0]]\n"),
    )];
    let n = 3000;
    for i in 0..n {
        let rest = if i + 1 < n {
            format!("## Relations\n- has_part [[N{}]]\n", i + 1)
        } else {
            String::new()
        };
        files.push((format!("bootstrap/N{i}.md"), note(&format!("N{i}"), &rest)));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let v = TestVault::new(&refs);
    let b = v.walk();
    assert_eq!(b.notes.len(), n + 1);
    assert_eq!(b.notes[n].id, format!("bootstrap/N{}", n - 1));
}

// ---------------------------------------------------------------------------------------------------------------
// No entry point (doc 04 §2.2, "No entry point")
// ---------------------------------------------------------------------------------------------------------------

const NO_ENTRY_LINE: &str = "error: bootstrap/index.md not found. Run `recognitor lint`.";

fn assert_no_entry(v: &TestVault) {
    // The library: one error whose text is the line without the CLI's `error: ` prefix.
    let err = bootstrap::load(v.index().connection()).unwrap_err();
    assert!(matches!(err, Error::NoBootstrap));
    assert!(!err.is_usage());
    assert_eq!(format!("error: {err}"), NO_ENTRY_LINE);
    // The CLI prints the same line, prints no payload, and exits non-zero.
    for args in [&["bootstrap"][..], &["bootstrap", "--list"][..]] {
        let out = v.cli(args);
        assert_eq!(stdout(&out), "", "{args:?}");
        assert_eq!(stderr(&out), format!("{NO_ENTRY_LINE}\n"), "{args:?}");
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
}

/// A vault without a `bootstrap/` folder is valid; `load_bootstrap` then returns the error line.
#[test]
fn no_bootstrap_folder_is_one_error_line() {
    assert_no_entry(&TestVault::new(&[
        ("baseline/index.md", &note("Baseline", "")),
        ("compass/index.md", &note("Compass", "")),
    ]));
}

/// A `bootstrap/` folder without `index.md`: `load_bootstrap` reads only the index, so the other notes do not load.
#[test]
fn a_bootstrap_folder_without_an_index_is_one_error_line() {
    assert_no_entry(&TestVault::new(&[
        ("bootstrap/Rules.md", &note("Rules", "")),
        ("bootstrap/profile/Profile.md", &note("Profile", "")),
    ]));
}

/// An empty vault.
#[test]
fn an_empty_vault_is_one_error_line() {
    assert_no_entry(&TestVault::new(&[]));
}

/// A case twin of the entry point: neither file is indexed, so the entry is missing (doc 03 §4).
#[test]
fn a_case_twin_entry_is_one_error_line() {
    let v = TestVault::new(&[]);
    if !v.case_sensitive() {
        return;
    }
    v.write(
        "bootstrap/index.md",
        &note("Bootstrap", "## Relations\n- has_part [[Rules]]\n"),
    );
    v.write("bootstrap/Index.md", &note("Bootstrap", ""));
    v.write("bootstrap/Rules.md", &note("Rules", ""));
    assert_no_entry(&v);
}

/// Twin folders `Bootstrap/` and `bootstrap/` leave out every note under both (doc 03 §4).
#[test]
fn twin_bootstrap_folders_are_one_error_line() {
    let v = TestVault::new(&[]);
    if !v.case_sensitive() {
        return;
    }
    v.write("bootstrap/index.md", &note("Bootstrap", ""));
    v.write("Bootstrap/index.md", &note("Bootstrap", ""));
    assert_no_entry(&v);
}

/// The error clears when the entry appears: the next call sees the file (the sweep runs first).
#[test]
fn the_entry_appears_after_the_next_sweep() {
    let v = TestVault::new(&[("baseline/index.md", &note("Baseline", ""))]);
    let mut index = v.index();
    assert!(matches!(index.bootstrap(), Err(Error::NoBootstrap)));
    v.write("bootstrap/index.md", &note("Bootstrap", ""));
    // The 2-second gate (doc 02 §4) hides the file until it expires; expire it as the bench does.
    db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    assert_eq!(ids(&index.bootstrap().unwrap()), ["bootstrap/index"]);
}

// ---------------------------------------------------------------------------------------------------------------
// Ghosts (doc 04 §2.2, "Ghost links in the tree")
// ---------------------------------------------------------------------------------------------------------------

/// The doc's example line, for one ghost, at the end of the payload.
#[test]
fn a_ghost_link_ends_the_payload_with_one_line() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[Style]]\n"),
        ),
        (
            "bootstrap/Style.md",
            &note("Style", "## Relations\n- has_part [[behavior/personas/Mentor]]\n"),
        ),
    ]);
    let b = v.walk();
    assert_eq!(b.not_loaded, ["behavior/personas/Mentor"]);
    let payload = b.payload();
    assert!(
        payload.ends_with("not loaded: behavior/personas/Mentor (not written yet)\n"),
        "{payload}"
    );
    assert_eq!(payload.lines().filter(|l| l.starts_with("not loaded")).count(), 1);
}

/// Without ghosts, the line is left out.
#[test]
fn without_ghosts_there_is_no_line() {
    let v = layout_vault();
    assert!(!v.walk().payload().contains("not loaded"));
}

/// Ghosts in the walk order, each once, `, `-separated on one line (doc 04 §2.2).
#[test]
fn several_ghosts_share_one_line_in_walk_order() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note(
                "Bootstrap",
                "## Relations\n- has_part [[Second]]\n- has_part [[Hub]]\n- has_part [[Second]]\n",
            ),
        ),
        (
            "bootstrap/Hub.md",
            &note("Hub", "## Relations\n- has_part [[First]]\n- has_part [[Second]]\n"),
        ),
    ]);
    let b = v.walk();
    assert_eq!(b.not_loaded, ["Second", "First"]);
    assert!(
        b.payload()
            .ends_with("not loaded: Second (not written yet), First (not written yet)\n")
    );
}

/// Two notes called `Tone` in the bootstrap: `[[Tone]]` matches both (doc 03 §4, step 3), so it is an ambiguous ghost.
fn ambiguous_vault(index_links: &str) -> TestVault {
    TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", &format!("## Relations\n{index_links}")),
        ),
        ("bootstrap/Tone.md", &note("Tone", "One.\n")),
        ("bootstrap/behavior/Tone.md", &note("Tone", "Two.\n")),
    ])
}

/// Doc 04 §2.2: a ghost that more than one note matches says `(ambiguous)` instead of `(not written yet)`.
#[test]
fn an_ambiguous_ghost_says_so() {
    let v = ambiguous_vault("- has_part [[Tone]]\n");
    let b = v.walk();
    assert_eq!(ids(&b), ["bootstrap/index"]);
    assert_eq!(b.not_loaded, ["Tone"]);
    assert!(
        b.payload().ends_with("\nnot loaded: Tone (ambiguous)\n"),
        "{}",
        b.payload()
    );
    // In the `--list` tree it stands where its link is.
    assert_eq!(
        stdout(&v.cli(&["bootstrap", "--list"])),
        "bootstrap/index.md\n\u{2514}\u{254c}\u{254c} Tone  (ambiguous)\n"
    );
}

/// The doc's own example: a missing note and an ambiguous one share the line, in the order the links are written.
#[test]
fn a_missing_and_an_ambiguous_ghost_share_the_line() {
    let v = ambiguous_vault("- has_part [[Mentor]]\n- has_part [[Tone]]\n");
    assert!(
        v.walk()
            .payload()
            .ends_with("\nnot loaded: Mentor (not written yet), Tone (ambiguous)\n")
    );
    // Written the other way round, the order follows the links.
    v.write(
        "bootstrap/index.md",
        &note(
            "Bootstrap",
            "## Relations\n- has_part [[Tone]]\n- has_part [[Mentor]]\n",
        ),
    );
    assert!(
        v.walk()
            .payload()
            .ends_with("\nnot loaded: Tone (ambiguous), Mentor (not written yet)\n")
    );
}

/// The same ambiguous target linked from two notes is listed once.
#[test]
fn an_ambiguous_ghost_linked_twice_is_listed_once() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            &note("Bootstrap", "## Relations\n- has_part [[Tone]]\n- has_part [[Hub]]\n"),
        ),
        ("bootstrap/Hub.md", &note("Hub", "## Relations\n- has_part [[Tone]]\n")),
        ("bootstrap/Tone.md", &note("Tone", "One.\n")),
        ("bootstrap/behavior/Tone.md", &note("Tone", "Two.\n")),
    ]);
    let b = v.walk();
    assert_eq!(ids(&b), ["bootstrap/index", "bootstrap/Hub"]);
    assert_eq!(b.not_loaded, ["Tone"]);
    assert_eq!(b.payload().matches("Tone (ambiguous)").count(), 1);
}

/// Ambiguity is read from the index at walk time: renaming one of the twins heals the link, which then loads, and the
/// `not loaded` line goes. Matching by title counts too: a second note titled `Tone` makes `[[Tone]]` ambiguous.
#[test]
fn an_ambiguous_ghost_heals_when_one_note_is_renamed() {
    let v = ambiguous_vault("- has_part [[Tone]]\n");
    let mut index = v.index();
    assert!(index.bootstrap().unwrap().payload().contains("Tone (ambiguous)"));
    fs::rename(
        v.root.join("bootstrap/behavior/Tone.md"),
        v.root.join("bootstrap/behavior/Pitch.md"),
    )
    .unwrap();
    // The note keeps the H1 `Tone` in its text, so the title still matches: still ambiguous.
    db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    assert!(index.bootstrap().unwrap().payload().contains("Tone (ambiguous)"));
    // Retitle it too, and the link resolves to the one note left.
    v.write("bootstrap/behavior/Pitch.md", &note("Pitch", "Two.\n"));
    db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    let b = index.bootstrap().unwrap();
    assert_eq!(ids(&b), ["bootstrap/index", "bootstrap/Tone"]);
    assert!(b.not_loaded.is_empty() && b.ambiguous.is_empty());
    assert!(!b.payload().contains("not loaded"));
}

/// A ghost link behind a word the walk does not follow is not listed: the walk never looks at it (doc 05 §3.1).
#[test]
fn a_superseded_ghost_is_not_listed() {
    let v = TestVault::new(&[(
        "bootstrap/index.md",
        &note("Bootstrap", "## Relations\n- supersedes [[Gone]]\n"),
    )]);
    assert!(v.walk().not_loaded.is_empty());
}

/// A ghost heals: once the note is written, it loads. (Doc 03 §4, "Ghosts heal".)
#[test]
fn a_ghost_heals_when_the_note_appears() {
    let v = TestVault::new(&[(
        "bootstrap/index.md",
        &note("Bootstrap", "## Relations\n- has_part [[Later]]\n"),
    )]);
    let mut index = v.index();
    assert_eq!(index.bootstrap().unwrap().not_loaded, ["Later"]);
    v.write("bootstrap/Later.md", &note("Later", "Here now.\n"));
    db::meta_set(index.connection(), meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    let b = index.bootstrap().unwrap();
    assert_eq!(ids(&b), ["bootstrap/index", "bootstrap/Later"]);
    assert!(b.not_loaded.is_empty());
}

// ---------------------------------------------------------------------------------------------------------------
// The payload (doc 04 §2.2 step 5)
// ---------------------------------------------------------------------------------------------------------------

/// A vault of an index that links to the given notes, in order, and nothing else.
fn vault_with(notes: &[(&str, &str)]) -> TestVault {
    let links: String = notes
        .iter()
        .map(|(name, _)| format!("- has_part [[{name}]]\n"))
        .collect();
    let index = format!("# Bootstrap\n\nIntro.\n\n## Relations\n{links}");
    let mut files = vec![("bootstrap/index.md".to_string(), index)];
    for (name, text) in notes {
        files.push((format!("bootstrap/{name}.md"), (*text).to_string()));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    TestVault::new(&refs)
}

/// Step 5: each note's content as written, frontmatter and `## Relations` stripped, a blank line between notes.
/// A note that starts with its own `# ` heading gets no second one.
#[test]
fn the_payload_is_each_notes_content_as_written() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "---\ntitle: Start here\ntype: manifest\n---\n\n# Start here\n\nIntro text.\n\n## Relations\n\
             - has_part [[Rules]]\n",
        ),
        (
            "bootstrap/Rules.md",
            "---\ntags: [a]\n---\n# Rules\n\nBe brief.\n\n- [rule] Never guess.\n\n\
             ## Relations\n- part_of [[Start here]]\n",
        ),
    ]);
    assert_eq!(
        v.walk().payload(),
        "# Start here\n\nIntro text.\n\n# Rules\n\nBe brief.\n\n- [rule] Never guess.\n"
    );
}

/// A note without a heading line gets `# <title>` above it: its frontmatter title, or, with none, its file name.
#[test]
fn a_note_without_a_heading_gets_its_title_added() {
    let v = vault_with(&[
        ("a", "---\ntitle: From frontmatter\n---\nText A.\n"),
        ("b", "Text B.\n"),
    ]);
    assert_eq!(
        v.walk().payload(),
        "# Bootstrap\n\nIntro.\n\n# From frontmatter\n\nText A.\n\n# b\n\nText B.\n"
    );
}

/// A note whose first heading is not an H1 (`## Section`) still gets the title heading above it; so does a first
/// line that only looks like one (`#tag`, no space). The added heading is the note's title, which is its first H1
/// wherever that is (doc 03 §2), so `c` and `d` get the text of their late and indented H1.
#[test]
fn a_first_heading_that_is_not_an_h1_does_not_count() {
    let v = vault_with(&[
        ("a", "## Section\n\nText A.\n"),
        ("b", "#tag line\n\nText B.\n"),
        ("c", "Text first.\n\n# Late heading\n"),
        ("d", "  # indented\n"),
    ]);
    assert_eq!(
        v.walk().payload(),
        "# Bootstrap\n\nIntro.\n\n\
         # a\n\n## Section\n\nText A.\n\n\
         # b\n\n#tag line\n\nText B.\n\n\
         # Late heading\n\nText first.\n\n# Late heading\n\n\
         # indented\n\n  # indented\n"
    );
}

/// A note whose own `# ` heading differs from its title is left as written: no second heading.
#[test]
fn an_own_heading_that_differs_from_the_title_is_left_as_written() {
    let v = vault_with(&[(
        "a",
        "---\ntitle: Frontmatter title\n---\n# Another heading\n\nText A.\n",
    )]);
    let b = v.walk();
    assert_eq!(b.notes[1].title, "Frontmatter title");
    assert_eq!(b.payload(), "# Bootstrap\n\nIntro.\n\n# Another heading\n\nText A.\n");
}

/// Blank lines in front of the first line do not hide its heading, and a heading comes after the frontmatter, not
/// before it.
#[test]
fn blank_lines_before_the_heading_are_skipped() {
    let v = vault_with(&[("a", "---\ntitle: T\n---\n\n\n\n# Own\n\nText A.\n")]);
    assert_eq!(v.walk().payload(), "# Bootstrap\n\nIntro.\n\n# Own\n\nText A.\n");
}

/// The heading carries the note's title: frontmatter title, else H1, else filename (doc 03 §2).
#[test]
fn the_added_heading_is_the_notes_title() {
    let v = vault_with(&[
        ("a", "---\ntitle: From frontmatter\n---\nText A.\n"),
        ("b", "Text B.\n"),
    ]);
    let titles: Vec<String> = v.walk().notes.iter().map(|n| n.title.clone()).collect();
    assert_eq!(titles, ["Bootstrap", "From frontmatter", "b"]);
}

/// Observation bullets are not removed from a bootstrap body: bootstrap notes are loaded whole (doc 05 §2.3).
#[test]
fn observations_stay_in_the_body() {
    let v = TestVault::new(&[(
        "bootstrap/index.md",
        &note("Bootstrap", "- [rule] @effective:2026-01-01 Answer first. #style\n"),
    )]);
    assert!(
        v.walk()
            .payload()
            .contains("- [rule] @effective:2026-01-01 Answer first. #style")
    );
}

/// `body_of`: what is stripped, case by case.
#[test]
fn body_stripping() {
    use bootstrap::body_of;
    // Frontmatter only at the very start, and only when closed.
    assert_eq!(body_of("---\na: 1\n---\nText\n"), "Text");
    assert_eq!(body_of("---\na: 1\n---\n\n\nText\n"), "Text");
    assert_eq!(body_of("\u{feff}---\na: 1\n---\nText\n"), "Text");
    assert_eq!(body_of("Text\n\n---\na: 1\n---\n"), "Text\n\n---\na: 1\n---");
    assert_eq!(body_of("---\nnever closed\n"), "---\nnever closed");
    // A relations section runs to the next H1 or H2.
    assert_eq!(
        body_of("# T\n\nA.\n\n## Relations\n- x [[Y]]\n\n## Later\nKept.\n"),
        "# T\n\nA.\n\n## Later\nKept."
    );
    // An H3 inside the section does not end it.
    assert_eq!(
        body_of("# T\n\n## Relations\n- x [[Y]]\n\n### Notes\nStill relations.\n\n# Next\nKept.\n"),
        "# T\n\n# Next\nKept."
    );
    // Without a following heading it runs to the end of the file; nothing after it is kept.
    assert_eq!(body_of("# T\n\nA.\n\n## Relations\n- x [[Y]]\n"), "# T\n\nA.");
    // More than one section: all of them go.
    assert_eq!(
        body_of("A.\n\n## Relations\n- x [[Y]]\n\n## Mid\nM.\n\n## Relations\n- x [[Z]]\n"),
        "A.\n\n## Mid\nM."
    );
    // The heading must be an H2 spelled exactly `Relations`.
    let others = "# Relations\n- x [[Y]]\n\n### Relations\n- x [[Y]]\n\n## relations\n- x [[Y]]\n";
    assert_eq!(body_of(others), others.trim_end());
    // A `## Relations` line in a code block is code.
    let code = "Text.\n\n```\n## Relations\n- x [[Y]]\n```\n\nAfter.\n";
    assert_eq!(body_of(code), code.trim_end());
    // CRLF files keep their line endings: nothing is changed but what is stripped.
    assert_eq!(
        body_of("---\r\na: 1\r\n---\r\n# T\r\n\r\nA.\r\n\r\n## Relations\r\n- x [[Y]]\r\n"),
        "# T\r\n\r\nA."
    );
    // An empty body.
    assert_eq!(body_of("---\na: 1\n---\n"), "");
    assert_eq!(body_of("## Relations\n- x [[Y]]\n"), "");
    // A Relations heading inside a blockquote cuts whole lines, as the parser's section does.
    assert_eq!(
        body_of("A.\n\n> ## Relations\n> - x [[Y]]\n\n## Next\nB.\n"),
        "A.\n\n## Next\nB."
    );
}

/// An empty body gives the title heading and nothing else.
#[test]
fn a_note_with_no_body_is_only_a_heading() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "---\ntitle: Start\n---\n## Relations\n- has_part [[Empty]]\n",
        ),
        ("bootstrap/Empty.md", "---\ntitle: Empty one\n---\n"),
    ]);
    assert_eq!(v.walk().payload(), "# Start\n\n# Empty one\n");
}

// ---------------------------------------------------------------------------------------------------------------
// `--list` (doc 04 §2.2 "CLI twin"): a tree in load order, each node the link as written, an arrow and the file name
// ---------------------------------------------------------------------------------------------------------------

/// The example of doc 04 §2.2, line for line: a hub under the index, a note with two children, a title link whose file
/// name differs, a link to a note that loaded earlier, and two ghosts on dashed branches.
#[test]
fn list_prints_the_doc_example() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Index\n\nS.\n\n## Relations\n- indexes [[Profile]]\n",
        ),
        (
            "bootstrap/profile/Profile.md",
            "# Profile\n\nS.\n\n## Relations\n- has_part [[identity]]\n- has_part [[Tools & sourcing]]\n\
             - has_part [[languages]]\n- has_part [[Mentor]]\n- has_part [[Tone]]\n",
        ),
        (
            "bootstrap/profile/identity.md",
            "# identity\n\nS.\n\n## Relations\n- has_part [[languages]]\n- has_part [[spoken english]]\n",
        ),
        ("bootstrap/profile/languages.md", "# languages\n\nS.\n"),
        ("bootstrap/profile/spoken english.md", "# Spoken English\n\nS.\n"),
        ("bootstrap/profile/tools.md", "# Tools & sourcing\n\nS.\n"),
        ("bootstrap/Tone.md", &note("Tone", "One.\n")),
        ("bootstrap/behavior/Tone.md", &note("Tone", "Two.\n")),
    ]);
    let out = v.cli(&["bootstrap", "--list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "bootstrap/index.md\n\
         \u{2514}\u{2500}\u{2500} Profile \u{2192} Profile.md\n\
         \x20   \u{251c}\u{2500}\u{2500} identity \u{2192} identity.md\n\
         \x20   \u{2502}   \u{251c}\u{2500}\u{2500} languages \u{2192} languages.md\n\
         \x20   \u{2502}   \u{2514}\u{2500}\u{2500} spoken english \u{2192} spoken english.md\n\
         \x20   \u{251c}\u{2500}\u{2500} Tools & sourcing \u{2192} tools.md\n\
         \x20   \u{251c}\u{2500}\u{2500} languages \u{2192} languages.md  (already loaded)\n\
         \x20   \u{251c}\u{254c}\u{254c} Mentor  (not written yet)\n\
         \x20   \u{2514}\u{254c}\u{254c} Tone  (ambiguous)\n"
    );
    assert_eq!(stderr(&out), "");
    // Same walk as the payload: the load order is the order the tree was drawn in.
    let b = v.walk();
    assert_eq!(
        ids(&b),
        [
            "bootstrap/index",
            "bootstrap/profile/Profile",
            "bootstrap/profile/identity",
            "bootstrap/profile/languages",
            "bootstrap/profile/spoken english",
            "bootstrap/profile/tools"
        ]
    );
    // The payload keeps its own ghost line, and `--list` carries no `not loaded` line and no warning.
    assert!(
        b.payload()
            .ends_with("\nnot loaded: Mentor (not written yet), Tone (ambiguous)\n")
    );
    assert!(!stdout(&out).contains("not loaded") && !stdout(&out).contains("warning"));
}

/// A deep and wide tree: the last child of each level gets the closing connector, the others the branching one, and
/// a level under a last child is indented with spaces, under another child with a vertical bar.
#[test]
fn list_draws_connectors_for_a_deep_and_wide_tree() {
    let leaf = |name: &str| (format!("bootstrap/{name}.md"), format!("# {name}\n\nS.\n"));
    let hub = |name: &str, kids: &[&str]| {
        let links: String = kids.iter().map(|k| format!("- has_part [[{k}]]\n")).collect();
        (
            format!("bootstrap/{name}.md"),
            format!("# {name}\n\nS.\n\n## Relations\n{links}"),
        )
    };
    let files = vec![
        hub("index", &["a", "b", "c"]),
        hub("a", &["a1", "a2"]),
        hub("a1", &["a1x"]),
        leaf("a1x"),
        hub("a2", &["a2x", "a2y"]),
        leaf("a2x"),
        leaf("a2y"),
        leaf("b"),
        hub("c", &["c1"]),
        hub("c1", &["c1x", "c1y"]),
        leaf("c1x"),
        leaf("c1y"),
    ];
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let v = TestVault::new(&refs);
    let tree = "bootstrap/index.md\n\
        \u{251c}\u{2500}\u{2500} a \u{2192} a.md\n\
        \u{2502}   \u{251c}\u{2500}\u{2500} a1 \u{2192} a1.md\n\
        \u{2502}   \u{2502}   \u{2514}\u{2500}\u{2500} a1x \u{2192} a1x.md\n\
        \u{2502}   \u{2514}\u{2500}\u{2500} a2 \u{2192} a2.md\n\
        \u{2502}       \u{251c}\u{2500}\u{2500} a2x \u{2192} a2x.md\n\
        \u{2502}       \u{2514}\u{2500}\u{2500} a2y \u{2192} a2y.md\n\
        \u{251c}\u{2500}\u{2500} b \u{2192} b.md\n\
        \u{2514}\u{2500}\u{2500} c \u{2192} c.md\n\
        \x20   \u{2514}\u{2500}\u{2500} c1 \u{2192} c1.md\n\
        \x20       \u{251c}\u{2500}\u{2500} c1x \u{2192} c1x.md\n\
        \x20       \u{2514}\u{2500}\u{2500} c1y \u{2192} c1y.md\n";
    assert_eq!(stdout(&v.cli(&["bootstrap", "--list"])), tree);
    // The tree is the walk: the lines after the root are the notes in load order.
    let b = v.walk();
    assert_eq!(b.notes.len(), 12);
    let drawn: Vec<&str> = tree
        .lines()
        .skip(1)
        .map(|l| l.rsplit(" \u{2192} ").next().unwrap_or(l))
        .collect();
    let files: Vec<&str> = b.notes[1..]
        .iter()
        .map(|n| n.path.rsplit('/').next().unwrap_or(&n.path))
        .collect();
    assert_eq!(drawn, files);
}

/// A tree with only the entry point is its path on one line, as stored: `Bootstrap/Index.md` stays so.
#[test]
fn list_of_a_lone_index_is_its_path() {
    let v = TestVault::new(&[("bootstrap/index.md", "# Index\n\nS.\n")]);
    assert_eq!(stdout(&v.cli(&["bootstrap", "--list"])), "bootstrap/index.md\n");
    let v = TestVault::new(&[("Bootstrap/Index.md", "# Index\n\nS.\n")]);
    assert_eq!(stdout(&v.cli(&["bootstrap", "--list"])), "Bootstrap/Index.md\n");
}

/// A node shows the link as written, whatever its spelling or form, and the file name from the stored path: a title
/// link gets the file that carries the title, a path link only its last segment.
#[test]
fn list_shows_the_link_as_written_and_the_file_name() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Index\n\nS.\n\n## Relations\n- has_part [[Tools & sourcing]]\n- has_part [[deep/Plain]]\n\
             - has_part [[SHOUT]]\n- indexes [[tools]]\n",
        ),
        ("bootstrap/tools.md", "# Tools & sourcing\n\nS.\n"),
        ("bootstrap/deep/Plain.md", "# Plain\n\nS.\n"),
        ("bootstrap/Shout.md", "# Shout\n\nS.\n"),
    ]);
    assert_eq!(
        stdout(&v.cli(&["bootstrap", "--list"])),
        "bootstrap/index.md\n\
         \u{251c}\u{2500}\u{2500} Tools & sourcing \u{2192} tools.md\n\
         \u{251c}\u{2500}\u{2500} deep/Plain \u{2192} Plain.md\n\
         \u{251c}\u{2500}\u{2500} SHOUT \u{2192} Shout.md\n\
         \u{2514}\u{2500}\u{2500} tools \u{2192} tools.md  (already loaded)\n"
    );
}

/// A ghost shows where its link is, as written, with no arrow and no file, and a ghost linked twice shows twice (the
/// payload lists it once). An ambiguous one says `(ambiguous)`.
#[test]
fn list_shows_ghosts_in_place() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Index\n\nS.\n\n## Relations\n- has_part [[Mentor]]\n- has_part [[Hub]]\n- has_part [[Tone]]\n",
        ),
        (
            "bootstrap/Hub.md",
            "# Hub\n\nS.\n\n## Relations\n- has_part [[Tone]]\n- has_part [[Mentor]]\n",
        ),
        ("bootstrap/Tone.md", &note("Tone", "One.\n")),
        ("bootstrap/behavior/Tone.md", &note("Tone", "Two.\n")),
    ]);
    let out = stdout(&v.cli(&["bootstrap", "--list"]));
    assert_eq!(
        out,
        "bootstrap/index.md\n\
         \u{251c}\u{254c}\u{254c} Mentor  (not written yet)\n\
         \u{251c}\u{2500}\u{2500} Hub \u{2192} Hub.md\n\
         \u{2502}   \u{251c}\u{254c}\u{254c} Tone  (ambiguous)\n\
         \u{2502}   \u{2514}\u{254c}\u{254c} Mentor  (not written yet)\n\
         \u{2514}\u{254c}\u{254c} Tone  (ambiguous)\n"
    );
    let b = v.walk();
    assert!(
        b.payload()
            .ends_with("\nnot loaded: Mentor (not written yet), Tone (ambiguous)\n")
    );
}

/// A link back to an ancestor (a cycle) and a self-link are shown as already loaded and not expanded.
#[test]
fn list_marks_cycles_as_already_loaded() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Index\n\nS.\n\n## Relations\n- has_part [[A]]\n",
        ),
        (
            "bootstrap/A.md",
            "# A\n\nS.\n\n## Relations\n- has_part [[B]]\n- has_part [[index]]\n",
        ),
        (
            "bootstrap/B.md",
            "# B\n\nS.\n\n## Relations\n- has_part [[A]]\n- has_part [[B]]\n",
        ),
    ]);
    assert_eq!(
        stdout(&v.cli(&["bootstrap", "--list"])),
        "bootstrap/index.md\n\
         \u{2514}\u{2500}\u{2500} A \u{2192} A.md\n\
         \x20   \u{251c}\u{2500}\u{2500} B \u{2192} B.md\n\
         \x20   \u{2502}   \u{251c}\u{2500}\u{2500} A \u{2192} A.md  (already loaded)\n\
         \x20   \u{2502}   \u{2514}\u{2500}\u{2500} B \u{2192} B.md  (already loaded)\n\
         \x20   \u{2514}\u{2500}\u{2500} index \u{2192} index.md  (already loaded)\n"
    );
}

/// Links the walk does not follow do not show in the tree: other relation words, plain links, links that leave the
/// bootstrap.
#[test]
fn list_leaves_out_links_that_are_not_followed() {
    let v = TestVault::new(&[
        (
            "bootstrap/index.md",
            "# Index\n\nS.\n\n## Relations\n- relates_to [[Side]]\n- relates_to [[Missing]]\n- [[Plain]]\n\
             - has_part [[knowledge::Deploy]]\n- has_part [[Leaf]]\n",
        ),
        ("bootstrap/Side.md", "# Side\n\nS.\n"),
        ("bootstrap/Plain.md", "# Plain\n\nS.\n"),
        ("bootstrap/Leaf.md", "# Leaf\n\nS.\n"),
        ("knowledge/Deploy.md", "# Deploy\n\nS.\n"),
    ]);
    assert_eq!(
        stdout(&v.cli(&["bootstrap", "--list"])),
        "bootstrap/index.md\n\u{2514}\u{2500}\u{2500} Leaf \u{2192} Leaf.md\n"
    );
}

/// The fixture vault's bootstrap as a tree: index, then the Profile hub, then Identity under it.
#[test]
fn list_of_the_fixture_vault() {
    let v = TestVault::new(&[]);
    copy_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault/bootstrap"),
        &v.root.join("bootstrap"),
    );
    assert_eq!(
        stdout(&v.cli(&["bootstrap", "--list"])),
        "bootstrap/index.md\n\
         \u{2514}\u{2500}\u{2500} Profile \u{2192} Profile.md\n\
         \x20   \u{2514}\u{2500}\u{2500} Identity \u{2192} Identity.md\n"
    );
}

/// The CLI, end to end: exit 0, the tree on stdout, nothing on stderr, no budget warning even when the payload is
/// over budget.
#[test]
fn list_through_the_cli_carries_no_budget_warning() {
    let v = layout_vault();
    config::set(&v.config, "bootstrap.max_tokens", "1").unwrap();
    let out = v.cli(&["bootstrap", "--list"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(
        stdout(&out),
        "bootstrap/index.md\n\
         \u{251c}\u{2500}\u{2500} Profile \u{2192} Profile.md\n\
         \u{251c}\u{2500}\u{2500} Style \u{2192} Style.md\n\
         \u{2502}   \u{2514}\u{2500}\u{2500} Mentor \u{2192} Mentor.md\n\
         \u{2514}\u{2500}\u{2500} Global rules \u{2192} Global rules.md\n"
    );
    // The payload itself does warn at that budget.
    assert!(stdout(&v.cli(&["bootstrap"])).contains("warning:"));
}

// ---------------------------------------------------------------------------------------------------------------
// Budget (doc 04 §2.2 "Budget"); the config key `bootstrap.max_tokens` (doc 05 §5)
// ---------------------------------------------------------------------------------------------------------------

fn vault_of_size(chars: usize) -> TestVault {
    let filler = "word ".repeat(chars / 5);
    TestVault::new(&[("bootstrap/index.md", &format!("# Big\n\n{filler}\n"))])
}

/// The size is chars ÷ 4 of the payload (rounded up); above the budget the result carries a warning, at or below it
/// does not.
#[test]
fn a_payload_above_the_budget_carries_a_warning() {
    let v = vault_of_size(4000);
    let b = v.walk();
    let tokens = b.tokens();
    assert_eq!(tokens, b.payload().chars().count().div_ceil(4));
    assert!(tokens > 1000 && tokens < 1100, "{tokens}");
    // At the budget: no warning, and the result is the payload.
    assert_eq!(b.render(tokens), b.payload());
    // Above it: the payload, a blank line, and the warning as the last line.
    let warned = b.render(tokens - 1);
    assert!(warned.starts_with(&b.payload()));
    assert_eq!(
        warned[b.payload().len()..],
        format!(
            "\nwarning: the bootstrap is {tokens} tokens (estimate), above bootstrap.max_tokens ({})\n",
            tokens - 1
        )
    );
}

/// The warning never counts itself, and the ghost line counts as payload.
#[test]
fn the_ghost_line_counts_toward_the_size() {
    let v = TestVault::new(&[(
        "bootstrap/index.md",
        &note("B", "## Relations\n- has_part [[Missing]]\n"),
    )]);
    let b = v.walk();
    assert!(b.payload().contains("not loaded"));
    assert_eq!(b.tokens(), b.payload().chars().count().div_ceil(4));
}

/// Characters, not bytes: a note in another language is not charged for its encoding.
#[test]
fn tokens_count_characters_not_bytes() {
    let v = TestVault::new(&[("bootstrap/index.md", &format!("# T\n\n{}\n", "ç".repeat(400)))]);
    let b = v.walk();
    let payload = b.payload();
    assert!(payload.len() > payload.chars().count());
    assert_eq!(b.tokens(), payload.chars().count().div_ceil(4));
}

/// The CLI reads `bootstrap.max_tokens` from the config file; without it the default applies.
#[test]
fn the_cli_reads_the_budget_from_the_config_file() {
    let v = vault_of_size(4000);
    let out = v.cli(&["bootstrap"]);
    assert!(out.status.success());
    assert!(
        !stdout(&out).contains("warning:"),
        "default {DEFAULT_MAX_TOKENS} is above this payload"
    );

    config::set(&v.config, "bootstrap.max_tokens", "500").unwrap();
    let out = v.cli(&["bootstrap"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("above bootstrap.max_tokens (500)"), "{text}");
    assert_eq!(text.lines().filter(|l| l.starts_with("warning:")).count(), 1);

    // `--list` is not a payload: no warning.
    assert!(!stdout(&v.cli(&["bootstrap", "--list"])).contains("warning"));
}

/// `bootstrap.max_tokens` is a config key: a whole number of at least 1, stored as a TOML integer.
#[test]
fn the_budget_is_a_config_key() {
    let v = TestVault::new(&[]);
    assert_eq!(config::bootstrap_max_tokens(&v.config).unwrap(), None);
    config::set(&v.config, "bootstrap.max_tokens", "3000").unwrap();
    assert_eq!(
        config::get(&v.config, "bootstrap.max_tokens").unwrap().as_deref(),
        Some("3000")
    );
    assert_eq!(config::bootstrap_max_tokens(&v.config).unwrap(), Some(3000));
    assert!(fs::read_to_string(&v.config).unwrap().contains("max_tokens = 3000"));
    for bad in ["0", "-5", "abc", "1.5", ""] {
        let err = config::set(&v.config, "bootstrap.max_tokens", bad).unwrap_err();
        assert!(matches!(err, Error::InvalidConfigValue { .. }), "{bad}");
        assert!(err.is_usage());
    }
    // A hand-edited value of the wrong kind is an error, not a silent default.
    fs::write(&v.config, "[bootstrap]\nmax_tokens = \"3000\"\n").unwrap();
    assert!(matches!(
        config::bootstrap_max_tokens(&v.config),
        Err(Error::Config { .. })
    ));
    fs::write(&v.config, "[bootstrap]\nmax_tokens = 0\n").unwrap();
    assert!(matches!(
        config::bootstrap_max_tokens(&v.config),
        Err(Error::Config { .. })
    ));
}

// ---------------------------------------------------------------------------------------------------------------
// The CLI (doc 07 §12)
// ---------------------------------------------------------------------------------------------------------------

/// `recognitor bootstrap` prints the payload on stdout and exits 0.
#[test]
fn the_cli_prints_the_payload() {
    let v = layout_vault();
    let out = v.cli(&["bootstrap"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    assert_eq!(stdout(&out), v.walk().payload());
    assert!(stdout(&out).starts_with("# Bootstrap\n\nSummary of Bootstrap.\n"));
}

/// The CLI and the library return the same text, and the CLI sees edits after the gate: here a first run builds the
/// database, a changed file shows at the next run once the 2-second gate has passed.
#[test]
fn the_cli_sweeps_before_it_loads() {
    let v = layout_vault();
    assert!(stdout(&v.cli(&["bootstrap"])).contains("Patient."));
    v.write("bootstrap/behavior/personas/Mentor.md", &note("Mentor", "Firm.\n"));
    // Expire the gate in the scratch database, as the bench does.
    let conn = rusqlite_open(&v);
    db::meta_set(&conn, meta_keys::LAST_SWEEP_FINISHED_MS, "0").unwrap();
    drop(conn);
    let text = stdout(&v.cli(&["bootstrap"]));
    assert!(text.contains("Firm.") && !text.contains("Patient."), "{text}");
}

fn rusqlite_open(v: &TestVault) -> rusqlite::Connection {
    db::open(&v.db).unwrap()
}

/// The vault comes from `--vault`, else `RECOGNITOR_VAULT`, else `vault.path` (doc 07 §12): with none, a usage error.
#[test]
fn the_vault_is_resolved_like_the_other_commands() {
    let v = layout_vault();
    let bin = |envs: &[(&str, &OsString)]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_recognitor"));
        cmd.arg("bootstrap")
            .env("RECOGNITOR_DB", &v.db)
            .env("RECOGNITOR_CONFIG", &v.config)
            .env_remove("RECOGNITOR_VAULT");
        for (k, val) in envs {
            cmd.env(k, val);
        }
        cmd.output().unwrap()
    };
    let none = bin(&[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr(&none).contains("--vault") && stderr(&none).contains("RECOGNITOR_VAULT"));
    let via_env = bin(&[("RECOGNITOR_VAULT", &v.root.clone().into_os_string())]);
    assert!(via_env.status.success(), "{}", stderr(&via_env));
    assert!(stdout(&via_env).starts_with("# Bootstrap"));
    config::set(&v.config, "vault.path", v.root.to_str().unwrap()).unwrap();
    let via_config = bin(&[]);
    assert!(via_config.status.success(), "{}", stderr(&via_config));
}

/// `--list` belongs to `bootstrap`; extra arguments are usage errors.
#[test]
fn the_cli_rejects_misplaced_arguments() {
    let v = layout_vault();
    for args in [
        &["sync", "--list"][..],
        &["search", "x", "--list"][..],
        &["bootstrap", "extra"][..],
    ] {
        let out = v.cli(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(stdout(&out), "");
    }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The fixture vault's bootstrap is a small tree with a hub in between: index -> Profile -> Identity.
#[test]
fn the_fixture_bootstrap_loads_the_index_the_hub_and_the_note() {
    let v = TestVault::new(&[]);
    copy_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vault/bootstrap"),
        &v.root.join("bootstrap"),
    );
    let b = v.walk();
    assert_eq!(
        ids(&b),
        [
            "bootstrap/index",
            "bootstrap/profile/Profile",
            "bootstrap/profile/Identity"
        ]
    );
    assert!(b.not_loaded.is_empty());
}

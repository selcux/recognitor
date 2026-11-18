//! `relink` tests (doc 07 §6.1, §5). Each vault is in its state after a move made outside Recognitor (§6.2): the note
//! sits at its new path and the linking notes still name the old one. Vaults live in temp folders that delete
//! themselves.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use recognitor::relink::{Relink, render};
use recognitor::{Error, Index};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TestVault {
    root: PathBuf,
    db: PathBuf,
}

impl TestVault {
    fn new(files: &[(&str, &str)]) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let base = std::env::temp_dir().join(format!("recognitor-relink-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("vault");
        fs::create_dir_all(&root).unwrap();
        let v = TestVault {
            root,
            db: base.join("index.db"),
        };
        for (rel, text) in files {
            let p = v.root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        v
    }

    fn file(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// An index on a fresh database: `relink` runs the sweep itself.
    fn index(&self) -> Index {
        Index::open(&self.root, Some(&self.db)).unwrap()
    }

    fn relink(&self, old: &str, new: &str, notes: &[&str]) -> String {
        let notes: Vec<String> = notes.iter().map(|n| n.to_string()).collect();
        render(&self.index().relink(old, new, &notes).unwrap())
    }
}

impl Drop for TestVault {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

const CACHING: &str = "# Caching\n\nA cache keeps answers close.\n";

#[test]
fn prefixed_links_become_names_and_keep_alias_heading_and_embed_and_code_and_live_names_stay() {
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        (
            "ops/Runbook.md",
            "# Runbook\n\nSee [[ideas::Caching#Keys|the cache]] and ![[ideas::Caching]]. Name [[Caching]] stays.\n\n\
             Code: `[[ideas::Caching]]`.\n\n## Relations\n- depends_on [[ideas::Caching]] (hot path)\n",
        ),
    ]);
    // The same note twice is handled once.
    let out = v.relink("ideas/Caching", "design/Caching", &["ops/Runbook", "Runbook"]);
    assert_eq!(out, "relinked: 3 links in 1 note\n");
    assert_eq!(
        v.file("ops/Runbook.md"),
        "# Runbook\n\nSee [[design::Caching#Keys|the cache]] and ![[design::Caching]]. Name [[Caching]] stays.\n\n\
         Code: `[[ideas::Caching]]`.\n\n## Relations\n- depends_on [[design::Caching]] (hot path)\n"
    );
    // A second run finds nothing left to rewrite.
    let again = v.relink("ideas/Caching", "design/Caching", &["ops/Runbook"]);
    assert_eq!(
        again,
        "relinked: 0 links in 0 notes\nwarning: skipped, link already gone: ops/Runbook\n"
    );
}

#[test]
fn a_prefixed_link_is_rewritten_and_a_name_that_points_elsewhere_is_not() {
    // Doc 07 §6.1: both links have the key `caching`; only the second pointed to the moved note.
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("ops/Caching.md", "# Caching\n\nThe ops cache.\n"),
        (
            "ops/Runbook.md",
            "# Runbook\n\nLocal [[Caching]], research [[ideas::Caching]].\n",
        ),
    ]);
    assert_eq!(
        v.relink("ideas/Caching", "design/Caching", &["ops/Runbook"]),
        "relinked: 1 link in 1 note\n"
    );
    assert_eq!(
        v.file("ops/Runbook.md"),
        "# Runbook\n\nLocal [[Caching]], research [[design::Caching]].\n"
    );
}

#[test]
fn the_prefix_goes_when_the_linking_note_is_in_the_new_namespace() {
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("design/Plan.md", "# Plan\n\nUses [[ideas::Caching]].\n"),
    ]);
    assert_eq!(
        v.relink("ideas/Caching", "design/Caching", &["design/Plan"]),
        "relinked: 1 link in 1 note\n"
    );
    assert_eq!(v.file("design/Plan.md"), "# Plan\n\nUses [[Caching]].\n");
}

#[test]
fn a_name_that_became_ambiguous_gets_the_prefix() {
    // `[[Caching]]` reached ops/Caching by step 3; after the move it matches two notes.
    let v = TestVault::new(&[
        ("design/Caching.md", "# Caching\n\nDesign cache.\n"),
        ("research/Caching.md", CACHING),
        ("ops/Runbook.md", "# Runbook\n\nSee [[Caching]].\n"),
    ]);
    assert_eq!(
        v.relink("ops/Caching", "research/Caching", &["ops/Runbook"]),
        "relinked: 1 link in 1 note\n"
    );
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[research::Caching]].\n");
}

#[test]
fn a_name_that_repeats_in_the_namespace_becomes_the_path_from_the_namespace_folder() {
    let v = TestVault::new(&[
        ("design/a/Caching.md", "# Caching\n\nAnother cache.\n"),
        ("design/b/Caching.md", CACHING),
        ("ops/Runbook.md", "# Runbook\n\nSee [[ideas::Caching]].\n"),
    ]);
    assert_eq!(
        v.relink("ideas/Caching", "design/b/Caching", &["ops/Runbook"]),
        "relinked: 1 link in 1 note\n"
    );
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[design::b/Caching]].\n");
}

/// Doc 07 §6.1 table: `ideas/old/Caching` moves to `design/b/Caching`. The old link is written four ways, and the new
/// link does not depend on how: a name (which still reaches the note on its own, so it is left alone unless it
/// became ambiguous), a prefixed name, a prefixed path and the same with `.md` and a heading.
fn table_vault(linking: (&str, &str), also_a: bool) -> TestVault {
    let mut files = vec![("design/b/Caching.md", CACHING), linking];
    if also_a {
        files.push(("design/a/Caching.md", "# Caching\n\nAnother cache.\n"));
    }
    TestVault::new(&files)
}

const OLD_FORMS: &str = "# Linker\n\nName [[Caching|n]], prefixed [[ideas::Caching|p]], path [[ideas::old/Caching|q]], \
                         md [[ideas::old/Caching.md#Keys]].\n";

fn table_relink(v: &TestVault, linker: &str) -> String {
    v.relink("ideas/old/Caching", "design/b/Caching", &[linker])
}

#[test]
fn table_row_1_outside_the_namespace_and_the_name_is_unique() {
    let v = table_vault(("ops/Linker.md", OLD_FORMS), false);
    assert_eq!(table_relink(&v, "ops/Linker"), "relinked: 3 links in 1 note\n");
    // `[[Caching]]` still reaches the note by step 4, so it is not a broken link and stays.
    assert_eq!(
        v.file("ops/Linker.md"),
        "# Linker\n\nName [[Caching|n]], prefixed [[design::Caching|p]], path [[design::Caching|q]], \
         md [[design::Caching.md#Keys]].\n"
    );
}

#[test]
fn table_row_2_inside_the_namespace_and_the_name_is_unique() {
    let v = table_vault(("design/Linker.md", OLD_FORMS), false);
    assert_eq!(table_relink(&v, "design/Linker"), "relinked: 3 links in 1 note\n");
    assert_eq!(
        v.file("design/Linker.md"),
        "# Linker\n\nName [[Caching|n]], prefixed [[Caching|p]], path [[Caching|q]], md [[Caching.md#Keys]].\n"
    );
}

#[test]
fn table_row_3_outside_the_namespace_and_the_name_repeats() {
    // `design/a/Caching` exists, so `[[Caching]]` was not a link to the note and stays what it was.
    let v = table_vault(("ops/Linker.md", OLD_FORMS), true);
    assert_eq!(table_relink(&v, "ops/Linker"), "relinked: 3 links in 1 note\n");
    assert_eq!(
        v.file("ops/Linker.md"),
        "# Linker\n\nName [[Caching|n]], prefixed [[design::b/Caching|p]], path [[design::b/Caching|q]], \
         md [[design::b/Caching.md#Keys]].\n"
    );
}

#[test]
fn table_row_4_inside_the_namespace_and_the_name_repeats() {
    let v = table_vault(("design/Linker.md", OLD_FORMS), true);
    assert_eq!(table_relink(&v, "design/Linker"), "relinked: 3 links in 1 note\n");
    assert_eq!(
        v.file("design/Linker.md"),
        "# Linker\n\nName [[Caching|n]], prefixed [[b/Caching|p]], path [[b/Caching|q]], md [[b/Caching.md#Keys]].\n"
    );
}

#[test]
fn a_path_inside_the_old_namespace_is_read_from_its_folder_and_never_from_the_vault_root() {
    // In `ideas`, `[[old/Caching]]` is `ideas/old/Caching`; `[[ideas/old/Caching]]` would be `ideas/ideas/old/Caching`
    // and was never a link to the note, so it is left alone.
    let v = table_vault(
        (
            "ideas/Linker.md",
            "# Linker\n\nSee [[old/Caching]] and [[ideas/old/Caching]].\n",
        ),
        false,
    );
    assert_eq!(
        v.relink("ideas/old/Caching", "design/b/Caching", &["ideas/Linker"]),
        "relinked: 1 link in 1 note\n"
    );
    assert_eq!(
        v.file("ideas/Linker.md"),
        "# Linker\n\nSee [[design::Caching]] and [[ideas/old/Caching]].\n"
    );
}

#[test]
fn a_note_directly_in_its_namespace_with_a_repeated_name_has_no_form_and_is_not_rewritten() {
    // Doc 07 §6.1, "No form, no rewrite": `[[Caching]]` is ambiguous in `design`, and a path from the namespace folder
    // needs a folder. The link stays as written and is warned about; the note is not written, and there is no
    // "link already gone" warning for it.
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("design/a/Caching.md", "# Caching\n\nAnother cache.\n"),
        ("ops/Runbook.md", "# Runbook\n\nSee [[ideas::Caching]].\n"),
    ]);
    let out = v.relink("ideas/Caching", "design/Caching", &["ops/Runbook"]);
    assert_eq!(
        out,
        "relinked: 0 links in 0 notes\nwarning: skipped, no unambiguous link to design/Caching: ops/Runbook\n"
    );
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[ideas::Caching]].\n");
}

#[test]
fn a_no_form_link_stays_while_the_other_links_of_the_note_are_rewritten() {
    // `[[ideas::Caching]]` has no form for `design/Caching`; the Markdown link and the relative link do, and so does
    // the link in the second note. Two no-form links in one note give one warning.
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("design/a/Caching.md", "# Caching\n\nAnother cache.\n"),
        (
            "ops/Runbook.md",
            "# Runbook\n\nSee [[ideas::Caching]], [[ideas::Caching|again]], [[../ideas/Caching]] and \
             [the cache](../ideas/Caching.md).\n",
        ),
    ]);
    let out = v.relink("ideas/Caching", "design/Caching", &["ops/Runbook"]);
    assert_eq!(
        out,
        "relinked: 2 links in 1 note\nwarning: skipped, no unambiguous link to design/Caching: ops/Runbook\n"
    );
    assert_eq!(
        v.file("ops/Runbook.md"),
        "# Runbook\n\nSee [[ideas::Caching]], [[ideas::Caching|again]], [[../design/Caching]] and \
         [the cache](../design/Caching.md).\n"
    );
}

#[test]
fn moved_and_renamed_into_a_folder_where_the_filename_repeats() {
    // `cognition/index` moves to `recognitor/sub/index`: `index` now repeats in `recognitor`.
    let v = TestVault::new(&[
        ("recognitor/index.md", "# Recognitor\n\nSee [[cognition::index]].\n"),
        ("recognitor/sub/index.md", "# Cognition\n\nThe hub.\n"),
        (
            "cognition/hubs/Storage.md",
            "# Storage\n\nPart of [[Index]] and [[recognitor::index]].\n",
        ),
    ]);
    assert_eq!(
        v.relink(
            "cognition/index",
            "recognitor/sub/index",
            &["cognition/hubs/Storage", "recognitor/index"]
        ),
        "relinked: 2 links in 2 notes\n"
    );
    assert_eq!(v.file("recognitor/index.md"), "# Recognitor\n\nSee [[sub/index]].\n");
    // `[[recognitor::index]]` points to another note and stays.
    assert_eq!(
        v.file("cognition/hubs/Storage.md"),
        "# Storage\n\nPart of [[recognitor::sub/index]] and [[recognitor::index]].\n"
    );
}

#[test]
fn the_name_follows_the_file_when_it_is_renamed_in_the_move() {
    // Doc 07 §6.1, "Moved and renamed": `topics/index` moves to `design/topics-index`, title "Topics". The other
    // namespace has an `index` of its own, so the old filename no longer names the moved note.
    let v = TestVault::new(&[
        (
            "design/index.md",
            "# Design

The design hub.

## Relations
- relates_to [[topics::index]]
- relates_to [[topics::Topics]]
",
        ),
        (
            "design/topics-index.md",
            "# Topics

The topics hub.
",
        ),
        (
            "topics/hubs/Storage.md",
            "# Storage

## Relations
- part_of [[Index]]
",
        ),
        (
            "topics/hubs/Graphs.md",
            "# Graphs

See [[index|the hub]] and [[Topics]].
",
        ),
    ]);
    let out = v.relink(
        "topics/index",
        "design/topics-index",
        &["topics/hubs/Storage", "topics/hubs/Graphs", "design/index"],
    );
    assert_eq!(
        out,
        "relinked: 4 links in 3 notes
"
    );
    // The old filename is not a name of the note any more: the new filename, prefixed from another namespace.
    assert_eq!(
        v.file("topics/hubs/Storage.md"),
        "# Storage

## Relations
- part_of [[design::topics-index]]
"
    );
    // An alias stays. `[[Topics]]` still reaches the note by its title, so it is not touched.
    assert_eq!(
        v.file("topics/hubs/Graphs.md"),
        "# Graphs

See [[design::topics-index|the hub]] and [[Topics]].
"
    );
    // Inside the new namespace the prefix goes and no path form is written; a title still names the note and stays.
    assert_eq!(
        v.file("design/index.md"),
        "# Design

The design hub.

## Relations
- relates_to [[topics-index]]
- relates_to [[Topics]]
"
    );
}

#[test]
fn a_title_link_survives_a_rename_and_keeps_the_title() {
    let v = TestVault::new(&[
        ("design/Cache Layer.md", CACHING),
        (
            "ops/Runbook.md",
            "# Runbook\n\nSee [[Caching]] and [[ideas::caching]].\n",
        ),
    ]);
    assert_eq!(
        v.relink("ideas/Caching", "design/Cache Layer", &["ops/Runbook"]),
        "relinked: 1 link in 1 note\n"
    );
    // `[[Caching]]` was not a link to the moved note (it resolved to nothing before); `[[ideas::caching]]` was.
    assert_eq!(
        v.file("ops/Runbook.md"),
        "# Runbook\n\nSee [[Caching]] and [[design::caching]].\n"
    );
}

#[test]
fn relative_and_markdown_links_follow_a_move_with_a_rename() {
    // The file was renamed in the move; the title stays, so title links still find it.
    let v = TestVault::new(&[
        ("design/Cache Layer.md", CACHING),
        (
            "ops/Runbook.md",
            "# Runbook\n\nSee [[../ideas/Caching]], [the cache](../ideas/Caching.md#keys \"Cache\"), \
             [[ideas::Caching]] and [[ideas::Caching.md]].\n",
        ),
    ]);
    assert_eq!(
        v.relink("ideas/Caching", "design/Cache Layer", &["ops/Runbook"]),
        "relinked: 4 links in 1 note\n"
    );
    assert_eq!(
        v.file("ops/Runbook.md"),
        "# Runbook\n\nSee [[../design/Cache Layer]], [the cache](../design/Cache%20Layer.md#keys \"Cache\"), \
         [[design::Caching]] and [[design::Caching.md]].\n"
    );
}

#[test]
fn notes_it_cannot_handle_become_warnings_and_the_rest_still_run() {
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("ops/Runbook.md", "# Runbook\n\nNothing here.\n"),
        ("ops/Deploy.md", "# Deploy\n\nWarm [[ideas::Caching]] first.\n"),
        ("a/index.md", "# A\n\nEntry.\n"),
        ("b/index.md", "# B\n\nEntry.\n"),
        ("archive/2026 s.md", "# Session\n\nTalked about [[ideas::Caching]].\n"),
    ]);
    let out = v.relink(
        "ideas/Caching",
        "design/Caching",
        &["ops/Runbook", "Nope", "index", "archive/2026 s", "ops/Deploy"],
    );
    assert_eq!(
        out,
        "relinked: 1 link in 1 note\n\
         warning: skipped, link already gone: ops/Runbook\n\
         warning: skipped, \"Nope\" not found\n\
         warning: skipped, \"index\" matches 2 notes: a/index, b/index\n\
         warning: skipped, archive notes have no links: archive/2026 s\n"
    );
    assert_eq!(v.file("ops/Deploy.md"), "# Deploy\n\nWarm [[design::Caching]] first.\n");
    assert_eq!(
        v.file("archive/2026 s.md"),
        "# Session\n\nTalked about [[ideas::Caching]].\n"
    );
}

#[test]
fn new_and_old_must_name_one_note() {
    let v = TestVault::new(&[
        ("design/Caching.md", CACHING),
        ("a/index.md", "# A\n\nEntry.\n"),
        ("b/index.md", "# B\n\nEntry.\n"),
        ("ops/Runbook.md", "# Runbook\n\nSee [[ideas::Caching]].\n"),
    ]);
    let notes = vec!["ops/Runbook".to_string()];
    let several = v.index().relink("ideas/Caching", "index", &notes).unwrap();
    assert!(matches!(several, Relink::Several { .. }));
    assert_eq!(
        render(&several),
        "\"index\" matches 2 notes. Call again with one id:\n  a/index\n  b/index\n"
    );
    let err = v.index().relink("ideas/Caching", "Nope", &notes).unwrap_err();
    assert!(matches!(err, Error::NoteNotFound(ref n) if n == "Nope"), "{err}");
    let err = v.index().relink("Caching", "design/Caching", &notes).unwrap_err();
    assert!(matches!(err, Error::OldNotAnId(_)), "{err}");
    // Nothing was written.
    assert_eq!(v.file("ops/Runbook.md"), "# Runbook\n\nSee [[ideas::Caching]].\n");
}

#[test]
fn a_bootstrap_note_gets_the_notice_its_bytes_stay_and_its_index_rows_follow() {
    let index_md = "\u{feff}# Bootstrap\r\n\r\nStanding rules.\r\n\r\n## Relations\r\n- indexes [[./Me]]\r\n";
    let v = TestVault::new(&[
        ("bootstrap/index.md", index_md),
        ("bootstrap/people/Me.md", "# Me\r\n\r\nWho I am.\r\n"),
    ]);
    let mut index = v.index();
    let out = render(
        &index
            .relink("bootstrap/Me", "bootstrap/people/Me", &["bootstrap/index".to_string()])
            .unwrap(),
    );
    assert_eq!(
        out,
        "relinked: 1 link in 1 note\nnotice: bootstrap/index loads as instructions at session start\n"
    );
    assert_eq!(
        v.file("bootstrap/index.md"),
        "\u{feff}# Bootstrap\r\n\r\nStanding rules.\r\n\r\n## Relations\r\n- indexes [[./people/Me]]\r\n"
    );
    let target: Option<String> = index
        .connection()
        .query_row(
            "SELECT target_key FROM edges WHERE source_key = 'bootstrap/index' AND predicate = 'indexes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(target.as_deref(), Some("bootstrap/people/me"));
    // The bootstrap walk reaches the note again.
    assert!(index.bootstrap().unwrap().render(10_000).contains("Who I am."));
    // No temporary file is left behind.
    let leftovers: Vec<String> = fs::read_dir(v.root.join("bootstrap"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("recognitor-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

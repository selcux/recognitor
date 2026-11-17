//! `load_bootstrap()`: the bootstrap link tree as one Markdown payload (doc 04 §2.2, doc 05 §3.1).
//!
//! The walk runs here, in Rust, with one visited set. Each step asks the index for one note's outgoing
//! `## Relations` links in written order. Notes are read from `notes.content`; no vault file is opened.
//!

use std::collections::HashMap;
use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::parse::{LinkSyntax, frontmatter_block, is_relations_heading, markdown_options};
use crate::paths::{BOOTSTRAP_NAMESPACE, note_id};
use crate::resolve::{Resolution, link_key, resolve};

/// Key of the entry point (doc 05 §3.2: `<namespace>/index`).
pub const ENTRY_KEY: &str = "bootstrap/index";

/// Default of `bootstrap.max_tokens` (doc 04 §2.2, "Budget").
pub const DEFAULT_MAX_TOKENS: usize = 4000;

/// One note of the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedNote {
    /// The id as written (doc 03 §4).
    pub id: String,
    /// The path as stored, relative to the vault root, with its extension and spelling: its last segment is the file
    /// name that `--list` shows.
    pub path: String,
    /// The note's title, for the heading.
    pub title: String,
    /// The body: frontmatter and `## Relations` stripped, trimmed.
    pub body: String,
}

/// What the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bootstrap {
    /// In load order: depth-first, links in the order written.
    pub notes: Vec<LoadedNote>,
    /// Ghost links in the tree, as written, in the order the walk met them, each once.
    pub not_loaded: Vec<String>,
    /// The ghosts among [`Bootstrap::not_loaded`] that more than one note matches (doc 04 §2.2, "Ghost links in the
    /// tree"). They are worked out from the index at walk time, with the resolution rules of doc 03 §4.
    pub ambiguous: Vec<String>,
    /// What each step of the walk met, in the order it met it, for the `--list` tree (doc 04 §2.2, "CLI twin").
    pub tree: Vec<TreeEntry>,
}

/// One step of the walk: a note it loaded, a link to a note that was already loaded, or a ghost link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// 0 for the entry point; a link's entry is one deeper than the note it was written in.
    pub depth: usize,
    pub what: TreeNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNode {
    /// The entry point, `bootstrap/index.md`: note 0 of [`Bootstrap::notes`].
    Entry,
    /// The note at index `note` of [`Bootstrap::notes`], loaded here by the link `link`, as written.
    Loaded { note: usize, link: String },
    /// A link, as written, to the note at index `note`, which loaded earlier: not expanded again. A link back to an
    /// ancestor is the same case.
    AlreadyLoaded { note: usize, link: String },
    /// A link to a note that is not there, as written.
    Ghost { target: String, ambiguous: bool },
}

/// Walks the bootstrap from `bootstrap/index.md` (doc 04 §2.2).
///
/// Fails with [`Error::NoBootstrap`] when the index has no entry point: no `bootstrap/` folder, no `index.md`, or a
/// case twin that was left out. Nothing else of the bootstrap is read then.
pub fn load(conn: &Connection) -> Result<Bootstrap> {
    // Loaded notes by key, to their index in `notes`.
    let mut visited: HashMap<String, usize> = HashMap::new();
    let mut notes = Vec::new();
    let mut not_loaded: Vec<String> = Vec::new();
    let mut ambiguous: Vec<String> = Vec::new();
    let mut tree: Vec<TreeEntry> = Vec::new();
    // Whether a ghost target is ambiguous, by target as written: the same link text resolves the same way from every
    // note on the walk, because they are all in the bootstrap namespace.
    let mut ghosts: HashMap<String, bool> = HashMap::new();

    let Some(entry) = read_note(conn, ENTRY_KEY)? else {
        return Err(Error::NoBootstrap);
    };
    visited.insert(ENTRY_KEY.to_string(), 0);
    notes.push(entry);
    tree.push(TreeEntry {
        depth: 0,
        what: TreeNode::Entry,
    });

    // Depth-first without recursion: one iterator of links per note on the path from the entry to the current note,
    // with that note's key. The stack's length is the depth of the links being read.
    let mut stack = vec![(ENTRY_KEY.to_string(), links_out(conn, ENTRY_KEY)?.into_iter())];
    loop {
        let depth = stack.len();
        let Some((source_key, links)) = stack.last_mut() else {
            break;
        };
        let Some(link) = links.next() else {
            stack.pop();
            continue;
        };
        let Some(key) = link.target_key else {
            let is_ambiguous = match ghosts.get(&link.target_ref) {
                Some(known) => *known,
                None => {
                    // Relation links are wikilinks, and every note on the walk is in the bootstrap namespace.
                    let found = link_key(source_key, LinkSyntax::Wiki, &link.target_ref)
                        .map(|ref_key| resolve(conn, BOOTSTRAP_NAMESPACE, &ref_key))
                        .transpose()?
                        .is_some_and(|r| matches!(r, Resolution::Ambiguous(_)));
                    ghosts.insert(link.target_ref.clone(), found);
                    found
                }
            };
            // The payload lists a ghost once, however many notes link to it (doc 04 §2.2); the tree shows each link.
            if !not_loaded.contains(&link.target_ref) {
                if is_ambiguous {
                    ambiguous.push(link.target_ref.clone());
                }
                not_loaded.push(link.target_ref.clone());
            }
            tree.push(TreeEntry {
                depth,
                what: TreeNode::Ghost {
                    target: link.target_ref,
                    ambiguous: is_ambiguous,
                },
            });
            continue;
        };
        if let Some(&at) = visited.get(&key) {
            tree.push(TreeEntry {
                depth,
                what: TreeNode::AlreadyLoaded {
                    note: at,
                    link: link.target_ref,
                },
            });
            continue;
        }
        if let Some(note) = read_note(conn, &key)? {
            visited.insert(key.clone(), notes.len());
            tree.push(TreeEntry {
                depth,
                what: TreeNode::Loaded {
                    note: notes.len(),
                    link: link.target_ref,
                },
            });
            notes.push(note);
            let links = links_out(conn, &key)?.into_iter();
            stack.push((key, links));
        }
    }
    Ok(Bootstrap {
        notes,
        not_loaded,
        ambiguous,
        tree,
    })
}

/// One row of the walk query.
struct Link {
    target_key: Option<String>,
    target_ref: String,
}

/// Doc 05 §3.1, as written: a note's outgoing `## Relations` links in written order, for the two relation words
/// that point down a tree, `indexes` and `has_part`, in any case (doc 04 §2.2 steps 2 and 4). A link with another
/// word, and a link that leaves the bootstrap, are not in the result; a ghost is, with `target_key` unset.
fn links_out(conn: &Connection, source_key: &str) -> Result<Vec<Link>> {
    let mut stmt = conn.prepare_cached(
        "SELECT e.target_key, e.target_ref \
         FROM edges e \
         LEFT JOIN notes n ON n.key = e.target_key \
         WHERE e.source_key = ?1 \
           AND e.is_inline = 0 \
           AND lower(e.predicate) IN ('indexes', 'has_part') \
           AND (n.namespace = 'bootstrap' OR e.target_key IS NULL) \
         ORDER BY e.position",
    )?;
    let rows = stmt.query_map(params![source_key], |r| {
        Ok(Link {
            target_key: r.get(0)?,
            target_ref: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn read_note(conn: &Connection, key: &str) -> Result<Option<LoadedNote>> {
    let row = conn
        .prepare_cached("SELECT path, title, content FROM notes WHERE key = ?1")?
        .query_row(params![key], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .optional()?;
    Ok(row.map(|(path, title, content)| LoadedNote {
        id: note_id(&path).to_string(),
        path,
        title,
        body: body_of(&content),
    }))
}

/// A note's text for the payload (doc 04 §2.2 step 5): the file without its frontmatter and without its
/// `## Relations` sections. Blank lines before the first line and after the last are dropped, so notes join with one
/// blank line. Nothing else changes, line endings included.
///
/// Frontmatter, headings and code blocks are found with the parser the indexer uses, so a `## Relations` line in a
/// code block stays. A relations section runs from its heading line to the next H1 or H2 line, or the end of the
/// file (doc 04 §2.2 step 5).
pub fn body_of(content: &str) -> String {
    let full = content.strip_prefix('\u{feff}').unwrap_or(content);
    // Frontmatter is the metadata block at the very start of the file, and nothing else (doc 03 §2). The rest is
    // parsed on its own, so no offset has to skip it.
    let text = &full[frontmatter_block(full).map_or(0, |(range, _)| range.end)..];
    let line_start = |offset: usize| text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let mut cuts: Vec<Range<usize>> = Vec::new();
    let mut relations_from: Option<usize> = None;
    let mut open_heading: Option<(HeadingLevel, usize, String)> = None;
    for (event, range) in Parser::new_ext(text, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => open_heading = Some((level, range.start, String::new())),
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, heading_text)) = &mut open_heading {
                    heading_text.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                let Some((level, start, heading_text)) = open_heading.take() else {
                    continue;
                };
                if !matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                    continue;
                }
                let at = line_start(start);
                if let Some(from) = relations_from.take() {
                    cuts.push(from..at);
                }
                if is_relations_heading(level, heading_text.trim()) {
                    relations_from = Some(at);
                }
            }
            _ => {}
        }
    }
    if let Some(from) = relations_from {
        cuts.push(from..text.len());
    }
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for cut in cuts {
        if cut.start > pos {
            out.push_str(&text[pos..cut.start]);
        }
        pos = pos.max(cut.end);
    }
    if pos < text.len() {
        out.push_str(&text[pos..]);
    }
    // Start at the first line that has a character other than whitespace.
    let mut first = out.len();
    let mut at = 0;
    for line in out.split_inclusive('\n') {
        if !line.trim().is_empty() {
            first = at;
            break;
        }
        at += line.len();
    }
    out[first..].trim_end().to_string()
}

impl Bootstrap {
    /// The payload (doc 04 §2.2 step 5): each note's content as written, in load order, with a blank line between
    /// notes. A note whose content does not start with a `# ` heading line gets `# <title>` above it. Then, when the
    /// tree has ghost links, one line that lists them after a blank line: `not loaded: Mentor (not written yet),
    /// Tone (ambiguous)`.
    pub fn payload(&self) -> String {
        let mut blocks: Vec<String> = self
            .notes
            .iter()
            .map(|n| {
                if n.body.is_empty() {
                    format!("# {}\n", n.title)
                } else if n.body.starts_with("# ") {
                    format!("{}\n", n.body)
                } else {
                    format!("# {}\n\n{}\n", n.title, n.body)
                }
            })
            .collect();
        if let Some(line) = self.not_loaded_line() {
            blocks.push(format!("{line}\n"));
        }
        blocks.join("\n")
    }

    /// `recognitor bootstrap --list`: which notes load, in order, without their content.
    ///
    /// A tree in load order (doc 04 §2.2, "CLI twin"): each note under the note whose link loaded it, drawn with
    /// `├── `, `└── ` and `│   ` like `tree`. The root is the entry point's path. Every other note shows the link as
    /// written, ` → `, and the file name it resolved to, from the stored path. A ghost shows where its link is, as
    /// written, on a dashed branch (`├╌╌ `, `└╌╌ `) with no file, marked `(not written yet)` or `(ambiguous)`, and a
    /// link to a note that loaded earlier is marked `(already loaded)` and not expanded. Two spaces come before a
    /// marker. There is no separate ghost line and no budget warning.
    pub fn list(&self) -> String {
        let n = self.tree.len();
        // An entry is the last child of its parent when the next entry that is not deeper than it is shallower.
        let is_last = |i: usize| {
            self.tree[i + 1..]
                .iter()
                .find(|e| e.depth <= self.tree[i].depth)
                .is_none_or(|e| e.depth < self.tree[i].depth)
        };
        let mut out = String::new();
        // For each level above the entry being drawn: whether that ancestor was the last child of its parent.
        let mut ancestors_last: Vec<bool> = Vec::new();
        for i in 0..n {
            let entry = &self.tree[i];
            let (label, marker) = match &entry.what {
                TreeNode::Entry => (self.notes[0].path.clone(), ""),
                TreeNode::Loaded { note, link } => (self.arrow(link, *note), ""),
                TreeNode::AlreadyLoaded { note, link } => (self.arrow(link, *note), "  (already loaded)"),
                TreeNode::Ghost { target, ambiguous } => (
                    target.clone(),
                    if *ambiguous {
                        "  (ambiguous)"
                    } else {
                        "  (not written yet)"
                    },
                ),
            };
            if entry.depth == 0 {
                out.push_str(&format!("{label}{marker}\n"));
                continue;
            }
            ancestors_last.truncate(entry.depth - 1);
            for last in &ancestors_last {
                out.push_str(if *last { "    " } else { "\u{2502}   " });
            }
            let last = is_last(i);
            // A ghost's branch is dashed (U+254C); the columns of the levels above stay solid.
            let dash = if matches!(entry.what, TreeNode::Ghost { .. }) {
                '\u{254c}'
            } else {
                '\u{2500}'
            };
            out.push(if last { '\u{2514}' } else { '\u{251c}' });
            out.push(dash);
            out.push(dash);
            out.push(' ');
            out.push_str(&format!("{label}{marker}\n"));
            ancestors_last.push(last);
        }
        out
    }

    /// `<link as written> → <file name>`: the file name is the last segment of the stored path, `.md` included.
    fn arrow(&self, link: &str, note: usize) -> String {
        let path = &self.notes[note].path;
        format!("{link} → {}", path.rsplit('/').next().unwrap_or(path))
    }

    /// The payload size in tokens, estimated as characters ÷ 4 (rounded up). The ghost line counts; a budget
    /// warning does not.
    pub fn tokens(&self) -> usize {
        estimate_tokens(self.payload().chars().count())
    }

    /// The payload, and after it the budget warning when [`Bootstrap::tokens`] is above `max_tokens` (doc 04 §2.2,
    /// "Budget": "the result carries a warning").
    ///
    /// The warning is the last line, after a blank line (doc 04 §2.2, "Budget").
    pub fn render(&self, max_tokens: usize) -> String {
        let mut out = self.payload();
        let tokens = self.tokens();
        if tokens > max_tokens {
            out.push('\n');
            out.push_str(&format!(
                "warning: the bootstrap is {tokens} tokens (estimate), above bootstrap.max_tokens ({max_tokens})\n"
            ));
        }
        out
    }

    fn not_loaded_line(&self) -> Option<String> {
        if self.not_loaded.is_empty() {
            return None;
        }
        let items: Vec<String> = self
            .not_loaded
            .iter()
            .map(|r| {
                let what = if self.ambiguous.contains(r) {
                    "ambiguous"
                } else {
                    "not written yet"
                };
                format!("{r} ({what})")
            })
            .collect();
        Some(format!("not loaded: {}", items.join(", ")))
    }
}

/// Tokens of a text, estimated as characters ÷ 4 (rounded up).
pub fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(4)
}

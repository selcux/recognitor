//! Note parser (doc 03 §2-3). One pass over the `pulldown-cmark` event stream extracts frontmatter, title, summary,
//! observations, typed relations and inline links. Rules run on the parse tree, not on raw lines, so links, code and
//! task boxes are recognized for what they are (doc 03 §1).
//!
//! The parser knows nothing about namespaces. [`silence`] applies the bootstrap and archive rules afterwards.

pub mod observation;
pub mod qualifier;

use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};

use crate::paths::{ARCHIVE_NAMESPACE, BOOTSTRAP_NAMESPACE, percent_decode, strip_md_ignore_case};
use observation::{NearMiss, near_miss, parse_observation};

/// Summary cap in characters (doc 03 §2).
pub const SUMMARY_MAX_CHARS: usize = 300;

/// Warnings the docs define (doc 02 §8, doc 03 §2-4, doc 07 §6). Non-fatal: the note is still indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WarningKind {
    /// Frontmatter is not valid YAML (doc 02 §8, rule 1).
    InvalidFrontmatter,
    /// A line under `## Relations` does not match `- <predicate> [[Target]] (context)` (doc 03 §3).
    RelationShape,
    /// An observation qualifier does not parse (doc 03 §2).
    QualifierUnparsed,
    /// The summary paragraph is longer than 300 characters (doc 03 §2).
    SummaryTooLong,
    /// A link matches more than one note and stays a ghost (doc 03 §4).
    AmbiguousLink,
    /// An observation line has no text left after the qualifier and tags are removed: kept as a plain list item.
    EmptyObservation,
    /// A file the walk listed could not be read (locked, or access denied): skipped, its stored rows are kept.
    Unreadable,
}

impl WarningKind {
    pub fn label(self) -> &'static str {
        match self {
            WarningKind::InvalidFrontmatter => "frontmatter is not valid YAML",
            WarningKind::RelationShape => "relations line of another shape",
            WarningKind::QualifierUnparsed => "qualifier does not parse",
            WarningKind::SummaryTooLong => "summary paragraph over 300 characters",
            WarningKind::AmbiguousLink => "ambiguous link",
            WarningKind::EmptyObservation => "observation line with no text",
            WarningKind::Unreadable => "file could not be read",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub kind: WarningKind,
    pub line: Option<u32>,
    pub message: String,
}

/// Inputs the docs do not cover. Each one is a placeholder behaviour, counted so the report can show how often the
/// gap fires on real data. See the slice report for the open questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Gap {
    /// No frontmatter `title` and no H1: the title falls back to the filename.
    TitleFromFilename,
    /// No H1, so no summary.
    SummaryNoH1,
    /// The block right after the H1 is not a paragraph (or there is none), so no summary.
    SummaryNotParagraph,
    /// The summary paragraph is over 300 characters and has no sentence end within them: cut at a word boundary.
    SummaryNoSentenceEnd,
    /// A Markdown link to a vault path that is not a note (another extension, or a folder): ignored, no edge.
    MarkdownLinkNotNote,
    /// A wikilink with an empty target (`[[#Heading]]`): ignored, no edge.
    EmptyWikiTarget,
    /// The file is not valid UTF-8: indexed with replacement characters.
    InvalidUtf8,
}

impl Gap {
    pub fn label(self) -> &'static str {
        match self {
            Gap::TitleFromFilename => "no frontmatter title and no H1: title from filename",
            Gap::SummaryNoH1 => "no H1: no summary",
            Gap::SummaryNotParagraph => "block after H1 is not a paragraph: no summary",
            Gap::SummaryNoSentenceEnd => "summary over 300 chars with no sentence end: cut at a word",
            Gap::MarkdownLinkNotNote => "Markdown link to a non-note path: ignored",
            Gap::EmptyWikiTarget => "wikilink with empty target ([[#Heading]]): ignored",
            Gap::InvalidUtf8 => "file is not valid UTF-8: replacement characters",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub category: String,
    pub content: String,
    pub qualifiers: Option<String>,
    pub tags: Option<String>,
    /// 1-based line of the list item in the file.
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkSyntax {
    /// `[[...]]` or `![[...]]`: resolved by doc 03 §4.
    Wiki,
    /// `[text](path.md)`: an exact file relative to the note, or to the vault root with a leading `/` (doc 03 §3).
    Markdown,
}

/// A typed edge from `## Relations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub predicate: String,
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub syntax: LinkSyntax,
    /// `edges.target_ref`: the wikilink target without alias and heading, or the Markdown link path without
    /// fragment and query, percent-encoding kept. The link key (`edges.ref_key`) is made from it by
    /// [`crate::resolve::link_key`].
    pub target: String,
    /// `Some` for a well-formed `## Relations` line; `None` for an inline `links_to` mention.
    pub relation: Option<Relation>,
    /// 1-based line of the link in the file.
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedNote {
    /// Frontmatter `title`, else the first H1. `None` when neither exists.
    pub title: Option<String>,
    /// Frontmatter `type`.
    pub note_type: Option<String>,
    pub summary: Option<String>,
    pub observations: Vec<Observation>,
    pub links: Vec<Link>,
    pub warnings: Vec<Warning>,
    pub gaps: Vec<Gap>,
    /// Bracket-led list items that are not observations (diagnostics).
    pub near_misses: Vec<NearMiss>,
}

/// What [`silence`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Suppressed {
    pub observations: usize,
    pub links: usize,
}

/// Applies the namespace rules: bootstrap notes store no observations, archive notes store no observations and no
/// edges (doc 03 §2-3, doc 04 §4). Warnings about removed items go with them.
pub fn silence(note: &mut ParsedNote, namespace: &str) -> Suppressed {
    let mut s = Suppressed::default();
    if namespace == BOOTSTRAP_NAMESPACE || namespace == ARCHIVE_NAMESPACE {
        s.observations = note.observations.len();
        note.observations.clear();
        note.warnings
            .retain(|w| !matches!(w.kind, WarningKind::QualifierUnparsed | WarningKind::EmptyObservation));
    }
    if namespace == ARCHIVE_NAMESPACE {
        s.links = note.links.len();
        note.links.clear();
        note.warnings.retain(|w| w.kind != WarningKind::RelationShape);
        note.gaps
            .retain(|g| !matches!(g, Gap::MarkdownLinkNotNote | Gap::EmptyWikiTarget));
    }
    s
}

/// The Markdown dialect every note is read in. Anything that has to agree with the parser on where headings and code
/// blocks are (the bootstrap payload does) parses with these options. They leave out metadata blocks on purpose:
/// frontmatter counts only at the very start of the file (doc 03 §2), and [`frontmatter_block`] finds it. A `---`
/// block later in the file is body text.
pub fn markdown_options() -> Options {
    Options::ENABLE_WIKILINKS | Options::ENABLE_TABLES | Options::ENABLE_FOOTNOTES | Options::ENABLE_STRIKETHROUGH
}

/// True for the heading that opens a relations section: an H2 whose text is exactly `Relations` (doc 03 §3). The
/// section runs to the next H1 or H2.
pub fn is_relations_heading(level: HeadingLevel, text: &str) -> bool {
    level == HeadingLevel::H2 && text == "Relations"
}

/// The frontmatter of `text` (without a byte-order mark): a `---` block at the very start of the file (doc 03 §2).
/// Returns its byte range, closing line included, and its YAML text. A `---` block anywhere else is not frontmatter,
/// and neither is an unclosed one.
pub fn frontmatter_block(text: &str) -> Option<(Range<usize>, String)> {
    let mut yaml = String::new();
    for (i, (event, r)) in Parser::new_ext(text, Options::ENABLE_YAML_STYLE_METADATA_BLOCKS)
        .into_offset_iter()
        .enumerate()
    {
        match event {
            Event::Start(Tag::MetadataBlock(_)) if i == 0 && r.start == 0 => {}
            Event::Text(t) if i > 0 => yaml.push_str(&t),
            Event::End(TagEnd::MetadataBlock(_)) if i > 0 => return Some((r, yaml)),
            _ => return None,
        }
    }
    None
}

/// Parses one note.
pub fn parse_note(src: &str) -> ParsedNote {
    let (base, text) = match src.strip_prefix('\u{feff}') {
        Some(rest) => ('\u{feff}'.len_utf8(), rest),
        None => (0, src),
    };
    // The frontmatter is read first and blanked out, with its line breaks kept, so that every offset and line number
    // of the body stays the same and the Markdown parser sees none of it.
    let front = frontmatter_block(text);
    let masked: std::borrow::Cow<'_, str> = match &front {
        Some((range, _)) => {
            let mut bytes = src.as_bytes().to_vec();
            for b in &mut bytes[base + range.start..base + range.end] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
            // Only whole characters of the block became spaces (one per byte), so the text stays valid UTF-8.
            std::borrow::Cow::Owned(String::from_utf8_lossy(&bytes).into_owned())
        }
        None => std::borrow::Cow::Borrowed(src),
    };
    let mut p = NoteParser::new(&masked);
    if let Some((_, yaml)) = front {
        p.metadata = yaml;
        p.frontmatter();
    }
    let text = masked.get(base..).unwrap_or_default();
    for (event, r) in Parser::new_ext(text, markdown_options()).into_offset_iter() {
        p.event(event, (r.start + base)..(r.end + base));
    }
    p.finish()
}

/// Drops the blockquote markers (`>`) of the enclosing containers from every line after the first, keeping the line
/// breaks. A paragraph line that starts with `>` would have started a blockquote, so a leading `>` on a continuation
/// line can only be a container marker.
fn strip_container_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        let mut line = line;
        if i > 0 {
            out.push('\n');
            line = line.trim_start();
            while let Some(rest) = line.strip_prefix('>') {
                line = rest.trim_start();
            }
        }
        out.push_str(line);
    }
    out
}

/// Joins the lines of a source slice with single spaces, trimming each line and dropping container markers.
pub fn collapse_lines(s: &str) -> String {
    strip_container_markers(s)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryState {
    BeforeH1,
    AwaitingBlock,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inline {
    Text,
    Break,
    Other,
}

struct OpenHeading {
    level: HeadingLevel,
    top_level: bool,
    text: String,
}

/// An open list item. Its "line" is its first block: the text before any nested block.
struct ItemCtx {
    line: u32,
    /// Event depth of the item's direct children.
    child_depth: usize,
    in_relations: bool,
    /// Source range of the first block's inline content.
    start: Option<usize>,
    end: usize,
    first_is_text: bool,
    done: bool,
    /// Indices into `links` of links inside the first block.
    links: Vec<usize>,
}

struct LinkRec {
    syntax: LinkSyntax,
    embed: bool,
    target: String,
    line: u32,
    range: Range<usize>,
    relation: Option<Relation>,
}

struct NoteParser<'a> {
    src: &'a str,
    line_starts: Vec<usize>,
    depth: usize,
    metadata: String,
    fm_title: Option<String>,
    fm_type: Option<String>,
    heading: Option<OpenHeading>,
    h1: Option<String>,
    summary_state: SummaryState,
    summary_range: Option<Range<usize>>,
    in_relations: bool,
    relations_para: Option<u32>,
    items: Vec<ItemCtx>,
    links: Vec<LinkRec>,
    observations: Vec<Observation>,
    warnings: Vec<Warning>,
    gaps: Vec<Gap>,
    near_misses: Vec<NearMiss>,
}

impl<'a> NoteParser<'a> {
    fn new(src: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(src.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i + 1));
        NoteParser {
            src,
            line_starts,
            depth: 0,
            metadata: String::new(),
            fm_title: None,
            fm_type: None,
            heading: None,
            h1: None,
            summary_state: SummaryState::BeforeH1,
            summary_range: None,
            in_relations: false,
            relations_para: None,
            items: Vec::new(),
            links: Vec::new(),
            observations: Vec::new(),
            warnings: Vec::new(),
            gaps: Vec::new(),
            near_misses: Vec::new(),
        }
    }

    fn line(&self, offset: usize) -> u32 {
        u32::try_from(self.line_starts.partition_point(|&s| s <= offset)).unwrap_or(u32::MAX)
    }

    fn warn(&mut self, kind: WarningKind, line: Option<u32>, message: String) {
        self.warnings.push(Warning { kind, line, message });
    }

    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag, range),
            Event::End(end) => self.end(end),
            Event::Text(t) => {
                if let Some(h) = &mut self.heading {
                    h.text.push_str(&t);
                }
                self.inline(range, Inline::Text);
            }
            Event::Code(t) => {
                if let Some(h) = &mut self.heading {
                    h.text.push_str(&t);
                }
                self.inline(range, Inline::Other);
            }
            Event::SoftBreak | Event::HardBreak => self.inline(range, Inline::Break),
            Event::InlineHtml(_) | Event::FootnoteReference(_) | Event::InlineMath(_) | Event::TaskListMarker(_) => {
                self.inline(range, Inline::Other);
            }
            Event::Rule => {
                if self.depth == 0 {
                    self.top_level_block(false, range);
                }
                if let Some(item) = self.items.last_mut() {
                    item.done = true;
                }
            }
            Event::Html(_) | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: Range<usize>) {
        if is_block(&tag) {
            let item_first_para = matches!(tag, Tag::Paragraph)
                && self
                    .items
                    .last()
                    .is_some_and(|i| !i.done && i.start.is_none() && self.depth == i.child_depth);
            if self.depth == 0 {
                self.top_level_block(matches!(tag, Tag::Paragraph), range.clone());
            }
            if let Some(item) = self.items.last_mut()
                && !item.done
                && !item_first_para
            {
                item.done = true;
            }
            if matches!(tag, Tag::Paragraph) && self.in_relations && !item_first_para {
                self.relations_para = Some(self.line(range.start));
            }
        } else {
            self.inline(range.clone(), Inline::Other);
        }
        match tag {
            Tag::Heading { level, .. } => {
                self.heading = Some(OpenHeading {
                    level,
                    top_level: self.depth == 0,
                    text: String::new(),
                });
            }
            Tag::Item => {
                let line = self.line(range.start);
                self.items.push(ItemCtx {
                    line,
                    child_depth: self.depth + 1,
                    in_relations: self.in_relations,
                    start: None,
                    end: 0,
                    first_is_text: false,
                    done: false,
                    links: Vec::new(),
                });
            }
            Tag::Link {
                link_type, dest_url, ..
            } => self.link(link_type, &dest_url, false, range),
            Tag::Image {
                link_type, dest_url, ..
            } => self.link(link_type, &dest_url, true, range),
            _ => {}
        }
        self.depth += 1;
    }

    fn end(&mut self, end: TagEnd) {
        self.depth = self.depth.saturating_sub(1);
        match end {
            TagEnd::Heading(_) => {
                if let Some(h) = self.heading.take() {
                    self.heading_done(h);
                }
            }
            TagEnd::Paragraph => {
                let depth = self.depth;
                if let Some(item) = self.items.last_mut()
                    && !item.done
                    && item.start.is_some()
                    && depth == item.child_depth
                {
                    item.done = true;
                }
                if let Some(line) = self.relations_para.take() {
                    self.warn(WarningKind::RelationShape, Some(line), relation_shape_message());
                }
            }
            TagEnd::Item => {
                if let Some(item) = self.items.pop() {
                    self.item_done(item);
                }
            }
            _ => {}
        }
    }

    fn top_level_block(&mut self, is_paragraph: bool, range: Range<usize>) {
        if self.summary_state == SummaryState::AwaitingBlock {
            if is_paragraph {
                self.summary_range = Some(range);
            } else {
                self.gaps.push(Gap::SummaryNotParagraph);
            }
            self.summary_state = SummaryState::Done;
        }
    }

    fn inline(&mut self, range: Range<usize>, kind: Inline) {
        // A backslash escape (`\[fact]`) arrives as plain text that starts after the backslash.
        let escaped = self.src[..range.start].ends_with('\\');
        if let Some(item) = self.items.last_mut()
            && !item.done
        {
            if item.start.is_none() {
                if kind == Inline::Break {
                    return;
                }
                item.start = Some(range.start);
                item.first_is_text = kind == Inline::Text && !escaped;
            }
            item.end = item.end.max(range.end);
        }
    }

    fn link(&mut self, link_type: LinkType, dest: &str, embed: bool, range: Range<usize>) {
        let (syntax, target) = match link_type {
            LinkType::WikiLink { .. } => {
                let target = dest.split('#').next().unwrap_or("").trim();
                if target.is_empty() {
                    self.gaps.push(Gap::EmptyWikiTarget);
                    return;
                }
                // Embeds of non-Markdown files (images, PDFs) are ignored (doc 03 §3).
                if embed && has_non_md_extension(target) {
                    return;
                }
                (LinkSyntax::Wiki, target.to_string())
            }
            LinkType::Autolink | LinkType::Email => return,
            _ if embed => return,
            _ => match markdown_target(dest) {
                MdTarget::Note(t) => (LinkSyntax::Markdown, t),
                MdTarget::NotNote => {
                    self.gaps.push(Gap::MarkdownLinkNotNote);
                    return;
                }
                MdTarget::Ignored => return,
            },
        };
        let idx = self.links.len();
        let line = self.line(range.start);
        self.links.push(LinkRec {
            syntax,
            embed,
            target,
            line,
            range,
            relation: None,
        });
        if let Some(item) = self.items.last_mut()
            && !item.done
        {
            item.links.push(idx);
        }
    }

    fn frontmatter(&mut self) {
        match serde_yaml::from_str::<serde_yaml::Value>(&self.metadata) {
            Ok(value) => {
                self.fm_title = yaml_scalar(value.get("title"));
                self.fm_type = yaml_scalar(value.get("type"));
            }
            Err(e) => {
                let detail = e.to_string();
                let detail = detail.lines().next().unwrap_or("").to_string();
                self.warn(
                    WarningKind::InvalidFrontmatter,
                    Some(1),
                    format!("frontmatter is not valid YAML: {detail}"),
                );
            }
        }
    }

    fn heading_done(&mut self, h: OpenHeading) {
        let text = h.text.trim().to_string();
        if h.level == HeadingLevel::H1 && h.top_level && self.summary_state == SummaryState::BeforeH1 {
            if !text.is_empty() {
                self.h1 = Some(text.clone());
            }
            self.summary_state = SummaryState::AwaitingBlock;
        }
        if matches!(h.level, HeadingLevel::H1 | HeadingLevel::H2) {
            self.in_relations = is_relations_heading(h.level, &text);
        }
    }

    fn item_done(&mut self, item: ItemCtx) {
        let Some(start) = item.start else { return };
        let end = item.end.max(start);
        let text = collapse_lines(&self.src[start..end]);
        if !item.first_is_text {
            let unescaped = text.strip_prefix('\\').unwrap_or(&text);
            if parse_observation(unescaped).is_some() {
                self.near_misses.push(NearMiss::NotPlainText);
            }
        } else if let Some(miss) = near_miss(&text) {
            self.near_misses.push(miss);
        } else if let Some(o) = parse_observation(&text) {
            if let Some(token) = &o.unparsed_qualifier {
                let msg = format!("qualifier \"{token}\" does not parse; it stays text");
                self.warn(WarningKind::QualifierUnparsed, Some(item.line), msg);
            }
            if o.no_text {
                let msg = format!(
                    "observation \"[{}]\" has no text; it stays a plain list item",
                    o.category
                );
                self.warn(WarningKind::EmptyObservation, Some(item.line), msg);
            } else {
                self.observations.push(Observation {
                    category: o.category,
                    content: o.content,
                    qualifiers: o.qualifier,
                    tags: o.tags,
                    line: item.line,
                });
            }
        }
        if item.in_relations {
            match self.relation_shape(start, end, &item.links) {
                Some(rel) => {
                    if let Some(&idx) = item.links.first() {
                        self.links[idx].relation = Some(rel);
                    }
                }
                None => self.warn(WarningKind::RelationShape, Some(item.line), relation_shape_message()),
            }
        }
    }

    /// `<predicate> [[Target]] (context)`: one wikilink, a single-token predicate before it, and nothing after it
    /// but an optional parenthesised context (doc 03 §3).
    fn relation_shape(&self, start: usize, end: usize, links: &[usize]) -> Option<Relation> {
        let [idx] = links else { return None };
        let link = &self.links[*idx];
        if link.syntax != LinkSyntax::Wiki || link.embed {
            return None;
        }
        let prefix = strip_container_markers(self.src.get(start..link.range.start)?);
        let suffix = strip_container_markers(self.src.get(link.range.end..end)?);
        let predicate = prefix.trim_end();
        let spaced = prefix.len() > predicate.len();
        if !spaced || predicate.is_empty() || !predicate.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        let rest = suffix.trim();
        let context = if rest.is_empty() {
            None
        } else {
            let inner = rest.strip_prefix('(')?.strip_suffix(')')?;
            Some(collapse_lines(inner)).filter(|c| !c.is_empty())
        };
        Some(Relation {
            predicate: predicate.to_string(),
            context,
        })
    }

    fn finish(mut self) -> ParsedNote {
        if self.summary_state == SummaryState::BeforeH1 {
            self.gaps.push(Gap::SummaryNoH1);
        } else if self.summary_state == SummaryState::AwaitingBlock {
            self.gaps.push(Gap::SummaryNotParagraph);
        }
        let summary = self.summary_range.clone().map(|r| {
            let text = collapse_lines(&self.src[r.clone()]);
            let chars = text.chars().count();
            if chars > SUMMARY_MAX_CHARS {
                let line = self.line(r.start);
                let msg = format!("summary paragraph is {chars} characters; the limit is {SUMMARY_MAX_CHARS}");
                self.warn(WarningKind::SummaryTooLong, Some(line), msg);
            }
            let (cut, no_sentence_end) = cut_summary(&text);
            if no_sentence_end {
                self.gaps.push(Gap::SummaryNoSentenceEnd);
            }
            cut
        });
        let links = self
            .links
            .into_iter()
            .map(|l| Link {
                syntax: l.syntax,
                target: l.target,
                relation: l.relation,
                line: l.line,
            })
            .collect();
        ParsedNote {
            title: self.fm_title.or(self.h1),
            note_type: self.fm_type,
            summary,
            observations: self.observations,
            links,
            warnings: self.warnings,
            gaps: self.gaps,
            near_misses: self.near_misses,
        }
    }
}

fn relation_shape_message() -> String {
    "line under \"## Relations\" does not match \"- <predicate> [[Target]] (context)\"; its links are indexed as \
     links_to"
        .to_string()
}

fn is_block(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::List(_)
            | Tag::Item
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::MetadataBlock(_)
    )
}

fn yaml_scalar(v: Option<&serde_yaml::Value>) -> Option<String> {
    let s = match v? {
        serde_yaml::Value::String(s) => s.trim().to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

fn has_non_md_extension(target: &str) -> bool {
    let name = target.rsplit('/').next().unwrap_or(target);
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => !name[i + 1..].eq_ignore_ascii_case("md"),
        _ => false,
    }
}

enum MdTarget {
    Note(String),
    NotNote,
    Ignored,
}

/// Classifies a Markdown link destination (doc 03 §3, "Markdown Links"). A destination without an extension counts as
/// a note, because the link key leaves out `.md` (doc 03 §4, DR3-05).
fn markdown_target(dest: &str) -> MdTarget {
    let dest = dest.trim();
    if dest.is_empty() || dest.starts_with('#') || has_scheme(dest) {
        return MdTarget::Ignored;
    }
    let path = dest.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() {
        return MdTarget::Ignored;
    }
    // A link key leaves out `.md` (doc 03 §4), so `[x](Setup)` and `[x](Setup.md)` are the same link. A path with
    // another extension (`notes.txt`, `report.pdf`) or a folder (`dir/`) is not a note.
    let decoded = percent_decode(path);
    // A path that normalizes to a folder (`.`, `..`, `a/..`, `./`) is no note and no ghost: ignored.
    if is_folder_path(&decoded) {
        return MdTarget::Ignored;
    }
    if decoded.ends_with('/') || (strip_md_ignore_case(&decoded).is_none() && has_non_md_extension(&decoded)) {
        return MdTarget::NotNote;
    }
    MdTarget::Note(path.to_string())
}

/// True when the last meaningful segment of the path is `.` or `..`, so it can only name a folder.
fn is_folder_path(path: &str) -> bool {
    matches!(path.rsplit('/').find(|s| !s.is_empty()), Some("." | ".."))
}

/// `scheme:` at the start: external URLs, `mailto:`, `file:` (doc 03 §3).
fn has_scheme(s: &str) -> bool {
    let Some(i) = s.find(':') else { return false };
    let scheme = &s[..i];
    scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// Cuts a summary to at most 300 characters at the last sentence end. Returns the text and whether no sentence end
/// was found (then the cut falls on the last word boundary).
fn cut_summary(text: &str) -> (String, bool) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.len() <= SUMMARY_MAX_CHARS {
        return (text.to_string(), false);
    }
    let mut sentence_end = None;
    for (i, &(byte, c)) in chars.iter().enumerate().take(SUMMARY_MAX_CHARS) {
        let followed_by_space = chars.get(i + 1).is_none_or(|&(_, n)| n.is_whitespace());
        if matches!(c, '.' | '!' | '?') && followed_by_space {
            sentence_end = Some(byte + c.len_utf8());
        }
    }
    if let Some(end) = sentence_end {
        return (text[..end].to_string(), false);
    }
    let word_end = chars
        .iter()
        .take(SUMMARY_MAX_CHARS + 1)
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|&(b, _)| b);
    let end = word_end.unwrap_or(chars[SUMMARY_MAX_CHARS].0);
    (text[..end].trim_end().to_string(), true)
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! The outline of a note's text for `read_note` (doc 07 §7, "Sections"): the head, the sections as a tree, and where
//! the top-level blocks start.
//!
//! A section is a heading and everything under it, up to the next heading of the same or a higher level, so its
//! subsections stay inside it. The note's first `#` heading is its title, not a section; everything before the first
//! heading after the title is the head. Headings are found with the parser the indexer uses, so a heading-shaped
//! line inside code is text (doc 03 §3, "Code is never parsed"). All offsets are byte offsets into the text given.

use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag};

use crate::parse::{frontmatter_block, markdown_options};

/// One section: its heading text as written, and the bytes from the start of its heading line to the start of the
/// next heading of the same or a higher level (or the end of the text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    /// The name as tools show and accept it: `name`, or `name [k]` when sibling sections share the name (doc 07
    /// §7.3), numbered from 1 in file order.
    pub display: String,
    pub level: u8,
    pub start: usize,
    pub end: usize,
    /// The subsections, in file order.
    pub children: Vec<Section>,
}

impl Section {
    /// Where the section's own head ends: the start of its first subsection, or its end.
    pub fn head_end(&self) -> usize {
        self.children.first().map_or(self.end, |c| c.start)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    /// End of the byte-order mark and the frontmatter, closing line break included; `0` when the text has neither.
    pub fm_end: usize,
    /// Start of each block that sits at the top level of the document (not inside a list or a quote), after the
    /// frontmatter, each moved back to the start of its line.
    pub block_starts: Vec<usize>,
    /// Start of the first section, or the end of the text when the note has none.
    pub head_end: usize,
    /// The top-level sections, in file order.
    pub sections: Vec<Section>,
}

struct Flat {
    level: u8,
    name: String,
    start: usize,
}

/// The outline of `text`, a whole file.
pub fn outline(text: &str) -> Outline {
    let bom = if text.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let mut fm_end = bom + frontmatter_block(&text[bom..]).map_or(0, |(range, _)| range.end);
    // The closing `---` line keeps its line break, so the frontmatter is whole lines.
    if text[fm_end..].starts_with("\r\n") {
        fm_end += 2;
    } else if text[fm_end..].starts_with('\n') {
        fm_end += 1;
    }
    let body = &text[fm_end..];
    let line_start = |offset: usize| text[..offset].rfind('\n').map_or(0, |i| i + 1);

    let mut depth = 0usize;
    let mut block_starts: Vec<usize> = Vec::new();
    let mut flat: Vec<Flat> = Vec::new();
    let mut open: Option<(u8, Range<usize>)> = None;
    for (event, range) in Parser::new_ext(body, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    block_starts.push(line_start(fm_end + range.start));
                    if let Tag::Heading { level, .. } = tag {
                        open = Some((level_number(level), range.clone()));
                    }
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some((level, r)) = open.take()
                {
                    flat.push(Flat {
                        level,
                        name: heading_name(&body[r.clone()]),
                        start: line_start(fm_end + r.start),
                    });
                }
            }
            Event::Rule if depth == 0 => block_starts.push(line_start(fm_end + range.start)),
            _ => {}
        }
    }
    block_starts.dedup();

    // The first heading is the title when it is an H1; it is not a section.
    if flat.first().is_some_and(|h| h.level == 1) {
        flat.remove(0);
    }
    let head_end = flat.first().map_or(text.len(), |h| h.start);
    // `next[i]`: the first heading after `i` of the same or a higher level, which ends section `i`.
    let mut next = vec![flat.len(); flat.len()];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..flat.len() {
        while let Some(&top) = stack.last() {
            if flat[top].level >= flat[i].level {
                next[top] = i;
                stack.pop();
            } else {
                break;
            }
        }
        stack.push(i);
    }
    let mut sections = build(&flat, &next, 0..flat.len(), text.len());
    number_repeats(&mut sections);
    Outline {
        fm_end,
        block_starts,
        head_end,
        sections,
    }
}

fn build(flat: &[Flat], next: &[usize], range: Range<usize>, text_len: usize) -> Vec<Section> {
    let mut out = Vec::new();
    let mut i = range.start;
    while i < range.end {
        let k = next[i].min(range.end);
        let end = if next[i] < flat.len() {
            flat[next[i]].start
        } else {
            text_len
        };
        out.push(Section {
            name: flat[i].name.clone(),
            display: flat[i].name.clone(),
            level: flat[i].level,
            start: flat[i].start,
            end,
            children: build(flat, next, (i + 1)..k, text_len),
        });
        i = k;
    }
    out
}

/// Numbers siblings that share a name, `Notes [1]`, `Notes [2]`, from 1 in file order. Names are compared in lowercase,
/// as `section` is matched; a unique name stays plain.
fn number_repeats(sections: &mut [Section]) {
    let mut totals: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in sections.iter() {
        *totals.entry(s.name.trim().to_lowercase()).or_default() += 1;
    }
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for s in sections.iter_mut() {
        let key = s.name.trim().to_lowercase();
        let n = seen.entry(key.clone()).or_default();
        *n += 1;
        if totals.get(&key).copied().unwrap_or(0) > 1 {
            s.display = format!("{} [{}]", s.name, n);
        }
        number_repeats(&mut s.children);
    }
}

fn level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The heading text as written, without its `#` marks (ATX) or its underline (setext).
fn heading_name(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let first = lines.first().copied().unwrap_or("").trim();
    if first.starts_with('#') {
        let text = first.trim_start_matches('#').trim();
        // A closing sequence of `#` after a space is not part of the text.
        let without = text.trim_end_matches('#');
        let text = if without.len() < text.len() && (without.is_empty() || without.ends_with(char::is_whitespace)) {
            without.trim_end()
        } else {
            text
        };
        return text.to_string();
    }
    let n = lines.len().saturating_sub(1).max(1);
    lines[..n.min(lines.len())]
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sections: &[Section]) -> Vec<String> {
        sections.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn the_title_is_not_a_section_and_the_head_ends_at_the_first_heading() {
        let text = "---\ntype: x\n---\n# T\n\nSum.\n\n## A\n\na\n\n### A1\n\nb\n\n## B\n\nc\n";
        let o = outline(text);
        assert_eq!(names(&o.sections), ["A", "B"]);
        assert_eq!(names(&o.sections[0].children), ["A1"]);
        assert_eq!(&text[o.fm_end..o.fm_end + 3], "# T");
        assert_eq!(&text[o.head_end..o.head_end + 4], "## A");
        assert_eq!(
            &text[o.sections[0].start..o.sections[0].end],
            "## A\n\na\n\n### A1\n\nb\n\n"
        );
        assert_eq!(
            &text[o.sections[0].children[0].start..o.sections[0].children[0].end],
            "### A1\n\nb\n\n"
        );
        assert_eq!(&text[o.sections[1].start..], "## B\n\nc\n");
    }

    #[test]
    fn a_section_ends_at_a_heading_of_the_same_or_a_higher_level() {
        let text = "# T\n\n### Deep\n\nx\n\n## Mid\n\ny\n\n# Other\n\nz\n";
        let o = outline(text);
        assert_eq!(names(&o.sections), ["Deep", "Mid", "Other"]);
        assert_eq!(&text[o.sections[0].start..o.sections[0].end], "### Deep\n\nx\n\n");
        assert_eq!(&text[o.sections[1].start..o.sections[1].end], "## Mid\n\ny\n\n");
    }

    #[test]
    fn siblings_that_share_a_name_are_numbered() {
        let text = "# T\n\n## Notes\n\n### Notes\n\n### Other\n\n## notes\n\n## Solo\n\n### Notes\n";
        let o = outline(text);
        let shown: Vec<&str> = o.sections.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(shown, ["Notes [1]", "notes [2]", "Solo"]);
        let inner: Vec<&str> = o.sections[0].children.iter().map(|s| s.display.as_str()).collect();
        assert_eq!(inner, ["Notes", "Other"]);
        assert_eq!(o.sections[2].children[0].display, "Notes");
    }

    #[test]
    fn code_is_never_a_heading() {
        let text = "# T\n\n```\n## Not a heading\n```\n\n## Real\n\nx\n";
        let o = outline(text);
        assert_eq!(names(&o.sections), ["Real"]);
    }

    #[test]
    fn no_headings_means_no_sections_and_the_head_is_everything() {
        let text = "just text\n\nmore\n";
        let o = outline(text);
        assert!(o.sections.is_empty());
        assert_eq!(o.head_end, text.len());
        assert_eq!(o.block_starts, [0, 11]);
    }

    #[test]
    fn heading_names_are_as_written() {
        assert_eq!(heading_name("## Storage ##\n"), "Storage");
        assert_eq!(heading_name("##   The `foo` flag  \n"), "The `foo` flag");
        assert_eq!(heading_name("C#\n==\n"), "C#");
        assert_eq!(heading_name("## C#\n"), "C#");
    }
}

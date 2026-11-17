//! Parser tests. Each input comes from an example in doc 03 unless marked otherwise.

use super::*;

const AUTH_FLOW: &str = "---
title: Auth Flow
type: spec
tags: [security, oauth]
---

# Auth Flow

Handles OAuth2 token refresh and session rotation across backend services.

## Observations
- [fact] Access tokens expire after 15 minutes. #tokens
- [decision] @effective:2026-06-10 Refresh tokens are rotated on every exchange.

## Relations
- part_of [[Security Architecture]]
- supersedes [[Legacy Cookie Auth]] (replaced after the 2026 audit)
- relates_to [[User Session Store]]
";

fn explicit(note: &ParsedNote) -> Vec<(String, String, Option<String>)> {
    note.links
        .iter()
        .filter_map(|l| {
            l.relation
                .as_ref()
                .map(|r| (r.predicate.clone(), l.target.clone(), r.context.clone()))
        })
        .collect()
}

fn inline_targets(note: &ParsedNote) -> Vec<String> {
    note.links
        .iter()
        .filter(|l| l.relation.is_none())
        .map(|l| l.target.clone())
        .collect()
}

fn kinds(note: &ParsedNote) -> Vec<WarningKind> {
    note.warnings.iter().map(|w| w.kind).collect()
}

#[test]
fn doc_03_anatomy_example() {
    let n = parse_note(AUTH_FLOW);
    assert_eq!(n.title.as_deref(), Some("Auth Flow"));
    assert_eq!(n.note_type.as_deref(), Some("spec"));
    assert_eq!(
        n.summary.as_deref(),
        Some("Handles OAuth2 token refresh and session rotation across backend services.")
    );
    assert_eq!(n.observations.len(), 2);
    assert_eq!(n.observations[0].category, "fact");
    assert_eq!(n.observations[0].tags.as_deref(), Some("tokens"));
    assert_eq!(n.observations[0].line, 12);
    assert_eq!(n.observations[1].qualifiers.as_deref(), Some("@effective:2026-06-10"));
    assert_eq!(
        n.observations[1].content,
        "Refresh tokens are rotated on every exchange."
    );
    assert_eq!(
        explicit(&n),
        vec![
            ("part_of".into(), "Security Architecture".into(), None),
            (
                "supersedes".into(),
                "Legacy Cookie Auth".into(),
                Some("replaced after the 2026 audit".into())
            ),
            ("relates_to".into(), "User Session Store".into(), None),
        ]
    );
    assert_eq!(n.links[0].line, 16);
    assert!(n.warnings.is_empty(), "{:?}", n.warnings);
}

#[test]
fn title_falls_back_to_h1_then_none() {
    let n = parse_note("# From Heading\n\nText.\n");
    assert_eq!(n.title.as_deref(), Some("From Heading"));
    assert_eq!(n.note_type, None);
    let n = parse_note("Just text, no heading.\n");
    assert_eq!(n.title, None);
    assert_eq!(n.summary, None);
    assert!(n.gaps.contains(&Gap::SummaryNoH1));
}

#[test]
fn frontmatter_title_wins_over_h1() {
    let n = parse_note("---\ntitle: Front\n---\n# Heading\n\nSummary.\n");
    assert_eq!(n.title.as_deref(), Some("Front"));
}

#[test]
fn invalid_yaml_warns_and_still_indexes() {
    let n = parse_note("---\ntitle: [unclosed\n---\n# Heading\n\nSummary here.\n");
    assert_eq!(kinds(&n), vec![WarningKind::InvalidFrontmatter]);
    assert_eq!(n.title.as_deref(), Some("Heading"));
    assert_eq!(n.summary.as_deref(), Some("Summary here."));
}

#[test]
fn summary_joins_lines_of_the_paragraph() {
    let n = parse_note("# T\n\nFirst line\nsecond line.\n\nNot this.\n");
    assert_eq!(n.summary.as_deref(), Some("First line second line."));
}

#[test]
fn summary_cut_at_sentence_end_within_300() {
    let first = "a".repeat(200) + ".";
    let second = " ".to_string() + &"b".repeat(150) + ".";
    let n = parse_note(&format!("# T\n\n{first}{second}\n"));
    assert_eq!(n.summary.as_deref(), Some(first.as_str()));
    assert_eq!(kinds(&n), vec![WarningKind::SummaryTooLong]);
    assert!(!n.gaps.contains(&Gap::SummaryNoSentenceEnd));
}

#[test]
fn summary_without_sentence_end_cut_at_word() {
    let words = "word ".repeat(80);
    let n = parse_note(&format!("# T\n\n{words}\n"));
    let s = n.summary.unwrap();
    assert!(s.chars().count() <= 300);
    assert!(s.ends_with("word"));
    assert!(n.gaps.contains(&Gap::SummaryNoSentenceEnd));
}

#[test]
fn summary_needs_a_paragraph_right_after_h1() {
    let n = parse_note("# T\n\n## Observations\n- [fact] x\n\nLater paragraph.\n");
    assert_eq!(n.summary, None);
    assert!(n.gaps.contains(&Gap::SummaryNotParagraph));
}

#[test]
fn observation_exclusions_in_real_lists() {
    let n = parse_note(
        "# T\n\n- [ ] todo\n- [x] done\n- [X] done\n- [-] dropped\n- [/] doing\n- [>] later\n- [?] q\n- [!] imp\n\
         - [b] book\n- [24:33.098] transcript\n- [Title](file.md) a link\n- [fact] counted\n",
    );
    assert_eq!(n.observations.len(), 1);
    assert_eq!(n.observations[0].content, "counted");
}

#[test]
fn observations_anywhere_and_nested() {
    let n = parse_note("# T\n\nIntro.\n\n## Notes\n- parent\n  - [rule] nested one\n> - [fact] in a quote\n");
    let cats: Vec<&str> = n.observations.iter().map(|o| o.category.as_str()).collect();
    assert_eq!(cats, vec!["rule", "fact"]);
    assert_eq!(n.observations[0].content, "nested one");
}

#[test]
fn observation_stops_before_nested_list() {
    let n = parse_note("# T\n\n- [fact] parent text\n  continued\n  - child item\n");
    assert_eq!(n.observations.len(), 1);
    assert_eq!(n.observations[0].content, "parent text continued");
}

#[test]
fn code_is_never_parsed() {
    let n = parse_note(
        "# T\n\nSee `[[Not A Link]]` and `[x](y.md)`.\n\n```\n- [fact] in code\n[[Also Not]]\n```\n\n\
         - `[fact]` code first\n",
    );
    assert!(n.observations.is_empty());
    assert!(n.links.is_empty());
}

#[test]
fn escaped_bracket_is_not_an_observation() {
    let n = parse_note("# T\n\n- \\[fact] escaped\n");
    assert!(n.observations.is_empty());
}

#[test]
fn qualifier_that_does_not_parse_warns_and_stays_text() {
    let n = parse_note("# T\n\n- [fact] @yesterday it rained\n");
    assert_eq!(n.observations[0].qualifiers, None);
    assert_eq!(n.observations[0].content, "@yesterday it rained");
    assert_eq!(kinds(&n), vec![WarningKind::QualifierUnparsed]);
}

#[test]
fn relations_lines_of_other_shapes() {
    let n = parse_note("# T\n\nS.\n\n## Relations\n- see also [[X]]\n- [[Y]]\n- part_of [[Z]] trailing words\n");
    assert!(explicit(&n).is_empty());
    assert_eq!(inline_targets(&n), vec!["X", "Y", "Z"]);
    assert_eq!(kinds(&n), vec![WarningKind::RelationShape; 3]);
}

#[test]
fn relations_section_ends_at_next_h2() {
    let n = parse_note("# T\n\nS.\n\n## Relations\n- part_of [[A]]\n\n## Later\n- Mother [[Alice]]\n");
    assert_eq!(explicit(&n), vec![("part_of".into(), "A".into(), None)]);
    assert_eq!(inline_targets(&n), vec!["Alice"]);
    assert!(n.warnings.is_empty());
}

#[test]
fn body_bullet_with_word_and_link_is_an_inline_mention() {
    let n = parse_note("# T\n\nS.\n\n- Mother [[Alice]]\n");
    assert!(explicit(&n).is_empty());
    assert_eq!(inline_targets(&n), vec!["Alice"]);
    assert!(n.warnings.is_empty());
}

#[test]
fn relations_paragraph_warns() {
    let n = parse_note("# T\n\nS.\n\n## Relations\nSee [[X]].\n");
    assert_eq!(inline_targets(&n), vec!["X"]);
    assert_eq!(kinds(&n), vec![WarningKind::RelationShape]);
}

#[test]
fn prefixed_target_in_relations() {
    let n = parse_note("# T\n\nS.\n\n## Relations\n- relates_to [[cognition::index]] (optional context)\n");
    assert_eq!(
        explicit(&n),
        vec![(
            "relates_to".into(),
            "cognition::index".into(),
            Some("optional context".into())
        )]
    );
}

#[test]
fn wikilink_forms() {
    let n = parse_note("# T\n\nSee [[A|alias]], [[B#Heading]], ![[C]] and ![[diagram.png]].\n");
    assert_eq!(inline_targets(&n), vec!["A", "B", "C"]);
    assert!(n.links.iter().all(|l| l.syntax == LinkSyntax::Wiki));
}

#[test]
fn markdown_links() {
    let n = parse_note(
        "# T\n\nFollow [the deploy checklist](../ops/Deploy%20Checklist.md) before release.\n\
         [a](https://example.com/x.md) [b](mailto:me@example.com) [c](file:///x.md) ![img](pic.png)\n\
         [d](#section) [e](/ops/Root.md#part) [f](q.md?x=1) [g](notes.txt)\n",
    );
    let md: Vec<(String, LinkSyntax)> = n.links.iter().map(|l| (l.target.clone(), l.syntax)).collect();
    assert_eq!(
        md,
        vec![
            ("../ops/Deploy%20Checklist.md".to_string(), LinkSyntax::Markdown),
            ("/ops/Root.md".to_string(), LinkSyntax::Markdown),
            ("q.md".to_string(), LinkSyntax::Markdown),
        ]
    );
    assert_eq!(n.gaps.iter().filter(|g| **g == Gap::MarkdownLinkNotNote).count(), 1);
}

#[test]
fn markdown_link_without_extension_is_a_note_link() {
    // The link key leaves out `.md` (doc 03 §4, DR3-05), so `[x](Setup)` is the same link as `[x](Setup.md)`.
    let n = parse_note("# T\n\n[a](Setup) [b](guides/Setup%20Guide) [c](report.pdf) [d](folder/) [e](v1.2)\n");
    let targets: Vec<&str> = n.links.iter().map(|l| l.target.as_str()).collect();
    assert_eq!(targets, vec!["Setup", "guides/Setup%20Guide"]);
    assert_eq!(n.gaps.iter().filter(|g| **g == Gap::MarkdownLinkNotNote).count(), 3);
}

#[test]
fn silence_archive_and_bootstrap() {
    let src = "# T\n\nS.\n\n- [fact] one\n- @x\n\n## Relations\n- part_of [[A]]\n- odd [[B]] shape\n";
    let mut archive = parse_note(src);
    let s = silence(&mut archive, "archive");
    assert_eq!(
        s,
        Suppressed {
            observations: 1,
            links: 2
        }
    );
    assert!(archive.observations.is_empty() && archive.links.is_empty() && archive.warnings.is_empty());

    let mut boot = parse_note(src);
    let s = silence(&mut boot, "bootstrap");
    assert_eq!(
        s,
        Suppressed {
            observations: 1,
            links: 0
        }
    );
    assert_eq!(boot.links.len(), 2);
    assert_eq!(kinds(&boot), vec![WarningKind::RelationShape]);

    let mut knowledge = parse_note(src);
    assert_eq!(silence(&mut knowledge, "backend"), Suppressed::default());
}

#[test]
fn bom_and_crlf() {
    let n = parse_note("\u{feff}---\r\ntitle: X\r\n---\r\n# H\r\n\r\nSummary.\r\n\r\n- [fact] a\r\n");
    assert_eq!(n.title.as_deref(), Some("X"));
    assert_eq!(n.summary.as_deref(), Some("Summary."));
    assert_eq!(n.observations[0].line, 8);
}

#[test]
fn near_misses_are_counted() {
    use observation::NearMiss;
    let n =
        parse_note("# T\n\n- [ ] task\n- [fact]: no space\n- \\[fact] escaped\n- [fact] counted\n- [[Link]] item\n");
    let mut got = n.near_misses.clone();
    got.sort();
    assert_eq!(
        got,
        vec![NearMiss::NotPlainText, NearMiss::NotCategory, NearMiss::NoSpace]
    );
    assert_eq!(n.observations.len(), 1);
}

// Fix-pass regressions (not from the docs): container markers, folder links, empty observations, calendar dates.

#[test]
fn blockquote_markers_are_dropped_from_continuation_lines() {
    let n = parse_note("# T\n\n> - [fact] Tokens expire\n>   after 15 minutes.\n");
    assert_eq!(n.observations.len(), 1);
    assert_eq!(n.observations[0].content, "Tokens expire after 15 minutes.");
    // Nested quotes, and a tag on the continuation line.
    let n = parse_note("# T\n\n> > - [fact] a\n> >   b\n> >   #tag\n");
    assert_eq!(n.observations[0].content, "a b #tag");
    assert_eq!(n.observations[0].tags.as_deref(), Some("tag"));
}

#[test]
fn blockquote_markers_are_dropped_from_relation_context() {
    let n = parse_note("# T\n\n## Relations\n> - supersedes [[Old]] (replaced\n>   after the audit)\n");
    assert_eq!(
        explicit(&n),
        vec![(
            "supersedes".to_string(),
            "Old".to_string(),
            Some("replaced after the audit".to_string())
        )]
    );
    assert!(kinds(&n).is_empty());
}

#[test]
fn markdown_links_to_folders_are_ignored() {
    let n = parse_note("# T\n\n[a](.) [b](..) [c](../) [d](sub/..) [e](./) [f](x.md)\n");
    assert_eq!(inline_targets(&n), vec!["x.md".to_string()]);
}

#[test]
fn observation_without_text_is_not_stored() {
    for line in [
        "- [decision] @2026-06-10",
        "- [fact] #tag",
        "- [fact] @2026-06-10 #tag #other",
    ] {
        let n = parse_note(&format!("# T\n\n{line}\n"));
        assert!(n.observations.is_empty(), "{line}");
        assert_eq!(kinds(&n), vec![WarningKind::EmptyObservation], "{line}");
        assert_eq!(n.warnings[0].line, Some(3));
    }
    // Text before the tags keeps it an observation.
    let n = parse_note("# T\n\n- [fact] @2026-06-10 text #tag\n");
    assert_eq!(n.observations.len(), 1);
    assert!(n.warnings.is_empty());
}

#[test]
fn empty_observation_warning_goes_with_silenced_notes() {
    let mut n = parse_note("# T\n\n- [fact] #tag\n");
    silence(&mut n, "bootstrap");
    assert!(n.warnings.is_empty());
}

#[test]
fn impossible_calendar_dates_stay_text() {
    for date in ["2026-02-31", "2026-04-31", "2026-02-29"] {
        let n = parse_note(&format!("# T\n\n- [fact] @{date} Something happened\n"));
        assert_eq!(kinds(&n), vec![WarningKind::QualifierUnparsed], "{date}");
        assert_eq!(n.observations[0].qualifiers, None, "{date}");
        assert_eq!(n.observations[0].content, format!("@{date} Something happened"));
    }
    let n = parse_note("# T\n\n- [fact] @2024-02-29 Leap day\n");
    assert!(n.warnings.is_empty());
    assert_eq!(n.observations[0].qualifiers.as_deref(), Some("@2024-02-29"));
}

#[test]
fn frontmatter_counts_only_at_the_start() {
    // The example that exposed it: a block after a blank line is body text.
    let n = parse_note("# Real\n\nSummary.\n\nText.\n\n---\ntitle: Mid\ntype: x\n---\n\nAfter [[Link]].\n");
    assert_eq!(n.title.as_deref(), Some("Real"));
    assert_eq!(n.note_type, None);
    assert_eq!(n.links.len(), 1);
    // Frontmatter at the start still works, with or without a byte-order mark, and keeps line numbers.
    let n = parse_note("\u{feff}---\ntitle: Front\n---\n# H\n\nS.\n\n- [fact] One.\n");
    assert_eq!(n.title.as_deref(), Some("Front"));
    assert_eq!(n.observations[0].line, 8);
    // An unclosed `---` at the start is not frontmatter.
    let n = parse_note("---\ntitle: Nope\n\n# H\n");
    assert_eq!(n.title.as_deref(), Some("H"));
}

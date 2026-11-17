//! Observation line shape (doc 03 §2, "Observation Syntax"): `[category] @qualifier text #tag1 #tag2`.
//!
//! This module works on the list item's text. The caller has already checked, on the parse tree, that the item's
//! first inline element is plain text, so links, code and escaped brackets never reach it.

use super::qualifier::is_qualifier;

/// The parts of one observation line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationParts {
    /// Lowercase category.
    pub category: String,
    /// Text without the category bracket and qualifier. Trailing tags stay in it.
    pub content: String,
    /// The raw qualifier, when one parsed.
    pub qualifier: Option<String>,
    /// Trailing tags, space-separated, without `#`.
    pub tags: Option<String>,
    /// A token after the category that starts with `@` but does not parse. It stays in `content`.
    pub unparsed_qualifier: Option<String>,
    /// Nothing is left once the qualifier and the trailing tags are removed (`[decision] @2026-06-10`, `[fact] #tag`).
    /// Such a line is not stored as an observation.
    pub no_text: bool,
}

/// Parses `text` (the item's first block, line breaks already turned into spaces) as an observation.
pub fn parse_observation(text: &str) -> Option<ObservationParts> {
    let rest = text.strip_prefix('[')?;
    let close = rest.find(']')?;
    let category = &rest[..close];
    if !is_category(category) {
        return None;
    }
    let after = &rest[close + 1..];
    // "followed by a space": `[fact]text` and a bare `[fact]` are not observations.
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let mut body = after.trim_start();
    let mut qualifier = None;
    let mut unparsed_qualifier = None;
    if body.starts_with('@') {
        let end = body.find(char::is_whitespace).unwrap_or(body.len());
        let token = &body[..end];
        if is_qualifier(token) {
            qualifier = Some(token.to_string());
            body = body[end..].trim_start();
        } else {
            unparsed_qualifier = Some(token.to_string());
        }
    }
    let content = body.trim().to_string();
    let tags = trailing_tags(&content);
    let no_text = content.split_whitespace().all(|t| tag_name(t).is_some());
    Some(ObservationParts {
        category: category.to_lowercase(),
        content,
        qualifier,
        tags,
        unparsed_qualifier,
        no_text,
    })
}

/// A list item that starts with a bracket but is not an observation. Diagnostics only: it explains differences
/// between an observation count made by hand and the parser's count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NearMiss {
    /// Observation-shaped text, but the first inline element is a link, code span or escape, not plain text.
    NotPlainText,
    /// The bracket does not hold a category: task boxes, timestamps, one-letter statuses.
    NotCategory,
    /// The bracket holds a category but no space follows it (`[fact]: x`).
    NoSpace,
}

impl NearMiss {
    pub fn label(self) -> &'static str {
        match self {
            NearMiss::NotPlainText => "observation-shaped, but starts with a link, code or escape",
            NearMiss::NotCategory => "bracket is not a category (task box, timestamp, status)",
            NearMiss::NoSpace => "category bracket not followed by a space",
        }
    }
}

/// Why `text`, which starts with `[` and is plain text, is not an observation. `None` when it is one or when it does
/// not start with a bracket.
pub fn near_miss(text: &str) -> Option<NearMiss> {
    let rest = text.strip_prefix('[')?;
    if parse_observation(text).is_some() {
        return None;
    }
    let Some(close) = rest.find(']') else {
        return Some(NearMiss::NotCategory);
    };
    if !is_category(&rest[..close]) {
        return Some(NearMiss::NotCategory);
    }
    Some(NearMiss::NoSpace)
}

/// Starts with a letter, then letters, digits, `_` or `-`; at least 2 characters (doc 03 §2).
fn is_category(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    first.is_alphabetic() && s.chars().count() >= 2 && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// The run of `#tag` tokens at the end of the text.
fn trailing_tags(content: &str) -> Option<String> {
    let mut tags: Vec<&str> = content.split_whitespace().rev().map_while(|t| tag_name(t)).collect();
    if tags.is_empty() {
        return None;
    }
    tags.reverse();
    Some(tags.join(" "))
}

/// `#name` where the name holds letters, digits, `_`, `-` or `/`, and at least one character that is not a digit.
fn tag_name(token: &str) -> Option<&str> {
    let name = token.strip_prefix('#')?;
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        && name.chars().any(|c| !c.is_ascii_digit());
    valid.then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(text: &str) -> ObservationParts {
        parse_observation(text).unwrap_or_else(|| panic!("{text:?} should be an observation"))
    }

    #[test]
    fn doc_03_anatomy_example() {
        let o = obs("[fact] Access tokens expire after 15 minutes. #tokens");
        assert_eq!(o.category, "fact");
        assert_eq!(o.content, "Access tokens expire after 15 minutes. #tokens");
        assert_eq!(o.tags.as_deref(), Some("tokens"));
        assert_eq!(o.qualifier, None);

        let o = obs("[decision] @effective:2026-06-10 Refresh tokens are rotated on every exchange.");
        assert_eq!(o.category, "decision");
        assert_eq!(o.qualifier.as_deref(), Some("@effective:2026-06-10"));
        assert_eq!(o.content, "Refresh tokens are rotated on every exchange.");
    }

    #[test]
    fn syntax_line_from_doc_03() {
        let o = obs("[category] @qualifier Observation text. #tag1 #tag2");
        assert_eq!(o.category, "category");
        assert_eq!(o.tags.as_deref(), Some("tag1 tag2"));
        // `@qualifier` is not an ISO value, so it stays text and is reported.
        assert_eq!(o.unparsed_qualifier.as_deref(), Some("@qualifier"));
        assert_eq!(o.content, "@qualifier Observation text. #tag1 #tag2");
    }

    #[test]
    fn category_is_lowercased() {
        assert_eq!(obs("[Fact] x").category, "fact");
    }

    #[test]
    fn what_never_counts() {
        for text in [
            "[ ] todo",
            "[x] done",
            "[X] done",
            "[-] cancelled",
            "[/] doing",
            "[>] later",
            "[?] question",
            "[!] important",
            "[b] bookmark",
            "[24:33.098] transcript line",
            "[fact]no space",
            "[fact]",
            "plain text",
            "[1abc] digit first",
        ] {
            assert_eq!(parse_observation(text), None, "{text:?}");
        }
    }

    #[test]
    fn categories_allow_digits_dash_underscore() {
        assert_eq!(obs("[use-when] x").category, "use-when");
        assert_eq!(obs("[step_2] x").category, "step_2");
    }

    #[test]
    fn tags_only_trailing() {
        assert_eq!(obs("[fact] Use #rust here").tags, None);
        assert_eq!(
            obs("[fact] Use rust #lang/rust #a-b").tags.as_deref(),
            Some("lang/rust a-b")
        );
        assert_eq!(obs("[fact] Issue #123").tags, None);
    }

    #[test]
    fn range_qualifier() {
        let o = obs("[rule] @effective[2026-06-10,) Rotate keys.");
        assert_eq!(o.qualifier.as_deref(), Some("@effective[2026-06-10,)"));
        assert_eq!(o.content, "Rotate keys.");
    }
}

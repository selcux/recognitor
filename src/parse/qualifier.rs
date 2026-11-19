// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! Observation qualifiers (doc 03 §2, "Qualifier"): `@kind:value`, `@kind[start,end)`, or a bare `@value` (kind
//! `valid`). Values are ISO 8601 only: `2026`, `2026-06`, `2026-06-10`, or a timestamp.

/// The qualifier kinds doc 03 §2 lists.
pub const KINDS: [&str; 5] = ["effective", "valid", "occurred", "due", "mentioned"];

/// True when `token` (starting with `@`) is a qualifier doc 03 §2 accepts.
pub fn is_qualifier(token: &str) -> bool {
    let Some(body) = token.strip_prefix('@') else {
        return false;
    };
    for kind in KINDS {
        if let Some(after) = body.strip_prefix(kind) {
            if let Some(value) = after.strip_prefix(':') {
                return is_value(value);
            }
            if after.starts_with(['[', '(']) {
                return is_range(after);
            }
        }
    }
    is_value(body)
}

/// `[` or `(`, optional value, `,`, optional value, `]` or `)`. An empty side is open.
fn is_range(s: &str) -> bool {
    let Some(inner) = s.strip_prefix(['[', '(']).and_then(|r| r.strip_suffix([']', ')'])) else {
        return false;
    };
    let mut sides = inner.split(',');
    let (Some(start), Some(end), None) = (sides.next(), sides.next(), sides.next()) else {
        return false;
    };
    (start.is_empty() || is_value(start)) && (end.is_empty() || is_value(end))
}

/// `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or `YYYY-MM-DDThh:mm[:ss[.fff]][Z|±hh[:mm]|±hhmm]` (ISO 8601 extended format).
pub fn is_value(s: &str) -> bool {
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let parts: Vec<&str> = date.split('-').collect();
    let date_ok = match parts.as_slice() {
        [y] => digits(y, 4),
        [y, m] => digits(y, 4) && in_range(m, 1, 12),
        [y, m, d] => digits(y, 4) && in_range(m, 1, 12) && real_day(y, m, d),
        _ => false,
    };
    if !date_ok {
        return false;
    }
    match time {
        None => true,
        Some(t) => parts.len() == 3 && is_time(t),
    }
}

/// The day exists in that month and year, leap years included (doc 02 §8, check 3).
fn real_day(y: &str, m: &str, d: &str) -> bool {
    let (Ok(year), Ok(month)) = (y.parse::<u32>(), m.parse::<u32>()) else {
        return false;
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let last = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    in_range(d, 1, last)
}

fn is_time(t: &str) -> bool {
    let (clock, zone) = split_zone(t);
    if !zone.is_empty() && !is_zone(zone) {
        return false;
    }
    let (hms, frac) = match clock.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (clock, None),
    };
    let fields: Vec<&str> = hms.split(':').collect();
    let ok = match fields.as_slice() {
        [h, m] => in_range(h, 0, 23) && in_range(m, 0, 59) && frac.is_none(),
        [h, m, s] => in_range(h, 0, 23) && in_range(m, 0, 59) && in_range(s, 0, 60),
        _ => false,
    };
    ok && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

fn split_zone(t: &str) -> (&str, &str) {
    if let Some(clock) = t.strip_suffix('Z') {
        return (clock, "Z");
    }
    match t.find(['+', '-']) {
        Some(i) => (&t[..i], &t[i..]),
        None => (t, ""),
    }
}

fn is_zone(z: &str) -> bool {
    if z == "Z" {
        return true;
    }
    let Some(rest) = z.strip_prefix(['+', '-']) else {
        return false;
    };
    match rest.split_once(':') {
        Some((h, m)) => in_range(h, 0, 23) && in_range(m, 0, 59),
        None if rest.len() == 2 => in_range(rest, 0, 23),
        None if rest.len() == 4 => in_range(&rest[..2], 0, 23) && in_range(&rest[2..], 0, 59),
        None => false,
    }
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

fn in_range(s: &str, lo: u32, hi: u32) -> bool {
    digits(s, 2) && s.parse::<u32>().is_ok_and(|v| (lo..=hi).contains(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_03_examples_parse() {
        for q in [
            "@effective:2026-06-10",
            "@effective[2026-06-10,)",
            "@valid[2026-01-01,2026-07-01)",
            "@2026",
            "@2026-06",
            "@2026-06-10",
            "@due:2026-06-10T14:30",
            "@occurred:2026-06-10T14:30:00Z",
            "@mentioned:2026-06-10T14:30:00.123+02:00",
            "@effective:2026-11-11",
            "@valid(,2027]",
            "@2024-02-29",
            "@2000-02-29",
            "@2026-04-30",
            "@2026-12-31",
        ] {
            assert!(is_qualifier(q), "{q} should parse");
        }
    }

    #[test]
    fn non_iso_and_unknown_kinds_do_not_parse() {
        for q in [
            "@yesterday",
            "@10/06/2026",
            "@effective:June",
            "@alice",
            "@unknown:2026",
            "@2026-13",
            "@2026-06-32",
            "@2026-02-31",
            "@2026-04-31",
            "@2026-02-29",
            "@1900-02-29",
            "@effective:2026-06-31",
            "@effective[2026",
            "@effective[2026,2027,2028)",
            "@[2026,2027)",
            "@2026-06T10:00",
            "@",
        ] {
            assert!(!is_qualifier(q), "{q} should not parse");
        }
    }
}

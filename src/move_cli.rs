//! The prompt of `recognitor move` (doc 07 §6.1, "CLI"): after the move, the user picks which linking notes `relink`
//! rewrites. Reading the answer is kept apart from the terminal: [`ask`] takes any reader and writer, so tests drive it
//! with text.
//!
//! The rules of the answer are not in the docs; they are this slice's choices and are listed in its report:
//! `all`, `none` or numbers from the list (`1,3`, `1 3`, `2-4`); Enter alone, and the end of input, mean `none`, so no
//! note is ever written without an explicit answer; an answer that is none of these is asked again.

use std::io::{self, BufRead, Write};
use std::path::Path;

/// What the user answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    All,
    None,
    /// Positions in the list, from 0, sorted, each once.
    Pick(Vec<usize>),
}

/// Parses one line of answer for a list of `count` notes. `Err` is the text that says what was wrong.
pub fn parse_answer(line: &str, count: usize) -> Result<Answer, String> {
    let line = line.trim();
    match line.to_lowercase().as_str() {
        "" | "n" | "none" | "no" => return Ok(Answer::None),
        "a" | "all" | "y" | "yes" => return Ok(Answer::All),
        _ => {}
    }
    let wrong = || format!("\"{line}\" is not an answer. Enter all, none, or numbers from 1 to {count}, like 1,3.");
    let mut picked: Vec<usize> = Vec::new();
    for token in line
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        let (from, to) = match token.split_once('-') {
            Some((a, b)) => (
                a.parse::<usize>().map_err(|_| wrong())?,
                b.parse::<usize>().map_err(|_| wrong())?,
            ),
            None => {
                let n = token.parse::<usize>().map_err(|_| wrong())?;
                (n, n)
            }
        };
        if from > to {
            return Err(wrong());
        }
        for n in [from, to] {
            if n < 1 || n > count {
                return Err(format!(
                    "{n} is not in the list, which has {}. Enter all, none, or numbers from 1 to {count}.",
                    if count == 1 {
                        "1 note".to_string()
                    } else {
                        format!("{count} notes")
                    }
                ));
            }
        }
        picked.extend(from - 1..to);
    }
    picked.sort_unstable();
    picked.dedup();
    Ok(Answer::Pick(picked))
}

/// The list and the question, as shown before the first answer.
pub fn prompt_text(notes: &[String]) -> String {
    let mut s = if notes.len() == 1 {
        "1 note has a link to the moved note:\n".to_string()
    } else {
        format!("{} notes have a link to the moved note:\n", notes.len())
    };
    let width = notes.len().to_string().len();
    for (i, note) in notes.iter().enumerate() {
        s.push_str(&format!("  {:>width$}. {note}\n", i + 1));
    }
    s.push_str("Rewrite the links in which? [all / none / numbers, e.g. 1,3] (Enter = none): ");
    s
}

/// Asks which of `notes` to relink and returns them, in list order. Writes the prompt to `out` and reads answers from
/// `input` until one parses. The end of `input` counts as `none`: the user closed the pipe or pressed Ctrl-D.
pub fn ask(notes: &[String], input: &mut impl BufRead, out: &mut impl Write) -> io::Result<Vec<String>> {
    write!(out, "{}", prompt_text(notes))?;
    out.flush()?;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(out)?;
            return Ok(Vec::new());
        }
        match parse_answer(&line, notes.len()) {
            Ok(Answer::All) => return Ok(notes.to_vec()),
            Ok(Answer::None) => return Ok(Vec::new()),
            Ok(Answer::Pick(positions)) => return Ok(positions.into_iter().map(|i| notes[i].clone()).collect()),
            Err(message) => {
                writeln!(out, "{message}")?;
                write!(out, "Rewrite the links in which? ")?;
                out.flush()?;
            }
        }
    }
}

/// The `recognitor relink` command line that finishes what `move` started: the CLI's twin of the `relink(...)` call
/// of doc 07 §11. `vault` is the `--vault` the user gave, repeated so the line works as printed. Ids go in double
/// quotes, since a note name can hold spaces.
pub fn finish_command(old: &str, new: &str, notes: &[String], vault: Option<&Path>) -> String {
    let mut s = format!("recognitor relink \"{old}\" \"{new}\"");
    for note in notes {
        s.push_str(&format!(" \"{note}\""));
    }
    if let Some(v) = vault {
        s.push_str(&format!(" --vault \"{}\"", v.display()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn notes() -> Vec<String> {
        ["ideas/index", "ops/Runbook", "ops/Other"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn run(input: &str) -> (Vec<String>, String) {
        let mut out = Vec::new();
        let picked = ask(&notes(), &mut Cursor::new(input.to_string()), &mut out).unwrap();
        (picked, String::from_utf8(out).unwrap())
    }

    #[test]
    fn all_and_none_in_any_case() {
        assert_eq!(parse_answer("all", 3), Ok(Answer::All));
        assert_eq!(parse_answer(" A \n", 3), Ok(Answer::All));
        assert_eq!(parse_answer("None", 3), Ok(Answer::None));
        assert_eq!(parse_answer("n", 3), Ok(Answer::None));
    }

    #[test]
    fn enter_alone_is_none() {
        assert_eq!(parse_answer("\n", 3), Ok(Answer::None));
        assert_eq!(parse_answer("   ", 3), Ok(Answer::None));
    }

    #[test]
    fn numbers_by_comma_space_and_range_come_back_sorted_and_once() {
        assert_eq!(parse_answer("3,1", 3), Ok(Answer::Pick(vec![0, 2])));
        assert_eq!(parse_answer("2 3 2", 3), Ok(Answer::Pick(vec![1, 2])));
        assert_eq!(parse_answer("1-2", 3), Ok(Answer::Pick(vec![0, 1])));
        assert_eq!(parse_answer("1, 3", 3), Ok(Answer::Pick(vec![0, 2])));
    }

    #[test]
    fn a_bad_answer_says_what_is_wrong() {
        assert_eq!(
            parse_answer("maybe", 3),
            Err("\"maybe\" is not an answer. Enter all, none, or numbers from 1 to 3, like 1,3.".to_string())
        );
        assert_eq!(
            parse_answer("4", 3),
            Err("4 is not in the list, which has 3 notes. Enter all, none, or numbers from 1 to 3.".to_string())
        );
        assert!(parse_answer("0", 3).is_err());
        assert!(parse_answer("3-1", 3).is_err());
        assert!(parse_answer("1,x", 3).is_err());
    }

    #[test]
    fn the_prompt_lists_the_notes_numbered() {
        let text = prompt_text(&notes());
        assert_eq!(
            text,
            "3 notes have a link to the moved note:\n  1. ideas/index\n  2. ops/Runbook\n  3. ops/Other\n\
             Rewrite the links in which? [all / none / numbers, e.g. 1,3] (Enter = none): "
        );
        assert!(prompt_text(&notes()[..1]).starts_with("1 note has a link to the moved note:\n  1. ideas/index\n"));
    }

    #[test]
    fn ask_returns_the_picked_notes_in_list_order() {
        assert_eq!(run("all\n").0, notes());
        assert_eq!(run("3,1\n").0, vec!["ideas/index".to_string(), "ops/Other".to_string()]);
        assert_eq!(run("\n").0, Vec::<String>::new());
    }

    #[test]
    fn a_bad_answer_is_asked_again_and_the_next_one_counts() {
        let (picked, shown) = run("9\nmaybe\n2\n");
        assert_eq!(picked, vec!["ops/Runbook".to_string()]);
        assert!(shown.contains("9 is not in the list"), "{shown}");
        assert!(shown.contains("\"maybe\" is not an answer"), "{shown}");
    }

    #[test]
    fn the_end_of_input_is_none_even_after_a_bad_answer() {
        assert_eq!(run("").0, Vec::<String>::new());
        assert_eq!(run("nope\n").0, Vec::<String>::new());
    }

    #[test]
    fn the_finish_command_quotes_every_id_and_repeats_the_vault_flag() {
        let list = vec!["ops/Runbook".to_string(), "ops/Sync design".to_string()];
        assert_eq!(
            finish_command("ideas/Caching", "design/b/Caching", &list, None),
            "recognitor relink \"ideas/Caching\" \"design/b/Caching\" \"ops/Runbook\" \"ops/Sync design\""
        );
        assert_eq!(
            finish_command("a/B", "c/B", &list[..1], Some(Path::new("/v"))),
            "recognitor relink \"a/B\" \"c/B\" \"ops/Runbook\" --vault \"/v\""
        );
    }
}

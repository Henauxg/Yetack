//! What the chat shows of a tool's output in few words: the lines a `Read` returned, what a
//! search found, and runs of lines that say the same thing.

use strum::{EnumString, IntoStaticStr, VariantArray};

/// How Claude Code's `Read` separates a line's number from its text.
const READ_NUMBER_ENDS: [char; 2] = ['\t', '→'];

/// How Claude Code's `Grep` and `Glob` start their output.
const FOUND_PREFIX: &str = "Found ";
const FILE_WORD: &str = " file";
const NO_FILES: &str = "No files found";
const NO_MATCHES: &str = "No matches found";
/// How `Glob` ends a list it cut.
const NOTE_START: char = '(';

/// The characters that open and close a quoted part of a line.
const QUOTES: [char; 3] = ['\'', '"', '`'];
/// Stands for the quoted text that changes along a run of lines.
const VARYING: char = '…';

/// The first and last line numbers a `Read` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub first: u32,
    pub last: u32,
}

/// The lines a `Read` returned, from the numbers Claude Code puts before each line. `None` for an
/// output without numbered lines, such as an empty file's note.
pub fn read_lines(output: &str) -> Option<LineRange> {
    let mut numbers = output.lines().filter_map(line_number);
    let first = numbers.next()?;
    let last = numbers.next_back().unwrap_or(first);
    Some(LineRange { first, last })
}

fn line_number(line: &str) -> Option<u32> {
    let (number, _) = line.trim_start().split_once(READ_NUMBER_ENDS)?;
    number.parse().ok()
}

/// The tools that search the agent's files, by their names in Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
pub enum SearchTool {
    Grep,
    Glob,
}

/// What a search found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    Files(usize),
    /// Matching lines, from a search that shows them.
    Lines(usize),
    Nothing,
}

/// What a `Grep` or a `Glob` found, read from its output.
pub fn found(tool: SearchTool, output: &str) -> Found {
    let output = output.trim();
    if output.is_empty() || output.starts_with(NO_FILES) || output.starts_with(NO_MATCHES) {
        return Found::Nothing;
    }
    let first_line = output.lines().next().unwrap_or_default();
    if let Some(files) = found_files(first_line) {
        return Found::Files(files);
    }
    let listed = output
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with(NOTE_START) && !line.starts_with(FOUND_PREFIX)
        })
        .count();
    match tool {
        SearchTool::Glob => Found::Files(listed),
        SearchTool::Grep => Found::Lines(listed),
    }
}

/// "Found 4 files" reads 4.
fn found_files(line: &str) -> Option<usize> {
    let rest = line.strip_prefix(FOUND_PREFIX)?;
    let (count, noun) = rest.split_once(' ')?;
    format!(" {noun}")
        .starts_with(FILE_WORD)
        .then(|| count.parse().ok())
        .flatten()
}

/// A part of a tool's output, as the chat shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPart {
    /// Lines as they are, joined by line breaks.
    Lines(String),
    /// Consecutive lines that differ at most in their quoted parts, shown once.
    Repeated {
        /// The first line, with `…` in the quoted parts that change along the run.
        shown: String,
        count: usize,
        /// The lines, joined by line breaks.
        lines: String,
    },
}

/// The output in parts, a line break between each two. A run of two lines or more that differ at
/// most in their quoted parts becomes one part. Blank lines never form a run.
pub fn output_parts(output: &str) -> Vec<OutputPart> {
    let lines: Vec<&str> = output.split('\n').collect();
    let mut parts = Vec::new();
    let mut plain: Vec<&str> = Vec::new();
    let mut rest: &[&str] = &lines;
    while let Some(&line) = rest.first() {
        let run_length = run_length(rest);
        let (run, after) = rest.split_at(run_length.max(1));
        rest = after;
        if run_length < 2 {
            plain.push(line);
            continue;
        }
        if !plain.is_empty() {
            parts.push(OutputPart::Lines(plain.join("\n")));
            plain.clear();
        }
        parts.push(OutputPart::Repeated {
            shown: shown_line(run),
            count: run_length,
            lines: run.join("\n"),
        });
    }
    if !plain.is_empty() {
        parts.push(OutputPart::Lines(plain.join("\n")));
    }
    parts
}

/// How many lines from the first one say the same thing as it.
fn run_length(lines: &[&str]) -> usize {
    let Some(first) = lines.first() else {
        return 0;
    };
    if first.trim().is_empty() {
        return 1;
    }
    let key = Split::of(first);
    lines
        .iter()
        .take_while(|line| Split::of(line).same_frame(&key))
        .count()
}

/// The first line of a run, with `…` in the quoted parts that change along it.
fn shown_line(run: &[&str]) -> String {
    let splits: Vec<Split<'_>> = run.iter().map(|line| Split::of(line)).collect();
    let Some(first) = splits.first() else {
        return String::new();
    };
    let mut shown = String::new();
    for (index, frame) in first.frames.iter().enumerate() {
        shown.push_str(frame);
        let Some(quoted) = first.quoted.get(index) else {
            continue;
        };
        let same = splits
            .iter()
            .all(|split| split.quoted.get(index) == Some(quoted));
        if same {
            shown.push_str(quoted);
        } else {
            let quote = quoted.chars().next().unwrap_or(VARYING);
            shown.push(quote);
            shown.push(VARYING);
            shown.push(quote);
        }
    }
    shown
}

/// A line cut at its quoted parts: the text around them, and the parts with their quotes. There
/// is one more frame than quoted parts.
#[derive(Debug, PartialEq, Eq)]
struct Split<'line> {
    frames: Vec<&'line str>,
    quoted: Vec<&'line str>,
}

impl<'line> Split<'line> {
    fn of(line: &'line str) -> Self {
        let mut frames = Vec::new();
        let mut quoted = Vec::new();
        let mut rest = line;
        while let Some((frame, part, after)) = next_quoted(rest) {
            frames.push(frame);
            quoted.push(part);
            rest = after;
        }
        frames.push(rest);
        Self { frames, quoted }
    }

    /// The same text around the quoted parts.
    fn same_frame(&self, other: &Self) -> bool {
        self.frames == other.frames && self.quoted.len() == other.quoted.len()
    }
}

/// The text before the first quoted part of `text`, the part with its quotes, and the text after
/// it. `None` when no quote closes.
fn next_quoted(text: &str) -> Option<(&str, &str, &str)> {
    let open = text.find(QUOTES)?;
    let (frame, from_quote) = text.split_at(open);
    let quote = from_quote.chars().next()?;
    let inside = from_quote.get(quote.len_utf8()..)?;
    let close = inside.find(quote)?;
    let (part, after) = from_quote.split_at(quote.len_utf8() * 2 + close);
    Some((frame, part, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_tools_names_are_pinned() {
        let names: Vec<&str> = SearchTool::VARIANTS
            .iter()
            .map(|&tool| tool.into())
            .collect();

        assert_eq!(names, ["Grep", "Glob"]);
    }

    #[test]
    fn a_read_returns_the_lines_its_numbers_say() {
        let tabbed = "    40\tpub fn next_token() {\n    41\t    self.skip();\n   121\t}\n";
        let arrows = "     1→use std::io;\n     2→\n";
        let reminder = "     7→fn main() {}\n\n<system-reminder>\nWhatever.\n</system-reminder>\n";

        assert_eq!(
            read_lines(tabbed),
            Some(LineRange {
                first: 40,
                last: 121
            })
        );
        assert_eq!(read_lines(arrows), Some(LineRange { first: 1, last: 2 }));
        assert_eq!(read_lines(reminder), Some(LineRange { first: 7, last: 7 }));
        assert_eq!(
            read_lines("<system-reminder>The file is empty.</system-reminder>"),
            None
        );
    }

    #[test]
    fn a_search_says_how_many_files_or_lines_it_found() {
        let files =
            "Found 4 files\n/work/src/lexer.rs\n/work/src/error.rs\n/work/src/a.rs\n/work/b.rs";
        let one = "Found 1 file\n/work/src/lexer.rs";
        let content = "/work/src/lexer.rs:12:let column = 0;\n/work/src/lexer.rs:30:column += 1;";
        let globbed = "/work/tests/columns.rs\n/work/tests/parser.rs\n(Results are truncated.)";

        assert_eq!(found(SearchTool::Grep, files), Found::Files(4));
        assert_eq!(found(SearchTool::Grep, one), Found::Files(1));
        assert_eq!(found(SearchTool::Grep, content), Found::Lines(2));
        assert_eq!(found(SearchTool::Glob, globbed), Found::Files(2));
        assert_eq!(found(SearchTool::Glob, "No files found"), Found::Nothing);
        assert_eq!(found(SearchTool::Grep, "No matches found"), Found::Nothing);
        assert_eq!(found(SearchTool::Grep, ""), Found::Nothing);
    }

    #[test]
    fn lines_that_differ_only_in_quotes_show_once_with_their_count() {
        let output = "warning: in 'src/a.rs', LF becomes CRLF\n\
                      warning: in 'src/b.rs', LF becomes CRLF\n\
                      warning: in 'src/c.rs', LF becomes CRLF\n\
                      [main 3f2a91c] Count columns from 1";

        assert_eq!(
            output_parts(output),
            [
                OutputPart::Repeated {
                    shown: "warning: in '…', LF becomes CRLF".to_owned(),
                    count: 3,
                    lines: "warning: in 'src/a.rs', LF becomes CRLF\n\
                            warning: in 'src/b.rs', LF becomes CRLF\n\
                            warning: in 'src/c.rs', LF becomes CRLF"
                        .to_owned(),
                },
                OutputPart::Lines("[main 3f2a91c] Count columns from 1".to_owned()),
            ]
        );
    }

    #[test]
    fn a_quoted_part_that_stays_the_same_shows_as_it_is() {
        let output = "skip \"tests\" in `a`\nskip \"tests\" in `b`";

        assert_eq!(
            output_parts(output),
            [OutputPart::Repeated {
                shown: "skip \"tests\" in `…`".to_owned(),
                count: 2,
                lines: output.to_owned(),
            }]
        );
    }

    #[test]
    fn different_lines_and_blank_lines_stay_as_they_are() {
        let output = "test a ... ok\ntest b ... ok\n\n\nidentical\nidentical\n";

        assert_eq!(
            output_parts(output),
            [
                OutputPart::Lines("test a ... ok\ntest b ... ok\n\n".to_owned()),
                OutputPart::Repeated {
                    shown: "identical".to_owned(),
                    count: 2,
                    lines: "identical\nidentical".to_owned(),
                },
                OutputPart::Lines(String::new()),
            ]
        );
    }

    #[test]
    fn an_unclosed_quote_is_text() {
        assert_eq!(
            Split::of("skip 'a' and 'b"),
            Split {
                frames: vec!["skip ", " and 'b"],
                quoted: vec!["'a'"],
            }
        );
    }
}

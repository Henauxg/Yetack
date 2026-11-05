//! Line diffs with word highlights inside changed lines and syntax colors, for inline edits and
//! the file view.

use std::ops::Range;
use std::time::{Duration, Instant};

use similar::{ChangeTag, DiffOp, TextDiff};

use super::syntax::{Language, Span, TokenClass, highlight};

/// How long a diff may take before similar approximates it, so a huge file cannot freeze the UI.
const DIFF_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub class: TokenClass,
    /// The changed words of a changed line.
    pub emphasized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_number: Option<u32>,
    pub new_number: Option<u32>,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRow {
    Line(DiffLine),
    /// Unchanged lines left out between two changes.
    Folded {
        lines: u32,
    },
}

/// Lines are numbered from `first_line` (1 for a whole file). Unchanged lines more than `context`
/// lines away from a change are left out: a `Folded` row between two groups of changes, nothing
/// before the first group or after the last. Equal texts give no row.
pub fn diff_lines(
    old: &str,
    new: &str,
    language: Option<Language>,
    context: usize,
    first_line: u32,
) -> Vec<DiffRow> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let started = Instant::now();
    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_slices(&old_lines, &new_lines);
    let groups = diff.grouped_ops(context);
    if groups.is_empty() {
        return Vec::new();
    }
    let old_spans = highlight(old, language);
    let new_spans = highlight(new, language);
    let deadline = started.checked_add(DIFF_TIMEOUT);
    let mut rows = Vec::new();
    let mut previous_end: Option<usize> = None;
    for group in &groups {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        if let Some(end) = previous_end {
            let skipped = first.old_range().start.saturating_sub(end);
            if skipped > 0 {
                rows.push(DiffRow::Folded {
                    lines: u32::try_from(skipped).unwrap_or(u32::MAX),
                });
            }
        }
        previous_end = Some(last.old_range().end);
        for operation in group {
            push_operation(
                &mut rows, &diff, operation, deadline, &old_spans, &new_spans, first_line,
            );
        }
    }
    rows
}

/// Every line of `text` as `Context` with syntax colors, for the file view's "Whole file" mode.
pub fn plain_lines(text: &str, language: Option<Language>, first_line: u32) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    for (index, spans) in highlight(text, language).into_iter().enumerate() {
        let number = line_number(index, first_line);
        lines.push(DiffLine {
            kind: LineKind::Context,
            old_number: number,
            new_number: number,
            segments: segments(&spans, &[]),
        });
    }
    lines
}

fn push_operation(
    rows: &mut Vec<DiffRow>,
    diff: &TextDiff<'_, '_, str>,
    operation: &DiffOp,
    deadline: Option<Instant>,
    old_spans: &[Vec<Span>],
    new_spans: &[Vec<Span>],
    first_line: u32,
) {
    for change in diff.iter_inline_changes_deadline(operation, deadline) {
        let old_index = change.old_index();
        let new_index = change.new_index();
        let (kind, spans) = match change.tag() {
            ChangeTag::Equal => (
                LineKind::Context,
                new_index.and_then(|index| new_spans.get(index)),
            ),
            ChangeTag::Delete => (
                LineKind::Removed,
                old_index.and_then(|index| old_spans.get(index)),
            ),
            ChangeTag::Insert => (
                LineKind::Added,
                new_index.and_then(|index| new_spans.get(index)),
            ),
        };
        let spans: &[Span] = spans.map_or(&[], Vec::as_slice);
        let mut emphasized = Vec::new();
        let mut offset = 0;
        for &(is_emphasized, value) in change.values() {
            let end = offset + value.len();
            if is_emphasized && end > offset {
                emphasized.push(offset..end);
            }
            offset = end;
        }
        // A line changed from end to end is already marked by its row color.
        let covers_line = emphasized.iter().map(|range| range.len()).sum::<usize>() >= offset;
        if covers_line {
            emphasized.clear();
        }
        rows.push(DiffRow::Line(DiffLine {
            kind,
            old_number: old_index.and_then(|index| line_number(index, first_line)),
            new_number: new_index.and_then(|index| line_number(index, first_line)),
            segments: segments(spans, &emphasized),
        }));
    }
}

fn line_number(index: usize, first_line: u32) -> Option<u32> {
    u32::try_from(index).ok()?.checked_add(first_line)
}

/// Cuts the spans of one line at the edges of the emphasized byte ranges, which are sorted and do
/// not overlap.
fn segments(spans: &[Span], emphasized: &[Range<usize>]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::with_capacity(spans.len());
    let mut span_start = 0;
    for span in spans {
        let span_end = span_start + span.text.len();
        let mut cut = span_start;
        while cut < span_end {
            let (next, is_emphasized) = next_cut(cut, span_end, emphasized);
            if let Some(text) = span.text.get(cut - span_start..next - span_start) {
                push_segment(&mut segments, text, span.class, is_emphasized);
            }
            cut = next;
        }
        span_start = span_end;
    }
    segments
}

/// From `position`, the end of the run that is either inside one emphasized range or outside all
/// of them, and whether it is inside. The run stops at `limit`.
fn next_cut(position: usize, limit: usize, emphasized: &[Range<usize>]) -> (usize, bool) {
    for range in emphasized {
        if range.contains(&position) {
            return (range.end.min(limit), true);
        }
        if range.start > position {
            return (range.start.min(limit), false);
        }
    }
    (limit, false)
}

fn push_segment(segments: &mut Vec<Segment>, text: &str, class: TokenClass, emphasized: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = segments.last_mut()
        && last.class == class
        && last.emphasized == emphasized
    {
        last.text.push_str(text);
        return;
    }
    segments.push(Segment {
        text: text.to_owned(),
        class,
        emphasized,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row as `(kind sign, old number, new number, text with emphasized parts in brackets)`.
    fn summary(row: &DiffRow) -> (String, Option<u32>, Option<u32>, String) {
        match row {
            DiffRow::Folded { lines } => (format!("~{lines}"), None, None, String::new()),
            DiffRow::Line(line) => {
                let sign = match line.kind {
                    LineKind::Context => " ",
                    LineKind::Added => "+",
                    LineKind::Removed => "-",
                };
                let mut text = String::new();
                for segment in &line.segments {
                    if segment.emphasized {
                        text.push_str(&format!("[{}]", segment.text));
                    } else {
                        text.push_str(&segment.text);
                    }
                }
                (sign.to_owned(), line.old_number, line.new_number, text)
            }
        }
    }

    fn summaries(rows: &[DiffRow]) -> Vec<(String, Option<u32>, Option<u32>, String)> {
        rows.iter().map(summary).collect()
    }

    fn row(
        sign: &str,
        old: Option<u32>,
        new: Option<u32>,
        text: &str,
    ) -> (String, Option<u32>, Option<u32>, String) {
        (sign.to_owned(), old, new, text.to_owned())
    }

    #[test]
    fn an_added_line_comes_with_its_context() {
        let rows = diff_lines("one\ntwo\nthree\n", "one\ntwo\nnew\nthree\n", None, 1, 1);

        assert_eq!(
            summaries(&rows),
            vec![
                row(" ", Some(2), Some(2), "two"),
                row("+", None, Some(3), "new"),
                row(" ", Some(3), Some(4), "three"),
            ]
        );
    }

    #[test]
    fn a_removed_line_keeps_its_old_number() {
        let rows = diff_lines("one\ntwo\nthree", "one\nthree", None, 0, 1);

        assert_eq!(summaries(&rows), vec![row("-", Some(2), None, "two")]);
    }

    #[test]
    fn a_changed_word_is_the_only_emphasized_part() {
        let rows = diff_lines("let size = small;\n", "let size = large;\n", None, 3, 1);

        assert_eq!(
            summaries(&rows),
            vec![
                row("-", Some(1), None, "let size = [small;]"),
                row("+", None, Some(1), "let size = [large;]"),
            ]
        );
    }

    #[test]
    fn a_fully_different_line_has_no_emphasis() {
        let rows = diff_lines("alpha beta\n", "gamma delta\n", None, 3, 1);

        assert_eq!(
            summaries(&rows),
            vec![
                row("-", Some(1), None, "alpha beta"),
                row("+", None, Some(1), "gamma delta")
            ]
        );
    }

    #[test]
    fn distant_changes_fold_the_lines_between_them() {
        let old: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let new = old
            .replace("line 2\n", "line two\n")
            .replace("line 18\n", "line eighteen\n");

        let rows = diff_lines(&old, &new, None, 2, 1);

        let folded: Vec<u32> = rows
            .iter()
            .filter_map(|row| match row {
                DiffRow::Folded { lines } => Some(*lines),
                DiffRow::Line(_) => None,
            })
            .collect();
        // Lines 5 to 15 are more than 2 lines away from both changes.
        assert_eq!(folded, vec![11]);
        let first = rows.first().map(summary);
        assert_eq!(first, Some(row(" ", Some(1), Some(1), "line 1")));
        let last = rows.last().map(summary);
        assert_eq!(last, Some(row(" ", Some(20), Some(20), "line 20")));
    }

    #[test]
    fn a_created_file_is_all_added() {
        let rows = diff_lines("", "first\nsecond\n", None, 3, 1);

        assert_eq!(
            summaries(&rows),
            vec![
                row("+", None, Some(1), "first"),
                row("+", None, Some(2), "second")
            ]
        );
    }

    #[test]
    fn numbers_start_at_the_first_line() {
        let rows = diff_lines("keep\nold\n", "keep\nnew\n", None, 1, 40);

        assert_eq!(
            summaries(&rows),
            vec![
                row(" ", Some(40), Some(40), "keep"),
                row("-", Some(41), None, "old"),
                row("+", None, Some(41), "new"),
            ]
        );
    }

    #[test]
    fn equal_texts_give_no_row() {
        assert_eq!(
            diff_lines("same\ntext\n", "same\ntext", None, 3, 1),
            Vec::new()
        );
    }

    #[test]
    fn segments_carry_syntax_classes_and_emphasis() {
        let language = Language::from_tag("rust");

        let rows = diff_lines("let total = 1;\n", "let total = 22;\n", language, 0, 1);

        let Some(DiffRow::Line(added)) = rows.get(1) else {
            panic!("expected an added line in {rows:?}");
        };
        assert_eq!(added.kind, LineKind::Added);
        let keyword = added.segments.first().expect("a first segment");
        assert_eq!(
            (keyword.text.as_str(), keyword.class),
            ("let", TokenClass::Keyword)
        );
        assert!(
            added.segments.iter().any(|segment| segment.emphasized
                && segment.class == TokenClass::Number
                && segment.text == "22"),
            "{added:?}"
        );
    }

    #[test]
    fn whole_file_lines_are_context_with_colors() {
        let lines = plain_lines("fn main() {}\n\n", Language::from_tag("rust"), 1);

        assert_eq!(lines.len(), 2);
        let first = lines.first().expect("a first line");
        assert_eq!(first.kind, LineKind::Context);
        assert_eq!((first.old_number, first.new_number), (Some(1), Some(1)));
        assert_eq!(
            first.segments.first().map(|segment| segment.class),
            Some(TokenClass::Keyword)
        );
        assert_eq!(lines.get(1).map(|line| line.segments.len()), Some(0));
    }

    #[test]
    fn cuts_follow_both_spans_and_emphasis() {
        let spans = vec![
            Span {
                text: "abc".to_owned(),
                class: TokenClass::Keyword,
            },
            Span {
                text: "def".to_owned(),
                class: TokenClass::Plain,
            },
        ];

        let emphasized: Vec<Range<usize>> = vec![1..4, 5..6];

        let cut = segments(&spans, &emphasized);

        let parts: Vec<(&str, TokenClass, bool)> = cut
            .iter()
            .map(|segment| (segment.text.as_str(), segment.class, segment.emphasized))
            .collect();
        assert_eq!(
            parts,
            vec![
                ("a", TokenClass::Keyword, false),
                ("bc", TokenClass::Keyword, true),
                ("d", TokenClass::Plain, true),
                ("e", TokenClass::Plain, false),
                ("f", TokenClass::Plain, true),
            ]
        );
    }
}

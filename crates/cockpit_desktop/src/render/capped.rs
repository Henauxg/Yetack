//! The cap on the lines one view renders.

/// The most lines, or diff rows, one view renders, so that a huge file cannot freeze the window.
pub const MAX_RENDERED_LINES: usize = 5_000;

/// The first rows of a text, at most the number asked for, and how many lines follow them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capped<T> {
    pub rows: Vec<T>,
    /// The lines after `rows`, left out.
    pub more: usize,
}

/// Splits `text` after its first `max_lines` lines, as `str::lines` counts them: the text of
/// those lines, and how many lines follow.
pub fn split_lines(text: &str, max_lines: usize) -> (&str, usize) {
    let mut end = 0;
    let mut pieces = text.split_inclusive('\n');
    for piece in pieces.by_ref().take(max_lines) {
        end += piece.len();
    }
    (text.get(..end).unwrap_or(text), pieces.count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_splits_after_its_first_lines() {
        assert_eq!(
            split_lines("one\ntwo\r\nthree\nfour", 2),
            ("one\ntwo\r\n", 2)
        );
        assert_eq!(split_lines("one\ntwo\n", 2), ("one\ntwo\n", 0));
        assert_eq!(split_lines("one\ntwo\n", 5), ("one\ntwo\n", 0));
        assert_eq!(split_lines("", 5), ("", 0));
    }
}

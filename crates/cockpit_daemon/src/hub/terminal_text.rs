//! A terminal's kept output as plain text, for an agent that reads the terminal: the output goes
//! through a terminal parser, each part at the size the PTY had when the shell wrote it, and the
//! scrollback and the screen come out as lines.
//!
//! While a full-screen program runs, as `less` or `vim`, the text is that program's screen.

use std::num::NonZeroU16;

use cockpit_protocol::{MAX_TEXT_LINES, TerminalSize, TerminalSizeAt};

/// The rows the parser keeps above the screen. A line wrapped over several rows counts each.
const SCROLLBACK_ROWS: usize = MAX_TEXT_LINES as usize;

/// The lines of the kept output, oldest first, without trailing blanks or the blank lines under
/// the last one. A row the terminal wrapped joins the row after it into one line.
///
/// `output` starts at byte `start` of all the shell wrote, and `sizes` are the sizes the PTY took
/// over it, oldest first.
pub(super) fn lines(output: &[u8], start: u64, sizes: &[TerminalSizeAt]) -> Vec<String> {
    let first = sizes.first().map_or(DEFAULT_SIZE, |at| at.size);
    let (rows, cols) = dimensions(first);
    let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK_ROWS);
    let mut written = first_whole_line(output, start);
    for at in sizes.iter().skip(1) {
        let until = usize::try_from(at.offset.saturating_sub(start))
            .unwrap_or(usize::MAX)
            .min(output.len());
        if until > written {
            parser.process(output.get(written..until).unwrap_or_default());
            written = until;
        }
        let (rows, cols) = dimensions(at.size);
        parser.screen_mut().set_size(rows, cols);
    }
    parser.process(output.get(written..).unwrap_or_default());
    screen_lines(parser.screen_mut())
}

/// Where to start reading output that begins at byte `start` of all the shell wrote. Past byte 0,
/// the output was cut, maybe inside an escape sequence: it is read from the next line, or with no
/// line feed, from the next escape.
fn first_whole_line(output: &[u8], start: u64) -> usize {
    if start == 0 {
        return 0;
    }
    output
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|feed| feed + 1)
        .or_else(|| output.iter().position(|&byte| byte == ESCAPE))
        .unwrap_or(0)
}

const ESCAPE: u8 = 0x1b;

/// The largest screen the parser takes. A larger PTY is laid out at this size: a parser allocates
/// each cell of its screen.
const MAX_ROWS: u16 = 500;
const MAX_COLS: u16 = 1000;

/// The size of a terminal whose sizes are unknown, as a new PTY's.
const DEFAULT_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn dimensions(size: TerminalSize) -> (NonZeroU16, NonZeroU16) {
    (
        NonZeroU16::new(size.rows.min(MAX_ROWS)).unwrap_or(NonZeroU16::MIN),
        NonZeroU16::new(size.cols.min(MAX_COLS)).unwrap_or(NonZeroU16::MIN),
    )
}

/// The scrollback's rows, then the screen's, joined into lines. The parser shows the scrollback
/// only a screen at a time, so the window moves down it from the top.
fn screen_lines(screen: &mut vt100::Screen) -> Vec<String> {
    let rows = usize::from(screen.size().0.get());
    let mut wrapped_rows: Vec<(String, bool)> = Vec::new();
    screen.set_scrollback(usize::MAX);
    let mut above = screen.scrollback();
    while above > 0 {
        screen.set_scrollback(above);
        let taken = above.min(rows);
        wrapped_rows.extend(visible_rows(screen).take(taken));
        above -= taken;
    }
    screen.set_scrollback(0);
    wrapped_rows.extend(visible_rows(screen));
    let mut lines: Vec<String> = Vec::new();
    let mut continues = false;
    for (text, wrapped) in wrapped_rows {
        match lines.last_mut() {
            Some(last) if continues => last.push_str(&text),
            _ => lines.push(text),
        }
        continues = wrapped;
    }
    for line in &mut lines {
        line.truncate(line.trim_end().len());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// The rows on screen, at the current place in the scrollback, with whether each wraps into the
/// next one.
fn visible_rows(screen: &vt100::Screen) -> impl Iterator<Item = (String, bool)> + '_ {
    screen
        .rows(0, u16::MAX)
        .zip(0..)
        .map(|(text, row)| (text, screen.row_wrapped(row)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(offset: u64, cols: u16, rows: u16) -> TerminalSizeAt {
        TerminalSizeAt {
            offset,
            size: TerminalSize { cols, rows },
        }
    }

    #[test]
    fn colors_go_and_a_line_rewritten_after_a_return_keeps_its_last_text() {
        let output = b"\x1b[1;32mok\x1b[0m test a\r\n\
            Building 10%\rBuilding 99%\rDone        \r\n\
            \x1b[31merror\x1b[39m: \x1b[4mfailed\x1b[24m\r\n";

        let lines = lines(output, 0, &[size(0, 80, 24)]);

        assert_eq!(lines, ["ok test a", "Done", "error: failed"]);
    }

    #[test]
    fn cursor_moves_and_erases_land_where_the_shell_put_them() {
        // Writes two lines, goes back up to the first, and replaces its end.
        let output = b"first line\r\nsecond line\r\n\x1b[2A\x1b[6Cpart\x1b[K\x1b[2B\r$ ";

        let lines = lines(output, 0, &[size(0, 80, 24)]);

        assert_eq!(lines, ["first part", "second line", "$"]);
    }

    #[test]
    fn scrollback_comes_before_the_screen_and_wrapped_rows_join() {
        let mut output = Vec::new();
        for line in 1..=10 {
            output.extend_from_slice(format!("line {line}\r\n").as_bytes());
        }
        output.extend_from_slice(b"0123456789abcdef\r\n");

        let lines = lines(&output, 0, &[size(0, 8, 4)]);

        let mut expected: Vec<String> = (1..=10).map(|line| format!("line {line}")).collect();
        expected.push("0123456789abcdef".to_owned());
        assert_eq!(lines, expected);
    }

    /// The shell lays its output out for the PTY's size: a prompt drawn at column 70 of a wide
    /// terminal lands there only when that part goes in at the wide size.
    #[test]
    fn each_part_goes_in_at_the_size_it_was_written_at() {
        let narrow = b"ut off\r\nnarrow\r\n".as_slice();
        let wide = b"\x1b[70Gwide".as_slice();
        let output = [narrow, wide].concat();
        let resized = u64::try_from(narrow.len()).unwrap();

        let lines = lines(
            &output,
            100,
            &[size(90, 20, 5), size(100 + resized, 100, 5)],
        );

        assert_eq!(
            lines,
            ["narrow".to_owned(), format!("{}wide", " ".repeat(69))]
        );
    }

    /// A client may give the PTY up to 65,535 rows and columns, and the parser allocates each cell.
    #[test]
    fn the_parser_takes_at_most_a_large_screens_size() {
        let huge = TerminalSize {
            cols: u16::MAX,
            rows: u16::MAX,
        };

        let (rows, cols) = dimensions(huge);

        assert_eq!((rows.get(), cols.get()), (MAX_ROWS, MAX_COLS));
    }

    /// The kept output starts where the oldest bytes were dropped, which can be inside an escape
    /// sequence: the rest of it would show as text.
    #[test]
    fn output_cut_inside_an_escape_sequence_starts_at_the_next_line() {
        let output = b"8;5;196mred\x1b[0m end\r\nnext line\r\n";
        let no_line_feed = b";1H\x1b[2;1Hdrawn";

        let cut = lines(output, 4096, &[size(0, 80, 24)]);
        let whole = lines(output, 0, &[size(0, 80, 24)]);
        let drawn = lines(no_line_feed, 4096, &[size(0, 80, 24)]);

        assert_eq!(cut, ["next line"]);
        assert_eq!(whole, ["8;5;196mred end", "next line"]);
        assert_eq!(drawn, ["", "drawn"]);
    }

    #[test]
    fn no_output_gives_no_line() {
        assert!(lines(b"", 0, &[]).is_empty());
        assert!(lines(b"\r\n\r\n   \r\n", 0, &[size(0, 80, 24)]).is_empty());
    }
}

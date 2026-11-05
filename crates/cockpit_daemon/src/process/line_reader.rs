use std::io;

use tokio::io::AsyncBufRead;

const NEWLINE: u8 = b'\n';

/// Reads lines up to a maximum length. A longer line is skipped, not buffered.
pub(crate) struct LineReader<R> {
    reader: R,
    line: Vec<u8>,
    max_bytes: usize,
    /// The bytes of a line over the maximum read so far; the rest is skipped.
    skipped: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadLine {
    /// Without its `\n`. Invalid UTF-8 is replaced.
    Line(String),
    TooLong {
        bytes: usize,
    },
}

impl<R: AsyncBufRead + Unpin> LineReader<R> {
    pub(crate) fn new(reader: R, max_bytes: usize) -> Self {
        Self {
            reader,
            line: Vec::new(),
            max_bytes,
            skipped: None,
        }
    }

    /// The next line, or `None` at the end of the stream. A last line without `\n` still counts,
    /// and an empty line is skipped.
    ///
    /// It is cancel safe: what was read stays for the next call.
    pub(crate) async fn next(&mut self) -> io::Result<Option<ReadLine>> {
        use tokio::io::AsyncBufReadExt;

        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                return Ok(self.finish_line());
            }
            let (segment, complete) = match available.iter().position(|&byte| byte == NEWLINE) {
                Some(end) => (available.get(..end).unwrap_or_default(), true),
                None => (available, false),
            };
            let consumed = segment.len() + usize::from(complete);
            match &mut self.skipped {
                Some(skipped) => *skipped = skipped.saturating_add(segment.len()),
                None if self.line.len() + segment.len() > self.max_bytes => {
                    self.skipped = Some(self.line.len() + segment.len());
                    self.line.clear();
                }
                None => self.line.extend_from_slice(segment),
            }
            self.reader.consume(consumed);
            if complete && let Some(line) = self.finish_line() {
                return Ok(Some(line));
            }
        }
    }

    fn finish_line(&mut self) -> Option<ReadLine> {
        if let Some(bytes) = self.skipped.take() {
            return Some(ReadLine::TooLong { bytes });
        }
        if self.line.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        Some(ReadLine::Line(line))
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    async fn read_all(input: &[u8], max_bytes: usize, buffer: usize) -> Vec<ReadLine> {
        let mut reader = LineReader::new(BufReader::with_capacity(buffer, input), max_bytes);
        let mut lines = Vec::new();
        while let Some(line) = reader.next().await.unwrap() {
            lines.push(line);
        }
        lines
    }

    fn line(text: &str) -> ReadLine {
        ReadLine::Line(text.to_owned())
    }

    #[tokio::test]
    async fn reads_lines_split_across_reads_and_a_last_line_without_newline() {
        let lines = read_all(b"first\nsecond\nlast", 100, 3).await;

        assert_eq!(lines, [line("first"), line("second"), line("last")]);
    }

    #[tokio::test]
    async fn an_empty_line_does_not_end_the_stream() {
        let lines = read_all(b"first\n\nsecond\n\n", 100, 3).await;

        assert_eq!(lines, [line("first"), line("second")]);
    }

    #[tokio::test]
    async fn skips_a_long_line_and_goes_on() {
        let lines = read_all(b"short\nthis line is too long\nok\n", 8, 4).await;

        assert_eq!(
            lines,
            [line("short"), ReadLine::TooLong { bytes: 21 }, line("ok")]
        );
    }
}

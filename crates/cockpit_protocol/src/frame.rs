//! Line framing: one JSON object per line, UTF-8, ending in `\n`.

use std::io;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncWrite};

use crate::ids::RequestId;

/// The longest line, without its `\n`, that a reader accepts and a writer sends.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

const NEWLINE: u8 = b'\n';

/// The buffer a reader keeps between lines, so one large line does not hold its memory.
const KEPT_CAPACITY: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("the connection failed")]
    Io(#[from] io::Error),
    #[error("a line is longer than {max} bytes")]
    LineTooLong { max: usize },
    #[error("the stream ended in the middle of a line")]
    Truncated,
    /// The reader goes on with the next line after this error.
    #[error("a line is not a valid message")]
    Decode {
        /// The line's top-level `id`, when it has one, so that a daemon can answer the request
        /// with an error and a client can fail it.
        request_id: Option<RequestId>,
        #[source]
        source: serde_json::Error,
    },
    #[error("a message cannot be encoded")]
    Encode(#[source] serde_json::Error),
}

/// One encoded message, ready to write.
///
/// Encoding once lets the daemon send the same event to every client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame(Vec<u8>);

impl Frame {
    /// Refuses a message longer than `MAX_LINE_BYTES`, which a reader would refuse.
    pub fn encode<T: Serialize>(message: &T) -> Result<Self, FrameError> {
        // serde_json escapes the newlines inside strings, so the message is one line.
        let mut bytes = serde_json::to_vec(message).map_err(FrameError::Encode)?;
        if bytes.len() > MAX_LINE_BYTES {
            return Err(FrameError::LineTooLong {
                max: MAX_LINE_BYTES,
            });
        }
        bytes.push(NEWLINE);
        Ok(Self(bytes))
    }

    /// The line, with its `\n`.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub struct FrameReader<R> {
    reader: R,
    line: Vec<u8>,
    max_line_bytes: usize,
}

impl<R: AsyncBufRead + Unpin> FrameReader<R> {
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self::with_max_line_bytes(reader, MAX_LINE_BYTES)
    }

    fn with_max_line_bytes(reader: R, max_line_bytes: usize) -> Self {
        Self {
            reader,
            line: Vec::new(),
            max_line_bytes,
        }
    }

    /// Reads the next message, or `None` when the stream ends between two lines.
    ///
    /// It is cancel safe: dropping the future keeps the part of the line read so far for the next
    /// call. After `FrameError::Decode` the reader can go on; after any other error the stream is
    /// unusable.
    pub async fn next_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        if !self.read_line().await? {
            return Ok(None);
        }
        let message = match serde_json::from_slice(&self.line) {
            Ok(message) => Ok(Some(message)),
            Err(source) => Err(FrameError::Decode {
                request_id: probe_request_id(&self.line),
                source,
            }),
        };
        self.line.clear();
        self.line.shrink_to(KEPT_CAPACITY);
        message
    }

    /// Reads one line into `self.line`, without its `\n`. Returns `false` at the end of the
    /// stream.
    async fn read_line(&mut self) -> Result<bool, FrameError> {
        use tokio::io::AsyncBufReadExt;

        loop {
            let available = self.reader.fill_buf().await?;
            let Some(segment) = available.split_inclusive(|&byte| byte == NEWLINE).next() else {
                return if self.line.is_empty() {
                    Ok(false)
                } else {
                    Err(FrameError::Truncated)
                };
            };
            let (content, complete) = match segment.strip_suffix(&[NEWLINE]) {
                Some(content) => (content, true),
                None => (segment, false),
            };
            if self.line.len() + content.len() > self.max_line_bytes {
                return Err(FrameError::LineTooLong {
                    max: self.max_line_bytes,
                });
            }
            self.line.extend_from_slice(content);
            let consumed = segment.len();
            self.reader.consume(consumed);
            if complete {
                return Ok(true);
            }
        }
    }
}

#[derive(Deserialize)]
struct RequestIdProbe {
    id: Option<RequestId>,
}

fn probe_request_id(line: &[u8]) -> Option<RequestId> {
    serde_json::from_slice::<RequestIdProbe>(line).ok()?.id
}

pub struct FrameWriter<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), FrameError> {
        let frame = Frame::encode(message)?;
        self.send_frame(&frame).await
    }

    /// Writes the line and flushes it.
    pub async fn send_frame(&mut self, frame: &Frame) -> Result<(), FrameError> {
        use tokio::io::AsyncWriteExt;

        self.writer.write_all(frame.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::BufReader;

    use super::*;
    use crate::ids::{AgentId, Seq};
    use crate::request::{AgentParams, Method, Request};
    use crate::session::ClientMessage;

    fn hello(last_seq: u64) -> ClientMessage {
        ClientMessage::Hello {
            protocol_version: 1,
            last_seq: Some(Seq::new(last_seq)),
        }
    }

    /// A JSON string of exactly `len` bytes, quotes included.
    fn json_string_line(len: usize) -> Vec<u8> {
        let mut line = vec![b'"'];
        line.resize(len - 1, b'a');
        line.push(b'"');
        line
    }

    #[tokio::test]
    async fn reads_one_message_per_line_then_none() {
        let input = b"{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":3}\n{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":4}\n";
        let mut reader = FrameReader::new(&input[..]);

        let first: Option<ClientMessage> = reader.next_message().await.unwrap();
        let second: Option<ClientMessage> = reader.next_message().await.unwrap();
        let end: Option<ClientMessage> = reader.next_message().await.unwrap();

        assert_eq!(first, Some(hello(3)));
        assert_eq!(second, Some(hello(4)));
        assert_eq!(end, None);
    }

    #[tokio::test]
    async fn reads_a_line_split_across_reads() {
        let input = b"{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":3}\n";
        // A two-byte buffer makes every fill_buf return a piece of the line.
        let mut reader = FrameReader::new(BufReader::with_capacity(2, &input[..]));

        let message: Option<ClientMessage> = reader.next_message().await.unwrap();

        assert_eq!(message, Some(hello(3)));
    }

    #[tokio::test]
    async fn a_cancelled_read_keeps_the_part_of_the_line_read_so_far() {
        use tokio::io::AsyncWriteExt;

        let (mut sender, received) = tokio::io::duplex(1024);
        let mut reader = FrameReader::new(BufReader::new(received));
        sender
            .write_all(b"{\"type\":\"hello\",\"protocol_version\":1,")
            .await
            .unwrap();

        // A zero timeout polls the read once, then drops it.
        let cancelled =
            tokio::time::timeout(Duration::ZERO, reader.next_message::<ClientMessage>()).await;
        assert!(cancelled.is_err(), "{cancelled:?}");
        assert!(!reader.line.is_empty(), "the cancelled read took no bytes");
        sender.write_all(b"\"last_seq\":3}\n").await.unwrap();
        let message: Option<ClientMessage> = reader.next_message().await.unwrap();

        assert_eq!(message, Some(hello(3)));
    }

    #[tokio::test]
    async fn accepts_a_line_of_exactly_the_maximum() {
        let mut input = json_string_line(MAX_LINE_BYTES);
        input.push(NEWLINE);
        let mut reader = FrameReader::new(&input[..]);

        let text: Option<String> = reader.next_message().await.unwrap();

        assert_eq!(text.map(|text| text.len()), Some(MAX_LINE_BYTES - 2));
    }

    #[tokio::test]
    async fn refuses_a_line_longer_than_the_maximum() {
        let mut input = json_string_line(MAX_LINE_BYTES + 1);
        input.push(NEWLINE);
        let mut reader = FrameReader::new(&input[..]);

        let result = reader.next_message::<String>().await;

        assert!(
            matches!(
                result,
                Err(FrameError::LineTooLong {
                    max: MAX_LINE_BYTES
                })
            ),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn refuses_a_long_line_before_reading_all_of_it() {
        // No newline ever comes: the reader stops at the limit instead of buffering forever.
        let input = [b'a'; 64];
        let mut reader = FrameReader::with_max_line_bytes(&input[..], 10);

        let result = reader.next_message::<String>().await;

        assert!(
            matches!(result, Err(FrameError::LineTooLong { max: 10 })),
            "{result:?}"
        );
        assert!(reader.line.len() <= 10);
    }

    #[tokio::test]
    async fn a_truncated_last_line_is_an_error() {
        let input = b"{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":3}\n{\"type\":\"hel";
        let mut reader = FrameReader::new(&input[..]);

        let first: Option<ClientMessage> = reader.next_message().await.unwrap();
        let second = reader.next_message::<ClientMessage>().await;

        assert_eq!(first, Some(hello(3)));
        assert!(matches!(second, Err(FrameError::Truncated)), "{second:?}");
    }

    #[tokio::test]
    async fn invalid_json_is_a_decode_error_and_the_reader_goes_on() {
        let input = b"not json\n{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":3}\n";
        let mut reader = FrameReader::new(&input[..]);

        let first = reader.next_message::<ClientMessage>().await;
        let second: Option<ClientMessage> = reader.next_message().await.unwrap();

        assert!(
            matches!(
                first,
                Err(FrameError::Decode {
                    request_id: None,
                    ..
                })
            ),
            "{first:?}"
        );
        assert_eq!(second, Some(hello(3)));
    }

    #[tokio::test]
    async fn a_request_that_does_not_decode_keeps_its_id() {
        let input =
            b"{\"type\":\"request\",\"id\":7,\"request\":{\"method\":\"agent.fly\",\"params\":{}}}\n";
        let mut reader = FrameReader::new(&input[..]);

        let result = reader.next_message::<ClientMessage>().await;

        let Err(FrameError::Decode { request_id, source }) = result else {
            panic!("expected a decode error, got {result:?}");
        };
        assert_eq!(request_id, Some(RequestId::new(7)));
        // Clients tell an older daemon by the start of the error it answers with.
        assert!(
            source
                .to_string()
                .starts_with(&Method::unknown_message("agent.fly")),
            "{source}"
        );
    }

    #[tokio::test]
    async fn writes_one_line_per_message() {
        let mut writer = FrameWriter::new(Vec::new());

        writer.send(&hello(3)).await.unwrap();
        let interrupt = ClientMessage::Request {
            id: RequestId::new(1),
            request: Request::AgentInterrupt(AgentParams {
                agent: AgentId::new(2),
            }),
        };
        writer.send(&interrupt).await.unwrap();

        let written = String::from_utf8(writer.writer).unwrap();
        assert_eq!(
            written,
            "{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":3}\n{\"type\":\"request\",\"id\":1,\"request\":{\"method\":\"agent.interrupt\",\"params\":{\"agent\":2}}}\n"
        );
    }

    #[tokio::test]
    async fn a_newline_inside_a_string_stays_on_one_line() {
        let mut writer = FrameWriter::new(Vec::new());

        writer.send(&"two\nlines").await.unwrap();

        assert_eq!(writer.writer, b"\"two\\nlines\"\n");
    }

    #[test]
    fn encode_refuses_a_message_longer_than_the_maximum() {
        let text = "a".repeat(MAX_LINE_BYTES - 1);

        let result = Frame::encode(&text);

        assert!(
            matches!(
                result,
                Err(FrameError::LineTooLong {
                    max: MAX_LINE_BYTES
                })
            ),
            "{result:?}"
        );
    }
}

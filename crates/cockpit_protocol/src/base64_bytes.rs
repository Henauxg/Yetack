//! Bytes that a message carries as base64 text, such as an image's or a PDF's.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// Bytes in standard base64 with padding, as JSON carries them. Neither reading a message nor
/// [`Base64::new`] checks the text: [`Base64::decode`] does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Base64(String);

/// The text is not standard base64 with padding.
#[derive(Debug, thiserror::Error)]
#[error("the data is not base64: {0}")]
pub struct NotBase64(#[from] base64::DecodeError);

impl Base64 {
    /// Text that another program wrote as base64, such as Claude Code or the page.
    #[must_use]
    pub fn new(encoded: String) -> Self {
        Self(encoded)
    }

    #[must_use]
    pub fn encode(bytes: &[u8]) -> Self {
        Self(STANDARD.encode(bytes))
    }

    pub fn decode(&self) -> Result<Vec<u8>, NotBase64> {
        Ok(STANDARD.decode(&self.0)?)
    }

    /// The size of the bytes the text encodes, without decoding it: right for valid base64.
    #[must_use]
    pub fn decoded_len(&self) -> usize {
        let padding = self
            .0
            .bytes()
            .rev()
            .take_while(|&byte| byte == b'=')
            .count();
        (self.0.len() / 4 * 3).saturating_sub(padding)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_come_back_from_their_text() {
        let encoded = Base64::encode(b"%PDF-\xe2");

        assert_eq!(encoded.as_str(), "JVBERi3i");
        assert_eq!(encoded.decode().unwrap(), b"%PDF-\xe2");
    }

    #[test]
    fn text_that_is_not_base64_does_not_decode() {
        for text in ["not base64!", "JVBERi0", "JVB=ERi0"] {
            let decoded = Base64::new(text.to_owned()).decode();
            assert!(decoded.is_err(), "{text}: {decoded:?}");
        }
    }

    #[test]
    fn the_decoded_size_leaves_out_the_padding() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"abcd"] {
            assert_eq!(
                Base64::encode(bytes).decoded_len(),
                bytes.len(),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn the_text_travels_as_a_plain_json_string() {
        let encoded = Base64::encode(b"hello");

        let json = serde_json::to_string(&encoded).unwrap();

        assert_eq!(json, r#""aGVsbG8=""#);
        assert_eq!(serde_json::from_str::<Base64>(&json).unwrap(), encoded);
    }
}

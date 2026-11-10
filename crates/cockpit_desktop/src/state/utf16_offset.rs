//! A caret position as the page counts it.

use serde::{Deserialize, Serialize};

/// A position in a text, in UTF-16 code units, as the webview reports a caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Utf16Offset(usize);

impl Utf16Offset {
    #[cfg(test)]
    pub fn new(units: usize) -> Self {
        Self(units)
    }

    /// The end of `text`.
    pub fn end_of(text: &str) -> Self {
        Self(text.encode_utf16().count())
    }

    /// The byte offset in `text` of this position. A position inside a character, between the
    /// two halves of a surrogate pair, gives the end of that character. A position past the end
    /// of `text` gives its end.
    pub fn byte_offset(self, text: &str) -> usize {
        let mut units = 0;
        for (offset, character) in text.char_indices() {
            if units >= self.0 {
                return offset;
            }
            units += character.len_utf16();
        }
        text.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_outside_the_bmp_is_two_units_and_four_bytes() {
        let text = "a🙂b";

        let offsets: Vec<usize> = (0..=5)
            .map(|units| Utf16Offset::new(units).byte_offset(text))
            .collect();

        assert_eq!(offsets, [0, 1, 5, 5, 6, 6]);
        assert_eq!(Utf16Offset::end_of(text), Utf16Offset::new(4));
    }

    #[test]
    fn it_reads_and_writes_as_a_plain_number() {
        let offset: Utf16Offset = serde_json::from_str("15").unwrap();

        assert_eq!(offset, Utf16Offset::new(15));
        assert_eq!(serde_json::to_string(&offset).unwrap(), "15");
    }
}

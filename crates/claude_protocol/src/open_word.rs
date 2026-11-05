use std::convert::Infallible;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A word from a set Claude Code may extend, such as a status or a mode.
///
/// A word this crate does not know yet keeps its text, so a new Claude Code version still parses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpenWord<W> {
    Known(W),
    Unknown(String),
}

impl<W: Copy + Into<&'static str>> OpenWord<W> {
    /// The word as Claude Code writes it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(word) => (*word).into(),
            Self::Unknown(text) => text,
        }
    }
}

impl<W: Copy + Into<&'static str>> Serialize for OpenWord<W> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<W: FromStr> FromStr for OpenWord<W> {
    type Err = Infallible;

    fn from_str(text: &str) -> Result<Self, Infallible> {
        let parsed: Result<W, W::Err> = text.parse();
        let word = match parsed {
            Ok(word) => Self::Known(word),
            Err(_) => Self::Unknown(text.to_owned()),
        };
        Ok(word)
    }
}

impl<'de, W: FromStr> Deserialize<'de> for OpenWord<W> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let Ok(word) = text.parse::<Self>();
        Ok(word)
    }
}

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::ids::ToolUseId;
use crate::wire;

/// A block of a message's content, read from Claude Code or written to it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextBlock),
    Thinking(ThinkingBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    Image(ImageBlock),
    /// A block of a type this crate does not know, as Claude Code wrote it.
    #[serde(untagged)]
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
}

/// Claude's reasoning. Claude Code may send it empty when it only shows that Claude thinks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub thinking: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: ToolUseId,
    pub name: String,
    /// Read it with [`ToolInput::parse`](crate::ToolInput::parse) for the tools this crate knows.
    #[serde(default)]
    pub input: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: ToolUseId,
    /// Text content is read as one text block.
    #[serde(default, deserialize_with = "text_or_blocks")]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageBlock {
    pub source: ImageSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64(Base64Image),
    Url(UrlImage),
    /// A source of a type this crate does not know, as Claude Code wrote it.
    #[serde(untagged)]
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base64Image {
    /// Such as `image/png`.
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlImage {
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum BlockType {
    Text,
    Thinking,
    ToolUse,
    ToolResult,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum ImageSourceType {
    Base64,
    Url,
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(block_type) = wire::word::<BlockType>(&value, wire::TYPE) else {
            return Ok(Self::Unknown(value));
        };
        let block = match block_type {
            BlockType::Text => TextBlock::deserialize(&value).map(Self::Text),
            BlockType::Thinking => ThinkingBlock::deserialize(&value).map(Self::Thinking),
            BlockType::ToolUse => ToolUseBlock::deserialize(&value).map(Self::ToolUse),
            BlockType::ToolResult => ToolResultBlock::deserialize(&value).map(Self::ToolResult),
            BlockType::Image => ImageBlock::deserialize(&value).map(Self::Image),
        };
        block.map_err(D::Error::custom)
    }
}

impl<'de> Deserialize<'de> for ImageSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(source_type) = wire::word::<ImageSourceType>(&value, wire::TYPE) else {
            return Ok(Self::Unknown(value));
        };
        let source = match source_type {
            ImageSourceType::Base64 => Base64Image::deserialize(&value).map(Self::Base64),
            ImageSourceType::Url => UrlImage::deserialize(&value).map(Self::Url),
        };
        source.map_err(D::Error::custom)
    }
}

/// Reads message content, which Claude Code sends as a string or as a list of blocks, as blocks.
pub(crate) fn text_or_blocks<'de, D>(deserializer: D) -> Result<Vec<ContentBlock>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(vec![ContentBlock::Text(TextBlock { text })]),
        blocks => Vec::deserialize(blocks).map_err(D::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_types_are_pinned() {
        let words: Vec<&str> = BlockType::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();
        assert_eq!(
            words,
            ["text", "thinking", "tool_use", "tool_result", "image"]
        );
    }

    #[test]
    fn image_source_types_are_pinned() {
        let words: Vec<&str> = ImageSourceType::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();
        assert_eq!(words, ["base64", "url"]);
    }
}

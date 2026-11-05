//! A client's prompt, as the `user` line Claude Code reads.

use claude_protocol::{Base64Image, ContentBlock, ImageBlock, ImageSource, InputLine, TextBlock};
use cockpit_protocol::{Effort, Prompt};

use crate::transcript::words;

const EFFORT_COMMAND: &str = "/effort";
/// Names the Claude session, for Claude Code's resume list and `sessions.list`.
const RENAME_COMMAND: &str = "/rename";

/// Text alone, or the images followed by the text. `None` when an image's type has no word, which
/// the protocol's own types rule out.
pub(crate) fn prompt_line(prompt: &Prompt) -> Option<InputLine> {
    if prompt.images.is_empty() {
        return Some(InputLine::prompt(prompt.text.clone()));
    }
    let mut blocks = Vec::with_capacity(prompt.images.len() + 1);
    for image in &prompt.images {
        blocks.push(ContentBlock::Image(ImageBlock {
            source: ImageSource::Base64(Base64Image {
                media_type: words::media_type_word(image.media_type)?,
                data: image.data.clone(),
            }),
        }));
    }
    if !prompt.text.is_empty() {
        blocks.push(ContentBlock::Text(TextBlock {
            text: prompt.text.clone(),
        }));
    }
    Some(InputLine::prompt_blocks(blocks))
}

/// `/rename <title>`: naming a session has no control request.
pub(crate) fn rename_command(title: &str) -> String {
    format!("{RENAME_COMMAND} {title}")
}

/// `/effort <level>`: effort has no control request.
pub(crate) fn effort_command(effort: Effort) -> Option<String> {
    Some(format!("{EFFORT_COMMAND} {}", words::effort_word(effort)?))
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Image, ImageMediaType};
    use serde_json::json;

    use super::*;

    #[test]
    fn a_prompt_with_an_image_sends_the_image_first() {
        let prompt = Prompt {
            text: "What is wrong here?".to_owned(),
            images: vec![Image {
                media_type: ImageMediaType::Png,
                data: "aGVsbG8=".to_owned(),
            }],
        };

        let line = prompt_line(&prompt).unwrap();

        assert_eq!(
            serde_json::to_value(&line).unwrap(),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "aGVsbG8="
                }},
                {"type": "text", "text": "What is wrong here?"}
            ]}})
        );
    }

    #[test]
    fn a_text_prompt_is_a_string() {
        let prompt = Prompt {
            text: "Hello".to_owned(),
            images: Vec::new(),
        };

        let line = prompt_line(&prompt).unwrap();

        assert_eq!(
            serde_json::to_value(&line).unwrap(),
            json!({"type": "user", "message": {"role": "user", "content": "Hello"}})
        );
        assert_eq!(
            effort_command(Effort::High).as_deref(),
            Some("/effort high")
        );
    }
}

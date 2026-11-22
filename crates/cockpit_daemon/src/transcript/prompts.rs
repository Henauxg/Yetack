//! A client's prompt, as the `user` line Claude Code reads.

use claude_protocol::{Base64Image, ContentBlock, ImageBlock, ImageSource, InputLine, TextBlock};
use cockpit_protocol::{Effort, Prompt};
use strum::{EnumString, IntoStaticStr, VariantArray};

use crate::transcript::words;

/// Names the Claude session, for Claude Code's resume list and `sessions.list`.
const RENAME_COMMAND: &str = "/rename";

/// The slash commands that change a setting the daemon keeps for the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
enum SettingCommand {
    #[strum(serialize = "/effort")]
    Effort,
    #[strum(serialize = "/model")]
    Model,
}

/// A setting that a prompt typed in the composer changes, as the agent's menus do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TypedSetting {
    Effort(Effort),
    Model(String),
}

impl TypedSetting {
    /// Reads `/effort <level>` and `/model <model>`. `None` for any other text, and for a command
    /// without its word, with a level the cockpit does not know, or followed by more words.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split_whitespace();
        let command: SettingCommand = parts.next()?.parse().ok()?;
        let word = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        match command {
            SettingCommand::Effort => words::effort_from_word(word).map(Self::Effort),
            SettingCommand::Model => Some(Self::Model(word.to_owned())),
        }
    }
}

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
                data: image.data.as_str().to_owned(),
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
    let command: &str = SettingCommand::Effort.into();
    Some(format!("{command} {}", words::effort_word(effort)?))
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Base64, Image, ImageMediaType};
    use serde_json::json;

    use super::*;

    #[test]
    fn a_prompt_with_an_image_sends_the_image_first() {
        let prompt = Prompt {
            text: "What is wrong here?".to_owned(),
            images: vec![Image {
                media_type: ImageMediaType::Png,
                data: Base64::new("aGVsbG8=".to_owned()),
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

    #[test]
    fn setting_commands_are_pinned() {
        let commands: Vec<&str> = SettingCommand::VARIANTS
            .iter()
            .map(|&command| command.into())
            .collect();

        assert_eq!(commands, ["/effort", "/model"]);
    }

    #[test]
    fn a_typed_effort_needs_one_known_level() {
        let read = |text: &str| TypedSetting::parse(text);

        assert_eq!(
            read(" /effort  high "),
            Some(TypedSetting::Effort(Effort::High))
        );
        assert_eq!(
            read("/effort xhigh"),
            Some(TypedSetting::Effort(Effort::Xhigh))
        );
        assert_eq!(read("/effort"), None);
        assert_eq!(read("/effort turbo"), None);
        assert_eq!(read("/effort high please"), None);
        assert_eq!(read("/efforts high"), None);
        assert_eq!(read("Set /effort high"), None);
    }

    #[test]
    fn a_typed_model_needs_one_name() {
        let read = |text: &str| TypedSetting::parse(text);

        assert_eq!(
            read("/model other-model"),
            Some(TypedSetting::Model("other-model".to_owned()))
        );
        assert_eq!(read("/model"), None);
        assert_eq!(read("/model other model"), None);
    }
}

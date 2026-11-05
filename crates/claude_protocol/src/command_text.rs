use std::fmt;

use strum::{Display, IntoStaticStr, VariantArray};

use crate::tagged;

/// How a slash command starts.
const COMMAND_PREFIX: &str = "/";

/// A slash command the user typed, as Claude Code records the prompt that ran it, with a
/// `command-message`, a `command-name` and a `command-args` element.
///
/// Its [`Display`](fmt::Display) is the command as the user typed it:
///
/// ```
/// use claude_protocol::TypedCommand;
///
/// let text = "<command-message>greet</command-message>\n\
///             <command-name>/greet</command-name>\n<command-args>world</command-args>";
/// let command = TypedCommand::parse(text);
/// assert_eq!(command.map(|command| command.to_string()).as_deref(), Some("/greet world"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedCommand {
    /// With its slash, such as `/greet`.
    pub name: String,
    /// Empty for a command without arguments.
    pub args: String,
}

/// The elements of a command's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "kebab-case")]
enum Tag {
    CommandMessage,
    CommandName,
    CommandArgs,
    LocalCommandStdout,
}

impl TypedCommand {
    /// The command whose prompt `text` is, or `None` for text that does not start with a
    /// command's element.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_start();
        let starts = [Tag::CommandMessage, Tag::CommandName]
            .into_iter()
            .any(|tag| tagged::starts_with(text, tag.into()));
        if !starts {
            return None;
        }
        let (name, _) = tagged::element(text, Tag::CommandName.into())?;
        let name = name.trim();
        let name = if name.starts_with(COMMAND_PREFIX) {
            name.to_owned()
        } else {
            format!("{COMMAND_PREFIX}{name}")
        };
        let args = tagged::element(text, Tag::CommandArgs.into()).map_or("", |(args, _)| args);
        Some(Self {
            name,
            args: args.trim().to_owned(),
        })
    }
}

impl fmt::Display for TypedCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.args.is_empty() {
            return formatter.write_str(&self.name);
        }
        write!(formatter, "{} {}", self.name, self.args)
    }
}

/// The output of a local command, such as `/effort`, when `text` is Claude Code's record of it:
/// the content of a `<local-command-stdout>` element, which may be empty.
#[must_use]
pub fn local_command_output(text: &str) -> Option<&str> {
    let text = text.trim_start();
    let tag: &str = Tag::LocalCommandStdout.into();
    if !tagged::starts_with(text, tag) {
        return None;
    }
    tagged::element(text, tag).map(|(output, _)| output.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_pinned() {
        let tags: Vec<&str> = Tag::VARIANTS.iter().map(|&tag| tag.into()).collect();
        assert_eq!(
            tags,
            [
                "command-message",
                "command-name",
                "command-args",
                "local-command-stdout"
            ]
        );
    }

    #[test]
    fn reads_a_command_in_either_order_with_or_without_arguments() {
        let local = "<command-name>/effort</command-name>\n\
            <command-message>effort</command-message>\n<command-args>high</command-args>";
        let skill =
            "<command-message>review</command-message>\n<command-name>/review</command-name>";

        let texts: Vec<Option<String>> = [local, skill]
            .into_iter()
            .map(|text| TypedCommand::parse(text).map(|command| command.to_string()))
            .collect();

        assert_eq!(
            texts,
            [Some("/effort high".to_owned()), Some("/review".to_owned())]
        );
    }

    #[test]
    fn a_prompt_that_only_mentions_the_tags_is_no_command() {
        let prompt = "Why does <command-name>/x</command-name> show in the transcript?";

        assert_eq!(TypedCommand::parse(prompt), None);
        assert_eq!(local_command_output(prompt), None);
    }

    #[test]
    fn reads_a_local_commands_output() {
        let output = "<local-command-stdout>Set model to the default model</local-command-stdout>";

        assert_eq!(
            local_command_output(output),
            Some("Set model to the default model")
        );
        assert_eq!(
            local_command_output("<local-command-stdout></local-command-stdout>"),
            Some("")
        );
    }
}

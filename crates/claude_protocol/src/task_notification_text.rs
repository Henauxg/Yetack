use strum::{Display, IntoStaticStr, VariantArray};

use crate::ids::ToolUseId;
use crate::open_word::OpenWord;
use crate::system_event::TaskStatus;
use crate::tagged;

/// A `<task-notification>` element in the text of a `user` line whose origin is
/// [`OriginKind::TaskNotification`](crate::OriginKind::TaskNotification): Claude Code tells
/// Claude that a background task ended.
///
/// ```
/// use claude_protocol::{OpenWord, TaskNotificationText, TaskStatus};
///
/// let text = "<task-notification>\n<task-id>b1</task-id>\n<status>failed</status>\n\
///             <summary>Background command \"make\" failed</summary>\n</task-notification>";
/// assert_eq!(
///     TaskNotificationText::parse_all(text),
///     [TaskNotificationText {
///         status: Some(OpenWord::Known(TaskStatus::Failed)),
///         summary: Some("Background command \"make\" failed".to_owned()),
///         tool_use_id: None,
///     }],
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskNotificationText {
    pub status: Option<OpenWord<TaskStatus>>,
    /// Claude Code's sentence, such as `Task "Run the tests" was stopped by the user`.
    pub summary: Option<String>,
    /// The tool call that started the task, such as the `Agent` call of a background subagent.
    pub tool_use_id: Option<ToolUseId>,
}

/// The elements read from a task notification's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "kebab-case")]
enum Tag {
    TaskNotification,
    Status,
    Summary,
    ToolUseId,
}

impl TaskNotificationText {
    /// Every `<task-notification>` element in `text`, in order. An element without a closing tag
    /// is left out.
    #[must_use]
    pub fn parse_all(text: &str) -> Vec<Self> {
        let mut notifications = Vec::new();
        let mut rest = text;
        while let Some((inner, after)) = tagged::element(rest, Tag::TaskNotification.into()) {
            let content = |tag: Tag| {
                tagged::element(inner, tag.into())
                    .map(|(content, _)| content.trim())
                    .filter(|content| !content.is_empty())
            };
            notifications.push(Self {
                status: content(Tag::Status).map(|status| {
                    let Ok(status) = status.parse::<OpenWord<TaskStatus>>();
                    status
                }),
                summary: content(Tag::Summary).map(str::to_owned),
                tool_use_id: content(Tag::ToolUseId).map(|id| ToolUseId::new(id.to_owned())),
            });
            rest = after;
        }
        notifications
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_pinned() {
        let tags: Vec<&str> = Tag::VARIANTS.iter().map(|&tag| tag.into()).collect();
        assert_eq!(
            tags,
            ["task-notification", "status", "summary", "tool-use-id"]
        );
    }

    #[test]
    fn reads_each_notification_and_skips_what_does_not_close() {
        let text = "Two tasks ended.\n\
            <task-notification><tool-use-id>toolu_7</tool-use-id><status>killed</status>\
            <summary> Task \"Watch\" was stopped </summary></task-notification>\n\
            <task-notification><status>archived</status><summary></summary></task-notification>\n\
            <task-notification><status>completed</status>";

        let notifications = TaskNotificationText::parse_all(text);

        assert_eq!(
            notifications,
            [
                TaskNotificationText {
                    status: Some(OpenWord::Known(TaskStatus::Killed)),
                    summary: Some("Task \"Watch\" was stopped".to_owned()),
                    tool_use_id: Some(ToolUseId::new("toolu_7".to_owned())),
                },
                TaskNotificationText {
                    status: Some(OpenWord::Unknown("archived".to_owned())),
                    summary: None,
                    tool_use_id: None,
                },
            ]
        );
        assert!(TaskNotificationText::parse_all("no tags").is_empty());
    }
}

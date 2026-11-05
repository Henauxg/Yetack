//! The notices that show a `user` line Claude Code writes itself.

use claude_protocol::{MessageOrigin, OpenWord, OriginKind, TaskNotificationText};
use cockpit_protocol::BackgroundTaskStatus;

use crate::transcript::words;

/// The longest notice made from the first line of a message, in characters.
const MAX_LINE_NOTICE_CHARS: usize = 200;
const TASK_DONE: &str = "A background task finished";
const TASK_FAILED: &str = "A background task failed";
const TASK_STOPPED: &str = "A background task was stopped";
const TASK_ENDED: &str = "A background task ended";

/// The notices that show a message Claude Code wrote itself; `None` for one the user typed.
///
/// - A task notification gives Claude Code's summary of each ended task, or a sentence built
///   from its status when it has no summary.
/// - Any other origin gives its kind and the first line of the text, such as
///   `peer: Another session sent a message`, cut to [`MAX_LINE_NOTICE_CHARS`].
pub(super) fn notices(origin: Option<&MessageOrigin>, text: &str) -> Option<Vec<String>> {
    let origin = origin?;
    let notices: Vec<String> = match &origin.kind {
        OpenWord::Known(OriginKind::Human) => return None,
        OpenWord::Known(OriginKind::TaskNotification) => TaskNotificationText::parse_all(text)
            .into_iter()
            .map(task_notice)
            .collect(),
        OpenWord::Unknown(_) => Vec::new(),
    };
    if notices.is_empty() {
        return Some(vec![line_notice(origin.kind.as_str(), text)]);
    }
    Some(notices)
}

fn task_notice(notification: TaskNotificationText) -> String {
    if let Some(summary) = notification.summary {
        return summary;
    }
    let status = match notification.status {
        Some(OpenWord::Known(status)) => words::ended_task_status(status),
        Some(OpenWord::Unknown(_)) | None => None,
    };
    let sentence = match status {
        Some(BackgroundTaskStatus::Done) => TASK_DONE,
        Some(BackgroundTaskStatus::Failed) => TASK_FAILED,
        Some(BackgroundTaskStatus::Stopped) => TASK_STOPPED,
        Some(BackgroundTaskStatus::Running) | None => TASK_ENDED,
    };
    sentence.to_owned()
}

fn line_notice(kind: &str, text: &str) -> String {
    let first_line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let notice = match first_line {
        Some(line) => format!("{kind}: {line}"),
        None => kind.to_owned(),
    };
    if notice.chars().count() <= MAX_LINE_NOTICE_CHARS {
        return notice;
    }
    let mut short: String = notice.chars().take(MAX_LINE_NOTICE_CHARS - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(kind: OpenWord<OriginKind>) -> MessageOrigin {
        MessageOrigin { kind }
    }

    #[test]
    fn a_typed_prompt_gives_no_notice() {
        let human = origin(OpenWord::Known(OriginKind::Human));

        assert_eq!(notices(None, "Fix the cart"), None);
        assert_eq!(notices(Some(&human), "Fix the cart"), None);
    }

    #[test]
    fn a_task_without_a_summary_gets_a_sentence_from_its_status() {
        let task = origin(OpenWord::Known(OriginKind::TaskNotification));
        let text = "<task-notification><status>failed</status></task-notification>\n\
            <task-notification><status>killed</status></task-notification>\n\
            <task-notification><status>archived</status></task-notification>";

        assert_eq!(
            notices(Some(&task), text),
            Some(vec![
                TASK_FAILED.to_owned(),
                TASK_STOPPED.to_owned(),
                TASK_ENDED.to_owned()
            ])
        );
    }

    #[test]
    fn another_origin_gives_its_kind_and_first_line_cut_short() {
        let peer = origin(OpenWord::Unknown("peer".to_owned()));
        let task = origin(OpenWord::Known(OriginKind::TaskNotification));
        let long = "x".repeat(MAX_LINE_NOTICE_CHARS);

        let short = notices(
            Some(&peer),
            "\n Another session sent a message \nSecond line",
        );
        let cut = notices(Some(&peer), &long).unwrap_or_default();
        let untagged = notices(Some(&task), "The task ended");

        assert_eq!(
            short,
            Some(vec!["peer: Another session sent a message".to_owned()])
        );
        assert_eq!(cut[0].chars().count(), MAX_LINE_NOTICE_CHARS);
        assert!(cut[0].starts_with("peer: x") && cut[0].ends_with('…'));
        assert_eq!(
            untagged,
            Some(vec!["task-notification: The task ended".to_owned()])
        );
    }
}

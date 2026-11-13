//! The notices that show a `user` line Claude Code writes itself.

use claude_protocol::{MessageOrigin, OpenWord, OriginKind, TaskNotificationText};
use cockpit_protocol::{BackgroundTaskStatus, NoticeKind};

use crate::transcript::words;

/// The longest notice made from the first line of a message, in characters.
const MAX_LINE_NOTICE_CHARS: usize = 200;
const TASK_DONE: &str = "A background task finished";
const TASK_FAILED: &str = "A background task failed";
const TASK_STOPPED: &str = "A background task was stopped";
const TASK_ENDED: &str = "A background task ended";

/// The notices that show a message Claude Code wrote itself, each with its kind; `None` for one
/// the user typed.
///
/// - A task notification gives Claude Code's summary of each ended task, or a sentence built
///   from its status when it has no summary.
/// - A peer's message gives its first line of prose, past the tags and the bracketed preamble
///   Claude Code wraps it in.
/// - Any other origin gives its kind and the first line of the text, such as
///   `scheduled: Run the nightly check`.
///
/// A first line is cut to [`MAX_LINE_NOTICE_CHARS`].
pub(super) fn notices(
    origin: Option<&MessageOrigin>,
    text: &str,
) -> Option<Vec<(String, NoticeKind)>> {
    let origin = origin?;
    let notices: Vec<(String, NoticeKind)> = match &origin.kind {
        OpenWord::Known(OriginKind::Human) => return None,
        OpenWord::Known(OriginKind::TaskNotification) => TaskNotificationText::parse_all(text)
            .into_iter()
            .map(task_notice)
            .collect(),
        OpenWord::Known(OriginKind::Peer) => {
            vec![(cut(message_line(text)), NoticeKind::Message)]
        }
        OpenWord::Unknown(_) => Vec::new(),
    };
    if notices.is_empty() {
        return Some(vec![(
            line_notice(origin.kind.as_str(), text),
            NoticeKind::Plain,
        )]);
    }
    Some(notices)
}

fn task_notice(notification: TaskNotificationText) -> (String, NoticeKind) {
    let status = match notification.status {
        Some(OpenWord::Known(status)) => words::ended_task_status(status),
        Some(OpenWord::Unknown(_)) | None => None,
    };
    let (sentence, kind) = match status {
        Some(BackgroundTaskStatus::Done) => (TASK_DONE, NoticeKind::TaskDone),
        Some(BackgroundTaskStatus::Failed) => (TASK_FAILED, NoticeKind::TaskFailed),
        Some(BackgroundTaskStatus::Stopped) => (TASK_STOPPED, NoticeKind::TaskStopped),
        Some(BackgroundTaskStatus::Running) | None => (TASK_ENDED, NoticeKind::TaskDone),
    };
    (
        notification.summary.unwrap_or_else(|| sentence.to_owned()),
        kind,
    )
}

/// A peer's message's first line of prose: the first line that is neither a tag, such as
/// `<agent-message from="…">`, nor a bracketed preamble, such as `[Subagent hand-back] …`.
/// Without one, its first line.
fn message_line(text: &str) -> &str {
    let lines = || text.lines().map(str::trim).filter(|line| !line.is_empty());
    lines()
        .find(|line| !is_tag(line) && !is_preamble(line))
        .or_else(|| lines().next())
        .unwrap_or_default()
}

/// A peer's message without the wrapper Claude Code adds, to unfold under its line: the tags
/// around it and the bracketed preamble go, and its lines lose their common indent, which
/// Markdown would draw as a code block. `None` when the body is no more than the line.
pub(super) fn message_body(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty() && !is_tag(line) && !is_preamble(line))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty() && !is_tag(line))
        .map_or(start, |last| last + 1)
        .max(start);
    let body = lines.get(start..end).unwrap_or_default();
    let indent = body
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let body: Vec<&str> = body
        .iter()
        .map(|line| line.get(indent..).unwrap_or_else(|| line.trim_start()))
        .collect();
    (body.len() > 1).then(|| body.join("\n"))
}

/// A line that is a whole tag, such as `<agent-message from="…">`.
fn is_tag(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('<') && line.ends_with('>')
}

/// A line that opens with a bracketed preamble, such as `[Subagent hand-back] …`. A Markdown
/// link, such as `[The fix](…) is merged.`, is prose.
fn is_preamble(line: &str) -> bool {
    line.trim()
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .is_some_and(|(_, after)| !after.starts_with(['(', '[', ':']))
}

fn cut(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_NOTICE_CHARS {
        return line.to_owned();
    }
    let mut short: String = line.chars().take(MAX_LINE_NOTICE_CHARS - 1).collect();
    short.push('…');
    short
}

fn line_notice(kind: &str, text: &str) -> String {
    let first_line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let notice = match first_line {
        Some(line) => format!("{kind}: {line}"),
        None => kind.to_owned(),
    };
    cut(&notice)
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
    fn a_task_without_a_summary_gets_a_sentence_and_a_kind_from_its_status() {
        let task = origin(OpenWord::Known(OriginKind::TaskNotification));
        let text = "<task-notification><status>failed</status></task-notification>\n\
            <task-notification><status>killed</status></task-notification>\n\
            <task-notification><status>archived</status></task-notification>";

        assert_eq!(
            notices(Some(&task), text),
            Some(vec![
                (TASK_FAILED.to_owned(), NoticeKind::TaskFailed),
                (TASK_STOPPED.to_owned(), NoticeKind::TaskStopped),
                (TASK_ENDED.to_owned(), NoticeKind::TaskDone),
            ])
        );
    }

    #[test]
    fn a_peer_message_gives_its_first_line_of_prose() {
        let peer = origin(OpenWord::Known(OriginKind::Peer));
        let text = "<agent-message from=\"session-7\">\n\
            [Subagent hand-back] The text below is the report of a subagent.\n\n\
            \x20   The cart test passes now.\n\
            \x20   Second line\n\
            </agent-message>";

        assert_eq!(
            notices(Some(&peer), text),
            Some(vec![(
                "The cart test passes now.".to_owned(),
                NoticeKind::Message
            )])
        );
        assert_eq!(
            notices(Some(&peer), "<agent-message from=\"session-7\">"),
            Some(vec![(
                "<agent-message from=\"session-7\">".to_owned(),
                NoticeKind::Message
            )])
        );
    }

    #[test]
    fn a_peer_message_unfolds_to_its_body_without_the_wrapper() {
        let text = "<agent-message from=\"session-7\">\n\
            [Subagent hand-back] The text below is the report of a subagent.\n\n\
            \x20   The cart test passes now.\n\n\
            \x20   - The total counts the tax.\n\
            \x20     - Once.\n\
            </agent-message>\n";

        assert_eq!(
            message_body(text).as_deref(),
            Some("The cart test passes now.\n\n- The total counts the tax.\n  - Once.")
        );
        assert_eq!(
            message_body("<agent-message>\n  The cart test passes now.\n</agent-message>"),
            None
        );
        assert_eq!(message_body("<agent-message></agent-message>"), None);
    }

    #[test]
    fn a_peer_message_keeps_a_first_line_that_starts_with_a_link_or_a_less_than_sign() {
        let peer = origin(OpenWord::Known(OriginKind::Peer));
        let link = "<agent-message from=\"session-7\">\n\
            [The cart fix](https://example.com/pull/12) is merged.\n\
            Nothing else is left.\n\
            </agent-message>";
        let less = "<agent-message from=\"session-7\">\n\
            <= 2 ms per frame now.\n\
            The lexer was the slow part.\n\
            </agent-message>";

        assert_eq!(
            notices(Some(&peer), link),
            Some(vec![(
                "[The cart fix](https://example.com/pull/12) is merged.".to_owned(),
                NoticeKind::Message
            )])
        );
        assert_eq!(
            message_body(link).as_deref(),
            Some("[The cart fix](https://example.com/pull/12) is merged.\nNothing else is left.")
        );
        assert_eq!(
            notices(Some(&peer), less),
            Some(vec![(
                "<= 2 ms per frame now.".to_owned(),
                NoticeKind::Message
            )])
        );
        assert_eq!(
            message_body(less).as_deref(),
            Some("<= 2 ms per frame now.\nThe lexer was the slow part.")
        );
    }

    #[test]
    fn another_origin_gives_its_kind_and_first_line_cut_short() {
        let scheduled = origin(OpenWord::Unknown("scheduled".to_owned()));
        let task = origin(OpenWord::Known(OriginKind::TaskNotification));
        let long = "x".repeat(MAX_LINE_NOTICE_CHARS);

        let short = notices(Some(&scheduled), "\n Run the nightly check \nSecond line");
        let cut = notices(Some(&scheduled), &long).unwrap_or_default();
        let untagged = notices(Some(&task), "The task ended");

        assert_eq!(
            short,
            Some(vec![(
                "scheduled: Run the nightly check".to_owned(),
                NoticeKind::Plain
            )])
        );
        let (cut, _) = cut.first().cloned().unwrap_or_default();
        assert_eq!(cut.chars().count(), MAX_LINE_NOTICE_CHARS);
        assert!(cut.starts_with("scheduled: x") && cut.ends_with('…'));
        assert_eq!(
            untagged,
            Some(vec![(
                "task-notification: The task ended".to_owned(),
                NoticeKind::Plain
            )])
        );
    }
}

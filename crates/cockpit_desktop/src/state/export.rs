//! An agent's conversation as Markdown, for the palette's Export entry: the user's messages,
//! Claude's answers, and one line per tool call.

use cockpit_protocol::{AnswerState, ItemBody, Question, ToolCall, ToolCallStatus};

use super::app_state::AgentEntry;
use super::transcript::HistoryState;
use crate::format;

/// Who a part of the conversation comes from, which a heading names when it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Speaker {
    You,
    Claude,
}

impl Speaker {
    fn heading(self) -> &'static str {
        match self {
            Self::You => "## You",
            Self::Claude => "## Claude",
        }
    }
}

/// A piece of the Markdown. Lines of a list stay together; other parts are paragraphs.
enum Part {
    Paragraph(String),
    ListLine(String),
}

/// Whether the window holds the whole conversation, so that an export misses nothing.
pub fn is_complete(entry: &AgentEntry) -> bool {
    entry.transcript.history() == &(HistoryState::Loaded { has_older: false })
}

/// The conversation the window holds. A subagent's own items are left out: its call's line
/// stands for them.
pub fn conversation_markdown(entry: &AgentEntry) -> String {
    let mut parts = vec![Part::Paragraph(format!("# {}", entry.summary.title))];
    let mut speaker = None;
    let mut say = |parts: &mut Vec<Part>, from: Speaker, part: Part| {
        if speaker != Some(from) {
            speaker = Some(from);
            parts.push(Part::Paragraph(from.heading().to_owned()));
        }
        parts.push(part);
    };
    let top_level = entry
        .transcript
        .items()
        .filter(|item| item.parent.is_none());
    for item in top_level {
        match &item.body {
            ItemBody::UserMessage(message) if !message.queued => {
                if !message.text.trim().is_empty() {
                    say(
                        &mut parts,
                        Speaker::You,
                        Part::Paragraph(message.text.clone()),
                    );
                }
                if !message.images.is_empty() {
                    let images = format::count(message.images.len(), "image", "images");
                    let line = format!("_{images} attached_");
                    say(&mut parts, Speaker::You, Part::Paragraph(line));
                }
            }
            ItemBody::AssistantText { markdown } => {
                say(
                    &mut parts,
                    Speaker::Claude,
                    Part::Paragraph(markdown.clone()),
                );
            }
            ItemBody::ToolCall(call) | ItemBody::Subagent(call) => {
                say(&mut parts, Speaker::Claude, Part::ListLine(tool_line(call)));
            }
            ItemBody::Question(question) => {
                for line in question_lines(question) {
                    say(&mut parts, Speaker::Claude, Part::ListLine(line));
                }
            }
            ItemBody::Plan(plan) => {
                say(
                    &mut parts,
                    Speaker::Claude,
                    Part::Paragraph(plan.markdown.clone()),
                );
            }
            ItemBody::Notice { text } => parts.push(Part::Paragraph(format!("_{text}_"))),
            ItemBody::UserMessage(_)
            | ItemBody::Thinking { .. }
            | ItemBody::Approval(_)
            | ItemBody::TurnEnd(_) => {}
        }
    }
    joined(&parts)
}

/// "- Bash: cargo test (failed, exit 101)".
fn tool_line(call: &ToolCall) -> String {
    let mut line = format!("- {}", call.tool);
    if !call.summary.is_empty() {
        line = format!("{line}: {}", call.summary);
    }
    match (call.status, call.exit_code) {
        (ToolCallStatus::Done, _) => line,
        (ToolCallStatus::Running, _) => format!("{line} (running)"),
        (ToolCallStatus::Failed, Some(code)) => format!("{line} (failed, exit {code})"),
        (ToolCallStatus::Failed, None) => format!("{line} (failed)"),
    }
}

/// One line per asked question, with its answer.
fn question_lines(question: &Question) -> Vec<String> {
    let mut lines = Vec::with_capacity(question.questions.len());
    for (index, asked) in question.questions.iter().enumerate() {
        let answer = match &question.answer {
            AnswerState::Answered(answers) => {
                answers.get(index).map_or_else(String::new, |answer| {
                    let other = answer.other.iter().cloned();
                    let picked: Vec<String> =
                        answer.selected.iter().cloned().chain(other).collect();
                    picked.join(", ")
                })
            }
            AnswerState::Waiting => "waiting for an answer".to_owned(),
            AnswerState::Cancelled => "cancelled".to_owned(),
        };
        lines.push(format!("- Question: {} {answer}", asked.text));
    }
    lines
}

fn joined(parts: &[Part]) -> String {
    let mut markdown = String::new();
    let mut in_list = false;
    for part in parts {
        let (text, list_line) = match part {
            Part::Paragraph(text) => (text, false),
            Part::ListLine(text) => (text, true),
        };
        if !markdown.is_empty() {
            markdown.push_str(if in_list && list_line { "\n" } else { "\n\n" });
        }
        markdown.push_str(text.trim_end());
        in_list = list_line;
    }
    markdown.push('\n');
    markdown
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentStatus, AskedQuestion, HistoryPage, Image, ImageMediaType, ItemId, PendingId,
        QuestionAnswer, QuestionOption, TranscriptItem, UserMessage,
    };
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::tests::summary;

    fn call(tool: &str, summary: &str, status: ToolCallStatus, exit_code: Option<i32>) -> ToolCall {
        ToolCall {
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input: Value::Null,
            status,
            output: None,
            exit_code,
            edit: None,
        }
    }

    fn user(text: &str, images: usize, queued: bool) -> ItemBody {
        let image = Image {
            media_type: ImageMediaType::Png,
            data: "aGVsbG8=".to_owned(),
        };
        ItemBody::UserMessage(UserMessage {
            text: text.to_owned(),
            images: vec![image; images],
            queued,
        })
    }

    fn entry_with(bodies: Vec<(Option<u32>, ItemBody)>, has_older: bool) -> AgentEntry {
        let mut entry = AgentEntry::new(summary(1, AgentStatus::Idle));
        let items = bodies
            .into_iter()
            .zip(1..)
            .map(|((parent, body), id)| TranscriptItem {
                id: ItemId::new(id),
                parent: parent.map(ItemId::new),
                body,
            })
            .collect();
        entry
            .transcript
            .merge_page(HistoryPage { items, has_older });
        entry
    }

    #[test]
    fn a_conversation_exports_messages_answers_and_one_line_per_tool_call() {
        let question = Question {
            pending: PendingId::new("request-4".to_owned()),
            questions: vec![AskedQuestion {
                header: "Scope".to_owned(),
                text: "Which tests?".to_owned(),
                options: vec![QuestionOption {
                    label: "Unit".to_owned(),
                    description: String::new(),
                }],
                multi_select: false,
            }],
            answer: AnswerState::Answered(vec![QuestionAnswer {
                selected: vec!["Unit".to_owned()],
                other: Some("and the doc tests".to_owned()),
            }]),
        };
        let entry = entry_with(
            vec![
                (None, user("Fix the column numbers.", 1, false)),
                (
                    None,
                    ItemBody::Thinking {
                        text: "Hmm.".to_owned(),
                    },
                ),
                (
                    None,
                    ItemBody::AssistantText {
                        markdown: "I read the lexer first.\n".to_owned(),
                    },
                ),
                (
                    None,
                    ItemBody::ToolCall(call("Read", "src/lexer.rs", ToolCallStatus::Done, None)),
                ),
                (
                    None,
                    ItemBody::ToolCall(call(
                        "Bash",
                        "cargo test",
                        ToolCallStatus::Failed,
                        Some(101),
                    )),
                ),
                (
                    None,
                    ItemBody::Subagent(call(
                        "Agent",
                        "Find the callers",
                        ToolCallStatus::Done,
                        None,
                    )),
                ),
                (
                    Some(6),
                    ItemBody::ToolCall(call("Grep", "column", ToolCallStatus::Done, None)),
                ),
                (None, ItemBody::Question(question)),
                (
                    None,
                    ItemBody::AssistantText {
                        markdown: "The columns count from 1 now.".to_owned(),
                    },
                ),
                (
                    None,
                    ItemBody::Notice {
                        text: "Conversation compacted".to_owned(),
                    },
                ),
                (None, user("Thanks", 0, false)),
                (None, user("Also the docs", 0, true)),
            ],
            false,
        );

        let markdown = conversation_markdown(&entry);

        assert_eq!(
            markdown,
            "# Agent 1

## You

Fix the column numbers.

_1 image attached_

## Claude

I read the lexer first.

- Read: src/lexer.rs
- Bash: cargo test (failed, exit 101)
- Agent: Find the callers
- Question: Which tests? Unit, and the doc tests

The columns count from 1 now.

_Conversation compacted_

## You

Thanks
"
        );
        assert!(is_complete(&entry));
    }

    #[test]
    fn an_export_with_older_messages_unloaded_is_not_complete() {
        let entry = entry_with(vec![(None, user("Run the tests", 0, false))], true);

        assert!(!is_complete(&entry));
    }
}

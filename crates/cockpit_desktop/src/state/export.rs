//! An agent's conversation as Markdown, for the palette's Export entry: the user's messages,
//! Claude's answers, one line per tool call, and one line per answered approval or plan and per
//! failed or stopped turn.

use std::collections::BTreeSet;

use cockpit_protocol::{
    AnswerState, Approval, ApprovalDecision, ItemBody, ItemId, Plan, PlanDecision, Question,
    ToolCall, ToolCallStatus, TranscriptItem, TurnEnd,
};

use super::app_state::AgentEntry;
use super::transcript::{self, HistoryState};
use crate::format;

/// The shortest fence of a fenced code block.
const MIN_FENCE: usize = 3;

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
    let items: Vec<&TranscriptItem> = entry.transcript.items().collect();
    let awaiting: BTreeSet<ItemId> = transcript::awaiting_approval(&items);
    let top_level = items.iter().filter(|item| item.parent.is_none());
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
            ItemBody::ToolCall(call) => {
                let line = tool_line(call, awaiting.contains(&item.id));
                say(&mut parts, Speaker::Claude, Part::ListLine(line));
            }
            ItemBody::Subagent(call) => {
                say(
                    &mut parts,
                    Speaker::Claude,
                    Part::ListLine(tool_line(call, false)),
                );
            }
            ItemBody::Approval(approval) => {
                if let Some(line) = approval_line(approval) {
                    say(&mut parts, Speaker::Claude, Part::ListLine(line));
                }
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
                if let Some(line) = plan_line(plan) {
                    say(&mut parts, Speaker::Claude, Part::ListLine(line));
                }
            }
            ItemBody::TurnEnd(end) => {
                if let Some(line) = turn_end_line(end) {
                    say(&mut parts, Speaker::Claude, Part::ListLine(line));
                }
            }
            ItemBody::Notice { text } => parts.push(notice(text)),
            ItemBody::UserMessage(_) | ItemBody::Thinking { .. } => {}
        }
    }
    joined(&parts)
}

/// "- Bash: cargo test (failed, exit 101)".
fn tool_line(call: &ToolCall, awaits_approval: bool) -> String {
    let mut line = format!("- {}", call.tool);
    if !call.summary.is_empty() {
        line = format!("{line}: {}", call.summary);
    }
    match (call.status, call.exit_code) {
        (ToolCallStatus::Done, _) => line,
        (ToolCallStatus::Running, _) if awaits_approval => {
            format!("{line} (waiting for approval)")
        }
        (ToolCallStatus::Running, _) => format!("{line} (running)"),
        (ToolCallStatus::Failed, Some(code)) => format!("{line} (failed, exit {code})"),
        (ToolCallStatus::Failed, None) => format!("{line} (failed)"),
    }
}

/// "- Approval of Bash: git push (denied: not on main)". `None` while it waits: its call's line
/// says so.
fn approval_line(approval: &Approval) -> Option<String> {
    let outcome = match &approval.answer {
        AnswerState::Waiting => return None,
        AnswerState::Answered(answer) => match (answer.decision, &answer.message) {
            (ApprovalDecision::Allow, _) => "allowed".to_owned(),
            (ApprovalDecision::Deny, Some(message)) => format!("denied: {}", one_line(message)),
            (ApprovalDecision::Deny, None) => "denied".to_owned(),
        },
        AnswerState::Cancelled => "withdrawn when the turn stopped".to_owned(),
    };
    let mut line = format!("- Approval of {}", approval.tool);
    if !approval.summary.is_empty() {
        line = format!("{line}: {}", approval.summary);
    }
    Some(format!("{line} ({outcome})"))
}

/// "- Plan approved, Claude asks before each edit". `None` while it waits.
fn plan_line(plan: &Plan) -> Option<String> {
    let outcome = match &plan.answer {
        AnswerState::Waiting => return None,
        AnswerState::Answered(answer) => match (answer.decision, &answer.message) {
            (PlanDecision::AcceptEdits, _) => {
                "approved, Claude edits files without asking".to_owned()
            }
            (PlanDecision::AskBeforeEdits, _) => {
                "approved, Claude asks before each edit".to_owned()
            }
            (PlanDecision::KeepPlanning, Some(message)) => {
                format!("sent back to keep planning: {}", one_line(message))
            }
            (PlanDecision::KeepPlanning, None) => "sent back to keep planning".to_owned(),
        },
        AnswerState::Cancelled => "withdrawn when the turn stopped".to_owned(),
    };
    Some(format!("- Plan {outcome}"))
}

/// "- The turn failed after 12 s: overloaded". `None` for a turn that ended well.
fn turn_end_line(end: &TurnEnd) -> Option<String> {
    let duration = format::duration(end.duration);
    match &end.error {
        Some(error) => Some(format!(
            "- The turn failed after {duration}: {}",
            one_line(error)
        )),
        None if end.stopped => Some(format!("- Stopped after {duration}")),
        None => None,
    }
}

/// A notice in italics, or in a fenced block when it holds several lines, such as a local
/// command's output: italics end at a blank line, and a line could start a heading or a list.
fn notice(text: &str) -> Part {
    let text = text.trim_end();
    if !text.contains('\n') {
        return Part::Paragraph(format!("_{text}_"));
    }
    let fence = "`".repeat(longest_backtick_run(text).max(MIN_FENCE - 1) + 1);
    Part::Paragraph(format!("{fence}\n{text}\n{fence}"))
}

/// A fence must be longer than any run of backticks in the block.
fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars() {
        run = if character == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    longest
}

/// A user's text on one line of a list.
fn one_line(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words.join(" ")
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
    use std::time::Duration;

    use cockpit_protocol::{
        AgentStatus, ApprovalAnswer, AskedQuestion, CostUsd, HistoryPage, Image, ImageMediaType,
        PendingId, PlanAnswer, QuestionAnswer, QuestionOption, TokenUsage, UserMessage,
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
    fn an_export_says_what_waits_what_was_answered_and_how_turns_ended() {
        let approval = |call: u32, answer| {
            ItemBody::Approval(Approval {
                pending: PendingId::new(format!("request-{call}")),
                tool: "Bash".to_owned(),
                summary: "git push".to_owned(),
                input: Value::Null,
                edit: None,
                reason: None,
                answer,
                tool_call: Some(ItemId::new(call)),
            })
        };
        let plan = |answer| {
            ItemBody::Plan(Plan {
                pending: PendingId::new("request-9".to_owned()),
                markdown: "1. Rename the module".to_owned(),
                answer,
            })
        };
        let turn_end = |error: Option<&str>, stopped| {
            ItemBody::TurnEnd(TurnEnd {
                duration: Duration::from_secs(12),
                cost: CostUsd::new(0.1),
                usage: TokenUsage::default(),
                error: error.map(str::to_owned),
                stopped,
            })
        };
        let push = || ItemBody::ToolCall(call("Bash", "git push", ToolCallStatus::Running, None));
        let denied = AnswerState::Answered(ApprovalAnswer {
            decision: ApprovalDecision::Deny,
            message: Some("Not on main.\nOpen a branch.".to_owned()),
        });
        let keep_planning = AnswerState::Answered(PlanAnswer {
            decision: PlanDecision::KeepPlanning,
            message: Some("Keep the old name".to_owned()),
        });
        let accepted = AnswerState::Answered(PlanAnswer {
            decision: PlanDecision::AskBeforeEdits,
            message: None,
        });
        let entry = entry_with(
            vec![
                (None, push()),
                (None, approval(1, denied)),
                (None, turn_end(None, false)),
                (None, plan(keep_planning)),
                (None, plan(accepted)),
                (None, turn_end(None, true)),
                (
                    None,
                    ItemBody::Notice {
                        text: "Context usage\n\n# System prompt: 3k\n```json".to_owned(),
                    },
                ),
                (None, turn_end(Some("API error:\noverloaded"), false)),
                (None, push()),
                (None, approval(9, AnswerState::Waiting)),
            ],
            false,
        );

        let markdown = conversation_markdown(&entry);

        assert_eq!(
            markdown,
            "# Agent 1

## Claude

- Bash: git push (running)
- Approval of Bash: git push (denied: Not on main. Open a branch.)

1. Rename the module

- Plan sent back to keep planning: Keep the old name

1. Rename the module

- Plan approved, Claude asks before each edit
- Stopped after 12 s

````
Context usage

# System prompt: 3k
```json
````

- The turn failed after 12 s: API error: overloaded
- Bash: git push (waiting for approval)
"
        );
    }

    #[test]
    fn an_export_with_older_messages_unloaded_is_not_complete() {
        let entry = entry_with(vec![(None, user("Run the tests", 0, false))], true);

        assert!(!is_complete(&entry));
    }
}

//! An agent's conversation, as the cockpit models it: independent of Claude Code's wire format.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::VariantArray;

use crate::ids::{ItemId, PendingId};
use crate::usage::{CostUsd, TokenUsage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptItem {
    pub id: ItemId,
    /// The subagent call this item runs under; `None` at the top level.
    pub parent: Option<ItemId>,
    pub body: ItemBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemBody {
    UserMessage(UserMessage),
    AssistantText {
        markdown: String,
    },
    /// Folded in the app.
    Thinking {
        text: String,
    },
    ToolCall(ToolCall),
    /// A tool call that runs a subagent. The subagent's own items name it as their `parent`.
    Subagent(ToolCall),
    Approval(Approval),
    Question(Question),
    Plan(Plan),
    TurnEnd(TurnEnd),
    /// A line from the daemon or Claude Code that is not part of the conversation, such as
    /// "Conversation compacted" or "claude exited with status 1".
    Notice {
        text: String,
        /// Markdown that `text` titles, such as the instructions of a skill Claude Code loaded,
        /// which the app shows folded. `None` when missing, so a notice saved without it still
        /// loads.
        #[serde(default)]
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    pub text: String,
    pub images: Vec<Image>,
    /// Sent while the agent works, and not yet passed to Claude Code.
    pub queued: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub media_type: ImageMediaType,
    /// The image file's bytes, in base64.
    pub data: String,
}

/// The image types Claude accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
pub enum ImageMediaType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Claude Code's tool name, such as `Bash`, `Edit` or `mcp__server__tool`.
    pub tool: String,
    /// One line for the card's title: the command, the file path, the pattern.
    pub summary: String,
    pub input: Value,
    pub status: ToolCallStatus,
    /// `None` while the call runs.
    pub output: Option<String>,
    /// The exit code of a command, when Claude Code reports it.
    pub exit_code: Option<i32>,
    /// For `Edit` and `Write`: the file and its text before and after.
    pub edit: Option<FileEdit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEdit {
    pub path: PathBuf,
    /// Empty for a file that `Write` creates.
    pub old_text: String,
    pub new_text: String,
}

/// Claude Code asks whether a tool call may run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub pending: PendingId,
    pub tool: String,
    pub summary: String,
    pub input: Value,
    /// For `Edit` and `Write`, so the app shows the change before it is allowed.
    pub edit: Option<FileEdit>,
    /// Why Claude Code asks, when it says.
    pub reason: Option<String>,
    pub answer: AnswerState<ApprovalAnswer>,
    /// The tool call it asks about, when the daemon has its item. `None` when missing, so an
    /// approval saved without it still loads.
    #[serde(default)]
    pub tool_call: Option<ItemId>,
}

/// Whether an approval, a question or a plan still waits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "answer", rename_all = "snake_case")]
pub enum AnswerState<A> {
    Waiting,
    Answered(A),
    /// Claude Code withdrew the request, after an interrupt or when the process ended.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAnswer {
    pub decision: ApprovalDecision,
    /// For a denial, what Claude should do instead.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Allow,
    Deny,
}

/// An `AskUserQuestion` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub pending: PendingId,
    pub questions: Vec<AskedQuestion>,
    /// One answer per question, in the same order.
    pub answer: AnswerState<Vec<QuestionAnswer>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedQuestion {
    /// A short label, such as "Database".
    pub header: String,
    pub text: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// The labels of the chosen options.
    pub selected: Vec<String>,
    /// The text of the "Other" field, when filled.
    pub other: Option<String>,
}

/// An `ExitPlanMode` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub pending: PendingId,
    pub markdown: String,
    pub answer: AnswerState<PlanAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAnswer {
    pub decision: PlanDecision,
    /// With `KeepPlanning`, what to change in the plan.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecision {
    AcceptEdits,
    AskBeforeEdits,
    KeepPlanning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnEnd {
    #[serde(
        rename = "duration_ms",
        serialize_with = "crate::time::serialize_millis",
        deserialize_with = "crate::time::deserialize_millis"
    )]
    pub duration: Duration,
    /// The cost of this turn alone.
    pub cost: CostUsd,
    pub usage: TokenUsage,
    /// Why the turn failed; `None` when it succeeded or the user stopped it.
    pub error: Option<String>,
    /// The user stopped the turn, with an interrupt. `false` when missing, so a turn end saved
    /// without it still loads.
    #[serde(default)]
    pub stopped: bool,
}

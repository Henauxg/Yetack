//! An agent's conversation, as the cockpit models it: independent of Claude Code's wire format.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::VariantArray;

use crate::ids::{ItemId, PendingId};
use crate::stored_bool::{FromStoredBool, enum_or_stored_bool};
use crate::usage::{CostUsd, TokenUsage};

/// Titles a skill's instructions, followed by the skill's command.
const SKILL_INSTRUCTIONS_TITLE: &str = "Skill instructions";

/// The title of the notice that holds a skill's instructions, such as
/// `Skill instructions: /review`. The app finds a skill's instructions by it.
pub fn skill_instructions_title(skill: &str) -> String {
    format!("{SKILL_INSTRUCTIONS_TITLE}: /{skill}")
}

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

impl ImageMediaType {
    /// The type a file name's extension names, whatever its case, such as `shot.PNG`.
    #[must_use]
    pub fn from_file_name(name: &str) -> Option<Self> {
        let extension = name.rsplit_once('.')?.1.to_lowercase();
        match extension.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "gif" => Some(Self::Gif),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }
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
    /// Event logs of older daemons hold `multi_select`, a `bool`, in its place.
    #[serde(alias = "multi_select", deserialize_with = "enum_or_stored_bool")]
    pub selection: Selection,
}

/// How many options of a question the user may choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    Single,
    Multiple,
}

impl FromStoredBool for Selection {
    fn from_stored_bool(multi_select: bool) -> Self {
        if multi_select {
            Self::Multiple
        } else {
            Self::Single
        }
    }
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
#[serde(from = "StoredTurnEnd")]
pub struct TurnEnd {
    #[serde(
        rename = "duration_ms",
        serialize_with = "crate::time::serialize_millis"
    )]
    pub duration: Duration,
    /// The cost of this turn alone.
    pub cost: CostUsd,
    pub usage: TokenUsage,
    pub outcome: TurnOutcome,
    /// Why the turn failed: `Some` with `Failed` and `NotSignedIn`, else `None`.
    pub error: Option<String>,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    /// The user stopped the turn, with an interrupt.
    Stopped,
    Failed,
    /// The turn found no valid login for the agent's account.
    NotSignedIn,
}

/// A turn end as it reads, from this daemon or from the event log of an older one, which holds
/// `stopped` and `not_signed_in`, two `bool`s, in place of `outcome`.
#[derive(Deserialize)]
struct StoredTurnEnd {
    #[serde(
        rename = "duration_ms",
        deserialize_with = "crate::time::deserialize_millis"
    )]
    duration: Duration,
    cost: CostUsd,
    usage: TokenUsage,
    outcome: Option<TurnOutcome>,
    error: Option<String>,
    #[serde(default)]
    stopped: bool,
    #[serde(default)]
    not_signed_in: bool,
}

impl From<StoredTurnEnd> for TurnEnd {
    fn from(stored: StoredTurnEnd) -> Self {
        let outcome = match stored.outcome {
            Some(outcome) => outcome,
            None if stored.stopped => TurnOutcome::Stopped,
            None if stored.not_signed_in => TurnOutcome::NotSignedIn,
            None if stored.error.is_some() => TurnOutcome::Failed,
            None => TurnOutcome::Completed,
        };
        Self {
            duration: stored.duration,
            cost: stored.cost,
            usage: stored.usage,
            outcome,
            error: stored.error,
        }
    }
}

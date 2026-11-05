use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::ids::{HookId, MessageUuid, SessionId, TaskId, ToolUseId};
use crate::mcp_status::McpServerState;
use crate::open_word::OpenWord;
use crate::parse_error::ParseError;
use crate::permission::PermissionMode;
use crate::wire;

/// A `system` line, by its `subtype`.
#[derive(Debug, Clone, PartialEq)]
pub enum SystemEvent {
    Init(SystemInit),
    ApiRetry(ApiRetry),
    TaskStarted(TaskStarted),
    TaskProgress(TaskProgress),
    TaskNotification(TaskNotification),
    TaskUpdated(TaskUpdated),
    SessionStateChanged(SessionStateChanged),
    ThinkingTokens(ThinkingTokens),
    HookStarted(HookRun),
    HookProgress(HookRun),
    HookResponse(HookRun),
    /// A subtype this crate does not know, as Claude Code wrote it.
    Other(Value),
}

/// Sent at the start of each turn: the session, the model and what the session can use.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SystemInit {
    pub session_id: SessionId,
    pub uuid: Option<MessageUuid>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<OpenWord<PermissionMode>>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<InitMcpServer>,
    #[serde(default)]
    pub slash_commands: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    pub claude_code_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InitMcpServer {
    pub name: String,
    pub status: Option<OpenWord<McpServerState>>,
}

/// An API call failed and Claude Code retries it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiRetry {
    pub attempt: Option<u32>,
    pub max_retries: Option<u32>,
    #[serde(
        rename = "retry_delay_ms",
        default,
        deserialize_with = "wire::optional_milliseconds"
    )]
    pub retry_delay: Option<Duration>,
    /// The HTTP status of the failed call, such as 529.
    pub error_status: Option<u16>,
    /// Such as `rate_limit` or `server_error`.
    pub error: Option<String>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskStarted {
    pub task_id: TaskId,
    #[serde(default)]
    pub description: String,
    /// Such as `local_agent` for a subagent.
    pub task_type: Option<String>,
    /// The tool call that started the task.
    pub tool_use_id: Option<ToolUseId>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskProgress {
    pub task_id: TaskId,
    #[serde(default)]
    pub description: String,
    pub usage: Option<TaskUsage>,
    pub last_tool_name: Option<String>,
    pub tool_use_id: Option<ToolUseId>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

/// A task completed, failed or was stopped.
///
/// Not every task that ends sends one: a [`TaskUpdated`] with a terminal status ends it too.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskNotification {
    pub task_id: TaskId,
    pub status: Option<OpenWord<TaskStatus>>,
    #[serde(default)]
    pub summary: String,
    /// The file holding the task's full output.
    pub output_file: Option<PathBuf>,
    pub usage: Option<TaskUsage>,
    pub tool_use_id: Option<ToolUseId>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

/// A background task's state changed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TaskUpdated {
    pub task_id: TaskId,
    /// The fields that changed.
    #[serde(default)]
    pub patch: TaskPatch,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct TaskPatch {
    pub status: Option<OpenWord<TaskStatus>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct TaskUsage {
    pub total_tokens: u64,
    pub tool_uses: u32,
    #[serde(
        rename = "duration_ms",
        deserialize_with = "wire::optional_milliseconds"
    )]
    pub duration: Option<Duration>,
}

/// The statuses of `task_notification` and `task_updated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    /// A stopped task, as `task_notification` says it.
    Stopped,
    /// A stopped task, as `task_updated` says it.
    Killed,
}

impl TaskStatus {
    /// Whether the task has ended.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        match self {
            Self::Pending | Self::Running | Self::Paused => false,
            Self::Completed | Self::Failed | Self::Stopped | Self::Killed => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionStateChanged {
    pub state: OpenWord<SessionState>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum SessionState {
    Idle,
    Running,
    /// Claude Code waits for an answer to one of its control requests.
    RequiresAction,
}

/// Claude is thinking: an estimate of the tokens it has thought so far in this step.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ThinkingTokens {
    pub estimated_tokens: u32,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

/// A hook run: `hook_started` carries the names, `hook_progress` and `hook_response` the output.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HookRun {
    pub hook_id: HookId,
    /// Such as `SessionStart:startup`.
    #[serde(default)]
    pub hook_name: String,
    /// Such as `SessionStart` or `PreToolUse`.
    #[serde(default)]
    pub hook_event: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub output: String,
    pub exit_code: Option<i32>,
    pub outcome: Option<OpenWord<HookOutcome>>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum HookOutcome {
    Success,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum SystemSubtype {
    Init,
    ApiRetry,
    TaskStarted,
    TaskProgress,
    TaskNotification,
    TaskUpdated,
    SessionStateChanged,
    ThinkingTokens,
    HookStarted,
    HookProgress,
    HookResponse,
}

impl SystemEvent {
    pub(crate) fn from_value(value: Value) -> Result<Self, ParseError> {
        let Some(subtype) = wire::word::<SystemSubtype>(&value, wire::SUBTYPE) else {
            return Ok(Self::Other(value));
        };
        let kind: &'static str = subtype.into();
        match subtype {
            SystemSubtype::Init => wire::read(&value, kind).map(Self::Init),
            SystemSubtype::ApiRetry => wire::read(&value, kind).map(Self::ApiRetry),
            SystemSubtype::TaskStarted => wire::read(&value, kind).map(Self::TaskStarted),
            SystemSubtype::TaskProgress => wire::read(&value, kind).map(Self::TaskProgress),
            SystemSubtype::TaskNotification => wire::read(&value, kind).map(Self::TaskNotification),
            SystemSubtype::TaskUpdated => wire::read(&value, kind).map(Self::TaskUpdated),
            SystemSubtype::SessionStateChanged => {
                wire::read(&value, kind).map(Self::SessionStateChanged)
            }
            SystemSubtype::ThinkingTokens => wire::read(&value, kind).map(Self::ThinkingTokens),
            SystemSubtype::HookStarted => wire::read(&value, kind).map(Self::HookStarted),
            SystemSubtype::HookProgress => wire::read(&value, kind).map(Self::HookProgress),
            SystemSubtype::HookResponse => wire::read(&value, kind).map(Self::HookResponse),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtypes_are_pinned() {
        let words: Vec<&str> = SystemSubtype::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();
        assert_eq!(
            words,
            [
                "init",
                "api_retry",
                "task_started",
                "task_progress",
                "task_notification",
                "task_updated",
                "session_state_changed",
                "thinking_tokens",
                "hook_started",
                "hook_progress",
                "hook_response",
            ]
        );
    }
}

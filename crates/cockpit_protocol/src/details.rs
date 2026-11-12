//! What the floating blocks and the agent map show about an agent: tasks, background work,
//! subagents, changed files, context usage and MCP servers. The slash commands travel apart, in
//! `CommandsChanged`, since they change only when `claude` starts or reloads.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::{BackgroundTaskId, ItemId, PromptId};
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentDetails {
    /// The latest `TodoWrite` list.
    pub tasks: Vec<TaskListItem>,
    pub background_tasks: Vec<BackgroundTask>,
    pub changed_files: Vec<ChangedFile>,
    /// The last context usage Claude Code reported.
    pub context: Option<ContextUsage>,
    pub mcp_servers: Vec<McpServer>,
    /// Every subagent the agent started, at any depth, in the order they started. Empty when
    /// missing, so details from an older daemon still load.
    #[serde(default)]
    pub subagents: Vec<Subagent>,
    /// The prompts Claude has not read yet, oldest first. Each joins the transcript where Claude
    /// reads it.
    #[serde(default)]
    pub waiting_prompts: Vec<WaitingPrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingPrompt {
    pub id: PromptId,
    pub text: String,
    /// The images travel with the prompt only, so that each details change stays small.
    pub image_count: u32,
    pub state: WaitingState,
    /// When the user sent it. `None` from an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum WaitingState {
    /// The daemon holds it until the turn ends. It can still be cancelled.
    Queued,
    /// `claude` has it: Claude reads it at its next step, or starts a turn with it.
    Sent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListItem {
    pub content: String,
    pub status: TaskStatus,
    /// The text shown while the task is in progress, such as "Running the tests".
    pub active_form: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

/// A subagent or a command that Claude Code runs as a task, in the background or in the
/// foreground.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: BackgroundTaskId,
    pub kind: BackgroundTaskKind,
    pub description: String,
    pub status: BackgroundTaskStatus,
    /// With `ended_at`, gives the elapsed time, which the app keeps counting while the task runs.
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub tool_calls: u32,
    /// The tool call that started it, which `AgentStopTask` stops with it. Claude Code gives a
    /// task to a command that runs in the foreground too. `None` from an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<ItemId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    Agent,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Running,
    Stopped,
    Done,
    Failed,
}

/// A subagent, for the agent map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    /// The item of the call that started it, which its own items name as their `parent`.
    pub item: ItemId,
    /// The item of the subagent that started it; `None` when the agent did.
    pub parent: Option<ItemId>,
    pub description: String,
    /// Such as `Explore` or `general-purpose`; `None` for Claude Code's default.
    pub agent_type: Option<String>,
    pub run: SubagentRun,
    pub status: SubagentStatus,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    /// `None` until Claude Code reports the subagent's usage, which it may never do.
    pub tokens: Option<u64>,
    pub tool_calls: u32,
    /// Its latest tool call. When that call started a subagent, it waits for that one.
    pub last_tool: Option<SubagentToolCall>,
    /// Why it failed, in one line.
    pub error: Option<String>,
    /// The agent's turn it started in, counted from 1 as [`AgentSummary::turns`] counts them.
    ///
    /// [`AgentSummary::turns`]: crate::AgentSummary::turns
    pub turn: u32,
    /// Its background task, which `agent.stop_task` stops. `None` while Claude Code reports none.
    pub task: Option<BackgroundTaskId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentToolCall {
    /// `None` when only Claude Code's progress report names the tool, without its call.
    pub item: Option<ItemId>,
    pub tool: String,
    /// The call's one-line summary, as its item has it. Empty without the call.
    pub summary: String,
}

/// Whether the call that started the subagent waits for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum SubagentRun {
    Foreground,
    /// Its call returns at once, and the subagent runs as a background task.
    Background,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    /// One of its approvals, questions or plans waits for an answer.
    NeedsYou,
    Done,
    Failed,
    Stopped,
}

impl SubagentStatus {
    /// Whether it still runs, waiting for you or not.
    #[must_use]
    pub fn is_running(self) -> bool {
        match self {
            Self::Running | Self::NeedsYou => true,
            Self::Done | Self::Failed | Self::Stopped => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub lines_added: u32,
    pub lines_removed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub tokens: u32,
    pub max_tokens: u32,
    /// From 0 to 100.
    pub percent: u8,
    pub categories: Vec<ContextCategory>,
}

/// A part of the context, such as "System prompt" or "Messages".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub status: McpServerStatus,
    pub error: Option<String>,
    pub version: Option<String>,
    pub transport: Option<McpTransport>,
    pub scope: Option<McpScope>,
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum McpServerStatus {
    Connected,
    Failed,
    NeedsAuth,
    Pending,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// `command` is the command line that starts the server, with its arguments.
    Stdio {
        command: String,
    },
    Http {
        url: String,
    },
    Sse {
        url: String,
    },
}

/// Where the server's configuration lives. `mcp.remove` accepts `User`, `Project` and `Local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    User,
    Project,
    Local,
    Dynamic,
    Enterprise,
    Managed,
    ClaudeAi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    /// The server's `readOnlyHint`, `destructiveHint` and `openWorldHint`; `false` when absent.
    pub read_only: bool,
    pub destructive: bool,
    pub open_world: bool,
    /// Calls this agent made to the tool.
    pub calls: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    /// Without the leading `/`.
    pub name: String,
    pub description: String,
    pub kind: CommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Skill,
    Command,
}

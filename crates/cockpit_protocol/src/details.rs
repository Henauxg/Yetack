//! What the floating blocks show about an agent: tasks, background work, changed files, context
//! usage, MCP servers and slash commands.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::BackgroundTaskId;
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
    pub commands: Vec<SlashCommand>,
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

/// A subagent or a command running in the background.
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

//! What the sidebar and the center header show about an agent.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::{AgentId, ClaudeSessionId};
use crate::time::Timestamp;
use crate::usage::CostUsd;

/// Derived by the daemon, never by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// From a prompt until the turn ends.
    Working,
    /// An approval, a question or a plan waits for an answer.
    NeedsYou,
    /// The turn ended and nothing waits.
    Idle,
    /// The `claude` process ended.
    Exited,
}

/// Claude Code's permission modes, under Claude Code's own names.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
    Auto,
    DontAsk,
    BypassPermissions,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// The task list's progress, shown as "3/5".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskProgress {
    pub done: u16,
    pub total: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: AgentId,
    pub title: String,
    /// The agent's working folder, on the daemon's machine.
    pub folder: PathBuf,
    pub status: AgentStatus,
    /// What the agent does now, in one line, such as "Running cargo test".
    pub activity: Option<String>,
    /// Approvals, questions and plans waiting for an answer.
    pub pending_count: u16,
    /// `None` until Claude Code reports the model it uses.
    pub model: Option<String>,
    /// The effort the agent runs at: the one it was given, or else the one Claude Code applies by
    /// itself, from its settings or the model's default. `None` while unknown.
    pub effort: Option<Effort>,
    /// `effort` is the one Claude Code applies by itself, which an agent in another folder or on
    /// another model may not get: a client that starts a new agent like this one leaves its effort
    /// out. `false` when missing, so a summary saved without it still loads.
    #[serde(default)]
    pub effort_is_default: bool,
    pub mode: PermissionMode,
    /// `None` when the agent has no task list.
    pub task_progress: Option<TaskProgress>,
    /// The share of the context window in use, from 0 to 100.
    pub context_percent: Option<u8>,
    /// When the prompt cache expires if the agent stays idle.
    pub cache_expires_at: Option<Timestamp>,
    pub turns: u32,
    /// The total cost so far at API prices, which a subscription does not pay.
    pub cost: CostUsd,
    pub started_at: Timestamp,
    /// `None` until Claude Code reports its session.
    pub session: Option<ClaudeSessionId>,
}

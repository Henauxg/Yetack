//! What the sidebar and the center header show about an agent.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::{AccountId, AgentId, ClaudeSessionId};
use crate::position::Position;
use crate::stored_bool::{FromStoredBool, enum_or_stored_bool};
use crate::time::Timestamp;
use crate::usage::CostUsd;

/// Derived by the daemon, never by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// From a prompt until the turn ends.
    Working,
    /// An approval, a question or a plan waits for an answer, or the agent's account needs a
    /// sign-in.
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
    /// Where `effort` comes from. `Chosen` when missing, so a summary saved without it still
    /// loads. Event logs of older daemons hold `effort_is_default`, a `bool`, in its place.
    #[serde(
        default,
        alias = "effort_is_default",
        deserialize_with = "enum_or_stored_bool"
    )]
    pub effort_source: EffortSource,
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
    /// When the agent last did something: it started, a prompt went to it, it began to wait for
    /// you, a turn ended, or its process ended. `None` from a daemon that predates it.
    #[serde(default)]
    pub last_activity_at: Option<Timestamp>,
    /// When the running turn started. `None` between turns, and from a daemon that predates it.
    #[serde(default)]
    pub turn_started_at: Option<Timestamp>,
    /// `None` until Claude Code reports its session.
    pub session: Option<ClaudeSessionId>,
    /// Where the agent shows in the list. An event logged before positions existed reads 0.
    #[serde(default)]
    pub position: Position,
    /// The account the agent's `claude` runs with. The default account when missing, as for an
    /// agent from a daemon that predates accounts.
    #[serde(default)]
    pub account: AccountId,
    /// `NotWaiting` when missing, so a summary saved without it still loads. Event logs of older
    /// daemons hold `waits_for_sign_in`, a `bool`, in its place.
    #[serde(
        default,
        alias = "waits_for_sign_in",
        deserialize_with = "enum_or_stored_bool"
    )]
    pub sign_in_wait: SignInWait,
}

/// Where an agent's effort comes from.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum EffortSource {
    /// A client gave it, at the start or with `agent.set_effort`.
    #[default]
    Chosen,
    /// Claude Code applies it by itself, from its settings or the model's default. An agent in
    /// another folder or on another model may not get it: a client that starts a new agent like
    /// this one leaves its effort out.
    ClaudeCodeDefault,
}

impl FromStoredBool for EffortSource {
    fn from_stored_bool(is_default: bool) -> Self {
        if is_default {
            Self::ClaudeCodeDefault
        } else {
            Self::Chosen
        }
    }
}

/// Whether an agent waits for its account to sign in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum SignInWait {
    #[default]
    NotWaiting,
    /// The last turn found no valid login for the account, and the agent waits for you until you
    /// send a message or the account signs in.
    Waiting,
}

impl FromStoredBool for SignInWait {
    fn from_stored_bool(waits: bool) -> Self {
        if waits {
            Self::Waiting
        } else {
            Self::NotWaiting
        }
    }
}

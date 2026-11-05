//! What a client asks the daemon.
//!
//! A request is `{"method": "agent.prompt", "params": {...}}`. `Method` holds the method names,
//! so `Request` serializes by hand through it.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::agent::{Effort, PermissionMode};
use crate::details::McpScope;
use crate::ids::{AgentId, BackgroundTaskId, ClaudeSessionId, ItemId, PendingId};
use crate::settings::Settings;
use crate::transcript::{ApprovalAnswer, Image, PlanAnswer, QuestionAnswer};

/// One variant per method. Each says which `ResponseBody` answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Answered by `AgentStarted`.
    AgentStart(StartParams),
    /// Answered by `Done`. A prompt sent while the agent works is queued.
    AgentPrompt(PromptParams),
    /// Removes a queued prompt before Claude Code gets it. Answered by `Done`.
    AgentCancelQueued(CancelQueuedParams),
    /// Answers an approval, a question or a plan. Answered by `Done`.
    AgentAnswer(AnswerParams),
    /// Stops the current turn; the process keeps running. Answered by `Done`.
    AgentInterrupt(AgentParams),
    /// Ends the `claude` process; the agent stays listed as exited. Answered by `Done`.
    AgentStop(AgentParams),
    /// Restarts an exited agent with `--resume`. Answered by `Done`.
    AgentResume(AgentParams),
    /// Removes an exited agent from the list. Its Claude session stays on disk, for
    /// `sessions.resume`. Answered by `Done`.
    AgentRemove(AgentParams),
    /// Answered by `History`.
    AgentHistory(HistoryParams),
    /// Answered by `Details`. `DetailsChanged` events keep them current after that.
    AgentDetails(AgentParams),
    /// Answered by `Done`.
    AgentSetModel(SetModelParams),
    /// Answered by `Done`.
    AgentSetEffort(SetEffortParams),
    /// Answered by `Done`.
    AgentSetMode(SetModeParams),
    /// Renames the agent, and its Claude session when its process runs. Answered by `Done`, and
    /// an `AgentUpdated` event to every client.
    AgentRename(RenameParams),
    /// Places the agent in the list. Answered by `Done`, and an `AgentUpdated` event for each
    /// agent whose position changed: the moved one alone, unless its new neighbours leave no room.
    AgentMove(MoveParams),
    /// Asks Claude Code for the context usage now. Answered by `ContextUsage`.
    AgentContextUsage(AgentParams),
    /// Answered by `Done`.
    AgentStopTask(StopTaskParams),
    /// Asks Claude Code for the MCP servers' status now. Answered by `McpServers`.
    AgentMcpStatus(AgentParams),
    /// Answered by `Done`.
    AgentMcpReconnect(McpServerParams),
    /// Enables or disables an MCP server for this agent only. Answered by `Done`.
    AgentMcpToggle(McpToggleParams),
    /// The slash commands and skills, for the palette. Answered by `Commands`.
    AgentCommands(AgentParams),
    /// Answered by `File`.
    AgentReadFile(ReadFileParams),
    /// Runs `claude mcp remove <server> -s <scope>` in the agent's folder. Answered by `Done`.
    McpRemove(McpRemoveParams),
    /// Replaces the settings. Answered by `Done`, and a `SettingsChanged` event to every client.
    SettingsSet(Settings),
    /// Past Claude sessions on the daemon's machine. Answered by `Sessions`.
    SessionsList,
    /// Starts an agent from a past Claude session. Answered by `AgentStarted`.
    SessionsResume(SessionResumeParams),
}

/// The method names on the wire.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
pub enum Method {
    #[strum(serialize = "agent.start")]
    AgentStart,
    #[strum(serialize = "agent.prompt")]
    AgentPrompt,
    #[strum(serialize = "agent.cancel_queued")]
    AgentCancelQueued,
    #[strum(serialize = "agent.answer")]
    AgentAnswer,
    #[strum(serialize = "agent.interrupt")]
    AgentInterrupt,
    #[strum(serialize = "agent.stop")]
    AgentStop,
    #[strum(serialize = "agent.resume")]
    AgentResume,
    #[strum(serialize = "agent.remove")]
    AgentRemove,
    #[strum(serialize = "agent.history")]
    AgentHistory,
    #[strum(serialize = "agent.details")]
    AgentDetails,
    #[strum(serialize = "agent.set_model")]
    AgentSetModel,
    #[strum(serialize = "agent.set_effort")]
    AgentSetEffort,
    #[strum(serialize = "agent.set_mode")]
    AgentSetMode,
    #[strum(serialize = "agent.rename")]
    AgentRename,
    #[strum(serialize = "agent.move")]
    AgentMove,
    #[strum(serialize = "agent.context_usage")]
    AgentContextUsage,
    #[strum(serialize = "agent.stop_task")]
    AgentStopTask,
    #[strum(serialize = "agent.mcp_status")]
    AgentMcpStatus,
    #[strum(serialize = "agent.mcp_reconnect")]
    AgentMcpReconnect,
    #[strum(serialize = "agent.mcp_toggle")]
    AgentMcpToggle,
    #[strum(serialize = "agent.commands")]
    AgentCommands,
    #[strum(serialize = "agent.read_file")]
    AgentReadFile,
    #[strum(serialize = "mcp.remove")]
    McpRemove,
    #[strum(serialize = "settings.set")]
    SettingsSet,
    #[strum(serialize = "sessions.list")]
    SessionsList,
    #[strum(serialize = "sessions.resume")]
    SessionsResume,
}

impl Method {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

impl Serialize for Method {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Method {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let name = String::deserialize(deserializer)?;
        name.parse()
            .map_err(|_| D::Error::custom(format!("unknown method `{name}`")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartParams {
    pub folder: PathBuf,
    /// `None` lets the daemon name the agent.
    pub title: Option<String>,
    /// `None` keeps Claude Code's default, as do `effort` and `mode`.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub mode: Option<PermissionMode>,
    pub prompt: Option<Prompt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub text: String,
    pub images: Vec<Image>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptParams {
    pub agent: AgentId,
    pub prompt: Prompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelQueuedParams {
    pub agent: AgentId,
    /// The queued user message.
    pub item: ItemId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerParams {
    pub agent: AgentId,
    pub pending: PendingId,
    pub answer: Answer,
}

/// Its kind matches the item that waits: an `Approval`, a `Question` or a `Plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Approval(ApprovalAnswer),
    /// One answer per question, in the order of the questions.
    Question {
        answers: Vec<QuestionAnswer>,
    },
    Plan(PlanAnswer),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentParams {
    pub agent: AgentId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryParams {
    pub agent: AgentId,
    /// Items older than this one; `None` for the latest page.
    pub before: Option<ItemId>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModelParams {
    pub agent: AgentId,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEffortParams {
    pub agent: AgentId,
    pub effort: Effort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetModeParams {
    pub agent: AgentId,
    pub mode: PermissionMode,
}

/// The longest title the daemon keeps, for an agent or a past session: every title goes into
/// every snapshot.
pub const MAX_TITLE_CHARS: usize = 200;

/// The daemon trims the title, and refuses it empty or over [`MAX_TITLE_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameParams {
    pub agent: AgentId,
    pub title: String,
}

/// The agent goes just above `before`, or to the bottom of the list when `before` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveParams {
    pub agent: AgentId,
    pub before: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopTaskParams {
    pub agent: AgentId,
    pub task: BackgroundTaskId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerParams {
    pub agent: AgentId,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToggleParams {
    pub agent: AgentId,
    pub server: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFileParams {
    pub agent: AgentId,
    /// Absolute, or relative to the agent's folder.
    pub path: PathBuf,
}

/// The agent gives the folder that `project` and `local` scopes depend on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpRemoveParams {
    pub agent: AgentId,
    pub server: String,
    pub scope: McpScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResumeParams {
    pub session: ClaudeSessionId,
}

/// A request on the wire. `params` is absent for a method without parameters.
#[derive(Serialize, Deserialize)]
struct Tagged<P> {
    method: Method,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<P>,
}

impl Request {
    #[must_use]
    pub fn method(&self) -> Method {
        match self {
            Self::AgentStart(_) => Method::AgentStart,
            Self::AgentPrompt(_) => Method::AgentPrompt,
            Self::AgentCancelQueued(_) => Method::AgentCancelQueued,
            Self::AgentAnswer(_) => Method::AgentAnswer,
            Self::AgentInterrupt(_) => Method::AgentInterrupt,
            Self::AgentStop(_) => Method::AgentStop,
            Self::AgentResume(_) => Method::AgentResume,
            Self::AgentRemove(_) => Method::AgentRemove,
            Self::AgentHistory(_) => Method::AgentHistory,
            Self::AgentDetails(_) => Method::AgentDetails,
            Self::AgentSetModel(_) => Method::AgentSetModel,
            Self::AgentSetEffort(_) => Method::AgentSetEffort,
            Self::AgentSetMode(_) => Method::AgentSetMode,
            Self::AgentRename(_) => Method::AgentRename,
            Self::AgentMove(_) => Method::AgentMove,
            Self::AgentContextUsage(_) => Method::AgentContextUsage,
            Self::AgentStopTask(_) => Method::AgentStopTask,
            Self::AgentMcpStatus(_) => Method::AgentMcpStatus,
            Self::AgentMcpReconnect(_) => Method::AgentMcpReconnect,
            Self::AgentMcpToggle(_) => Method::AgentMcpToggle,
            Self::AgentCommands(_) => Method::AgentCommands,
            Self::AgentReadFile(_) => Method::AgentReadFile,
            Self::McpRemove(_) => Method::McpRemove,
            Self::SettingsSet(_) => Method::SettingsSet,
            Self::SessionsList => Method::SessionsList,
            Self::SessionsResume(_) => Method::SessionsResume,
        }
    }
}

impl Serialize for Request {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        fn tagged<P: Serialize, S: Serializer>(
            method: Method,
            params: &P,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            Tagged {
                method,
                params: Some(params),
            }
            .serialize(serializer)
        }

        let method = self.method();
        match self {
            Self::AgentStart(params) => tagged(method, params, serializer),
            Self::AgentPrompt(params) => tagged(method, params, serializer),
            Self::AgentCancelQueued(params) => tagged(method, params, serializer),
            Self::AgentAnswer(params) => tagged(method, params, serializer),
            Self::AgentInterrupt(params) => tagged(method, params, serializer),
            Self::AgentStop(params) => tagged(method, params, serializer),
            Self::AgentResume(params) => tagged(method, params, serializer),
            Self::AgentRemove(params) => tagged(method, params, serializer),
            Self::AgentHistory(params) => tagged(method, params, serializer),
            Self::AgentDetails(params) => tagged(method, params, serializer),
            Self::AgentSetModel(params) => tagged(method, params, serializer),
            Self::AgentSetEffort(params) => tagged(method, params, serializer),
            Self::AgentSetMode(params) => tagged(method, params, serializer),
            Self::AgentRename(params) => tagged(method, params, serializer),
            Self::AgentMove(params) => tagged(method, params, serializer),
            Self::AgentContextUsage(params) => tagged(method, params, serializer),
            Self::AgentStopTask(params) => tagged(method, params, serializer),
            Self::AgentMcpStatus(params) => tagged(method, params, serializer),
            Self::AgentMcpReconnect(params) => tagged(method, params, serializer),
            Self::AgentMcpToggle(params) => tagged(method, params, serializer),
            Self::AgentCommands(params) => tagged(method, params, serializer),
            Self::AgentReadFile(params) => tagged(method, params, serializer),
            Self::McpRemove(params) => tagged(method, params, serializer),
            Self::SettingsSet(params) => tagged(method, params, serializer),
            Self::SessionsList => Tagged::<()> {
                method,
                params: None,
            }
            .serialize(serializer),
            Self::SessionsResume(params) => tagged(method, params, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{DeserializeOwned, Error};

        fn params<P: DeserializeOwned, E: Error>(params: Option<Value>) -> Result<P, E> {
            serde_json::from_value(params.unwrap_or(Value::Null)).map_err(E::custom)
        }

        let Tagged {
            method,
            params: raw,
        } = Tagged::<Value>::deserialize(deserializer)?;
        let request = match method {
            Method::AgentStart => Self::AgentStart(params(raw)?),
            Method::AgentPrompt => Self::AgentPrompt(params(raw)?),
            Method::AgentCancelQueued => Self::AgentCancelQueued(params(raw)?),
            Method::AgentAnswer => Self::AgentAnswer(params(raw)?),
            Method::AgentInterrupt => Self::AgentInterrupt(params(raw)?),
            Method::AgentStop => Self::AgentStop(params(raw)?),
            Method::AgentResume => Self::AgentResume(params(raw)?),
            Method::AgentRemove => Self::AgentRemove(params(raw)?),
            Method::AgentHistory => Self::AgentHistory(params(raw)?),
            Method::AgentDetails => Self::AgentDetails(params(raw)?),
            Method::AgentSetModel => Self::AgentSetModel(params(raw)?),
            Method::AgentSetEffort => Self::AgentSetEffort(params(raw)?),
            Method::AgentSetMode => Self::AgentSetMode(params(raw)?),
            Method::AgentRename => Self::AgentRename(params(raw)?),
            Method::AgentMove => Self::AgentMove(params(raw)?),
            Method::AgentContextUsage => Self::AgentContextUsage(params(raw)?),
            Method::AgentStopTask => Self::AgentStopTask(params(raw)?),
            Method::AgentMcpStatus => Self::AgentMcpStatus(params(raw)?),
            Method::AgentMcpReconnect => Self::AgentMcpReconnect(params(raw)?),
            Method::AgentMcpToggle => Self::AgentMcpToggle(params(raw)?),
            Method::AgentCommands => Self::AgentCommands(params(raw)?),
            Method::AgentReadFile => Self::AgentReadFile(params(raw)?),
            Method::McpRemove => Self::McpRemove(params(raw)?),
            Method::SettingsSet => Self::SettingsSet(params(raw)?),
            Method::SessionsList => Self::SessionsList,
            Method::SessionsResume => Self::SessionsResume(params(raw)?),
        };
        Ok(request)
    }
}

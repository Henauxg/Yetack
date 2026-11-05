use serde::{Deserialize, Serialize};

/// Claude Code's session id, which `--resume` takes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `uuid` Claude Code gives each message it writes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageUuid(String);

impl MessageUuid {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A tool call: its result, its approval request and its subagent's lines carry it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolUseId(String);

impl ToolUseId {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Pairs a control request with its response.
///
/// Claude Code picks the ids of its own requests, and the caller picks the ids of its requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ControlRequestId(String);

impl ControlRequestId {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A background task (a subagent or a command), as the `task_*` events and `stop_task` name it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One run of a hook, shared by its `hook_started`, `hook_progress` and `hook_response` events.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HookId(String);

impl HookId {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

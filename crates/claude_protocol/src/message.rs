use serde::Deserialize;
use serde_json::Value;

use crate::content_block;
use crate::content_block::ContentBlock;
use crate::ids::{MessageUuid, SessionId, ToolUseId};

/// An `assistant` line: one step of Claude's answer, often one content block.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantMessage {
    pub message: AssistantBody,
    /// Set on the lines of a subagent: the tool call that started it.
    pub parent_tool_use_id: Option<ToolUseId>,
    /// Why the API call failed, such as `rate_limit` or `authentication_failed`.
    pub error: Option<String>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

/// The API message inside an `assistant` line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantBody {
    pub model: Option<String>,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    /// The usage of this API call, so the context size Claude saw.
    pub usage: Option<Usage>,
}

/// A `user` line: a prompt Claude Code replays, or tool results.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserMessage {
    pub message: UserBody,
    /// Set on the lines of a subagent: the tool call that started it.
    pub parent_tool_use_id: Option<ToolUseId>,
    /// A prompt echoed by `--replay-user-messages`, which shows Claude Code took it.
    #[serde(default, rename = "isReplay")]
    pub is_replay: bool,
    /// Claude Code's own record of the tool's result, richer than the content block: a string,
    /// or an object whose shape depends on the tool.
    pub tool_use_result: Option<Value>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserBody {
    /// Text content is read as one text block.
    #[serde(deserialize_with = "content_block::text_or_blocks")]
    pub content: Vec<ContentBlock>,
}

/// Tokens of an API call, or of a whole turn in a `result` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

/// A `stream_event` line, sent with `--include-partial-messages`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEvent {
    /// The raw event of the Anthropic API's stream, such as a `content_block_delta`.
    pub event: Value,
    /// Set on the lines of a subagent: the tool call that started it.
    pub parent_tool_use_id: Option<ToolUseId>,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

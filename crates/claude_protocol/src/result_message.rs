use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::ids::{MessageUuid, SessionId, ToolUseId};
use crate::message::Usage;
use crate::open_word::OpenWord;
use crate::wire;

/// A `result` line, which ends a turn.
///
/// Losing it would leave the turn looking unfinished, so only `subtype` and `session_id` are
/// strict. Every other field reads as missing when it is `null` or holds a value it cannot read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResultMessage {
    pub subtype: OpenWord<ResultSubtype>,
    /// Can be true with the `success` subtype, when the turn ended on an API error.
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub is_error: bool,
    /// Why the turn ended; an interrupt gives `aborted_streaming` or `aborted_tools`.
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub terminal_reason: Option<OpenWord<TerminalReason>>,
    /// Claude's last text.
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub result: Option<String>,
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub errors: Vec<String>,
    /// The HTTP status of the API call that failed the turn.
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub api_error_status: Option<u16>,
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub stop_reason: Option<String>,
    #[serde(
        rename = "duration_ms",
        default,
        deserialize_with = "wire::milliseconds_or_none"
    )]
    pub duration: Option<Duration>,
    #[serde(
        rename = "duration_api_ms",
        default,
        deserialize_with = "wire::milliseconds_or_none"
    )]
    pub api_duration: Option<Duration>,
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub num_turns: u32,
    /// The cost since the `claude` process started, at API prices.
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub total_cost_usd: Option<f64>,
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub usage: Option<Usage>,
    /// The usage since the `claude` process started, by model name.
    #[serde(
        rename = "modelUsage",
        default,
        deserialize_with = "wire::default_on_error"
    )]
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub permission_denials: Vec<PermissionDenial>,
    pub session_id: Option<SessionId>,
    #[serde(default, deserialize_with = "wire::default_on_error")]
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum ResultSubtype {
    Success,
    ErrorMaxTurns,
    ErrorDuringExecution,
    ErrorMaxBudgetUsd,
    ErrorMaxStructuredOutputRetries,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum TerminalReason {
    Completed,
    MaxTurns,
    AbortedStreaming,
    AbortedTools,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    #[serde(rename = "costUSD")]
    pub cost_usd: f64,
    /// The model's context window in tokens, which the context percentage divides by.
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
}

/// A tool call the turn denied.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PermissionDenial {
    pub tool_name: String,
    pub tool_use_id: ToolUseId,
    #[serde(default)]
    pub tool_input: Map<String, Value>,
}

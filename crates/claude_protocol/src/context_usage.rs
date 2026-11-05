use serde::Deserialize;
use serde_json::Value;

use crate::error::ParseError;

/// The answer to `get_context_usage`: what `/context` shows.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    #[serde(default)]
    pub categories: Vec<ContextCategory>,
    pub total_tokens: u32,
    /// The usable context: the window less the autocompact buffer.
    pub max_tokens: u32,
    /// The model's context window.
    pub raw_max_tokens: Option<u32>,
    /// From 0 to 100.
    pub percentage: Option<f64>,
    pub model: Option<String>,
    #[serde(default)]
    pub is_auto_compact_enabled: bool,
    pub auto_compact_threshold: Option<u32>,
}

/// A share of the context, such as the system prompt or the messages.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCategory {
    pub name: String,
    pub tokens: u32,
    #[serde(default)]
    pub is_deferred: bool,
}

impl TryFrom<&Value> for ContextUsage {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        Self::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "context usage",
            error,
        })
    }
}

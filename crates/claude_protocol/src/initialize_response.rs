use serde::Deserialize;
use serde_json::Value;

use crate::error::ParseError;

/// The answer to `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InitializeResponse {
    /// The slash commands and skills the session offers.
    #[serde(default)]
    pub commands: Vec<SlashCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    /// Without the leading `/`.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Such as `[file]`. Empty when the command takes no argument.
    #[serde(default)]
    pub argument_hint: String,
}

impl TryFrom<&Value> for InitializeResponse {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        Self::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "initialize",
            error,
        })
    }
}

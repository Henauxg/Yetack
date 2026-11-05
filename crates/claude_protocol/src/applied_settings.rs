use serde::Deserialize;
use serde_json::Value;

use crate::parse_error::ParseError;

/// The answer to `get_settings`. Of it, the daemon reads what Claude Code applies to the session.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SettingsAnswer {
    #[serde(default)]
    pub applied: AppliedSettings,
}

/// What Claude Code applies, once it has resolved its settings files, its flags and the model's
/// own defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct AppliedSettings {
    pub model: Option<String>,
    /// The effort level, such as `xhigh`. `None` for a model without effort levels.
    pub effort: Option<String>,
}

impl TryFrom<&Value> for SettingsAnswer {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        Self::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "settings",
            error,
        })
    }
}

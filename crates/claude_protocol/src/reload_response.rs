use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::initialize_response::SlashCommand;
use crate::parse_error::ParseError;

/// The answer to `reload_skills`: Claude Code read its skill folders again.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReloadSkillsResponse {
    /// Every skill the session offers now, as slash commands. Required: an answer without it
    /// would empty the agent's skills.
    pub skills: Vec<SlashCommand>,
}

/// The answer to `reload_plugins`: Claude Code read its plugins again. The agents and MCP servers
/// it also lists are left out.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReloadPluginsResponse {
    /// Every slash command and skill the session offers now, the plugins' included. Required:
    /// an answer without it would empty the agent's commands.
    pub commands: Vec<SlashCommand>,
    #[serde(default)]
    pub plugins: Vec<Plugin>,
    /// How many errors loading the plugins gave.
    #[serde(default)]
    pub error_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Plugin {
    #[serde(default)]
    pub name: String,
    /// Where Claude Code loaded it from.
    #[serde(default)]
    pub path: PathBuf,
    /// The marketplace it comes from, such as `notes@team-tools`.
    pub source: Option<String>,
    pub version: Option<String>,
}

impl TryFrom<&Value> for ReloadSkillsResponse {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        Self::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "reload_skills",
            error,
        })
    }
}

impl TryFrom<&Value> for ReloadPluginsResponse {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        Self::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "reload_plugins",
            error,
        })
    }
}

use serde::Deserialize;
use serde_json::Value;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::open_word::OpenWord;
use crate::parse_error::ParseError;

/// The answer to `mcp_status`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    #[serde(default)]
    pub mcp_servers: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    pub status: OpenWord<McpServerState>,
    /// Set when the status is `failed`.
    pub error: Option<String>,
    pub config: Option<McpServerConfig>,
    pub scope: Option<OpenWord<McpScope>>,
    /// What the server said about itself when it connected.
    pub server_info: Option<McpServerInfo>,
    /// Listed when the server is connected.
    #[serde(default)]
    pub tools: Vec<McpTool>,
}

/// How Claude Code reaches the server. The environment and headers are left out, as they may
/// hold secrets.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerConfig {
    /// `None` means `stdio`, as older configurations leave it out.
    #[serde(rename = "type")]
    pub transport: Option<OpenWord<McpTransport>>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub annotations: McpToolAnnotations,
}

/// The server's hints about a tool. `None` means the server did not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "kebab-case")]
pub enum McpServerState {
    Connected,
    Failed,
    NeedsAuth,
    Pending,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "kebab-case")]
pub enum McpTransport {
    Stdio,
    Sse,
    Http,
    Sdk,
    ClaudeaiProxy,
}

/// Where the server's configuration comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "lowercase")]
pub enum McpScope {
    Local,
    User,
    Project,
    Claudeai,
    Managed,
}

impl TryFrom<&Value> for McpStatus {
    type Error = ParseError;

    fn try_from(body: &Value) -> Result<Self, ParseError> {
        Self::deserialize(body).map_err(|error| ParseError::MalformedResponse {
            body: "MCP status",
            error,
        })
    }
}

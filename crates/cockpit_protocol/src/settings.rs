use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Settings the daemon keeps for every app that connects.
///
/// A field missing from the JSON takes its default, so a settings file written before a field
/// existed still reads.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// MCP servers left out of the view. They keep running.
    pub hidden_mcp_servers: BTreeSet<String>,
}

//! The MCP server the daemon gives each agent's `claude`, which lets the agent read the terminals
//! the user shares with it. `yetack mcp` runs it.
//!
//! It speaks MCP over stdio: one JSON-RPC 2.0 message per line. It answers `initialize`, `ping`,
//! `tools/list` and `tools/call`, and takes `notifications/initialized` and the other
//! notifications without an answer. Its agent comes from `YETACK_AGENT`, which the daemon sets.
//! The daemon checks it again: a connection from under the daemon reads only the terminals of the
//! agent whose `claude` it descends from.

mod daemon;
mod server;

pub use daemon::{AgentEnvError, agent_from_env};
pub use server::serve;

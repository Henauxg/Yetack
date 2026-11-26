//! The daemon's MCP server for its agents, `yetack mcp`, which lets an agent read the terminals the
//! user shares with it. Each agent's `claude` gets it in `--mcp-config`, with its two tools
//! allowed, so that a read asks no approval.
//!
//! The daemon's caller gives the server's command ([`McpCommand`]), since only it knows which
//! program serves `mcp`. `yetack serve` gives its own program. The daemon in the app and the app's
//! background service give none, so their agents get no such server.

use std::ffi::OsString;
use std::path::PathBuf;

use cockpit_protocol::AgentId;
use serde_json::json;

/// The `yetack` subcommand that runs the MCP server.
pub const MCP_SUBCOMMAND: &str = "mcp";
/// The server's name in `claude`, which prefixes its tools: `mcp__yetack__read_terminal`.
pub const MCP_SERVER_NAME: &str = "yetack";
/// Names the agent whose terminals the MCP server reads.
pub const AGENT_ENV: &str = "YETACK_AGENT";
/// The tool that lists the terminals the agent may read.
pub const LIST_TERMINALS_TOOL: &str = "list_terminals";
/// The tool that reads a terminal's text.
pub const READ_TERMINAL_TOOL: &str = "read_terminal";

const MCP_CONFIG_FLAG: &str = "--mcp-config";
const ALLOWED_TOOLS_FLAG: &str = "--allowedTools";

/// The command that runs the daemon's MCP server, such as `yetack --state-dir <folder> mcp`. The
/// server must reach this daemon. It holds no secret, since other users can read a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// The MCP server each agent's `claude` gets, from an [`McpCommand`] whose words are all UTF-8,
/// which JSON needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentTools {
    program: String,
    args: Vec<String>,
}

impl AgentTools {
    /// `None` when a word of `command` is not UTF-8.
    #[cfg(unix)]
    pub(crate) fn new(command: McpCommand) -> Option<Self> {
        let McpCommand { program, args } = command;
        let program = program.into_os_string().into_string().ok();
        let args: Option<Vec<String>> =
            args.into_iter().map(|arg| arg.into_string().ok()).collect();
        let (Some(program), Some(args)) = (program, args) else {
            tracing::warn!("the MCP server's command is not UTF-8: agents cannot read terminals");
            return None;
        };
        Some(Self { program, args })
    }

    /// The arguments that give `agent`'s `claude` the server, with its tools allowed.
    pub(crate) fn claude_args(&self, agent: AgentId) -> Vec<OsString> {
        let config = json!({
            "mcpServers": {
                MCP_SERVER_NAME: {
                    "type": "stdio",
                    "command": self.program,
                    "args": self.args,
                    "env": { AGENT_ENV: agent.get().to_string() },
                }
            }
        });
        let tools = [LIST_TERMINALS_TOOL, READ_TERMINAL_TOOL]
            .map(|tool| format!("mcp__{MCP_SERVER_NAME}__{tool}"))
            .join(",");
        vec![
            OsString::from(format!("{MCP_CONFIG_FLAG}={config}")),
            OsString::from(format!("{ALLOWED_TOOLS_FLAG}={tools}")),
        ]
    }
}

// Only a daemon behind a socket gives its agents the server, and only on Unix.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn claude_gets_the_server_with_the_agent_and_its_tools_allowed() {
        let command = McpCommand {
            program: PathBuf::from("/opt/yetack/bin/yetack"),
            args: ["--state-dir", "/home/ada/.local/state/yetack", "mcp"]
                .map(OsString::from)
                .to_vec(),
        };
        let tools = AgentTools::new(command).unwrap();

        let args = tools.claude_args(AgentId::new(7));

        let args: Vec<&str> = args.iter().filter_map(|arg| arg.to_str()).collect();
        let [config, allowed] = args.as_slice() else {
            panic!("expected two arguments, got {args:?}");
        };
        let config: Value =
            serde_json::from_str(config.strip_prefix("--mcp-config=").unwrap()).unwrap();
        assert_eq!(
            config,
            json!({
                "mcpServers": {
                    "yetack": {
                        "type": "stdio",
                        "command": "/opt/yetack/bin/yetack",
                        "args": ["--state-dir", "/home/ada/.local/state/yetack", "mcp"],
                        "env": { "YETACK_AGENT": "7" },
                    }
                }
            })
        );
        assert_eq!(
            *allowed,
            "--allowedTools=mcp__yetack__list_terminals,mcp__yetack__read_terminal"
        );
    }

    #[test]
    fn a_command_that_is_not_utf8_gives_no_server() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let bad_program = McpCommand {
            program: PathBuf::from(OsStr::from_bytes(b"/opt/\xff/yetack")),
            args: vec![OsString::from("mcp")],
        };
        let bad_arg = McpCommand {
            program: PathBuf::from("/opt/yetack/bin/yetack"),
            args: vec![OsStr::from_bytes(b"/srv/\xff").to_owned()],
        };

        assert_eq!(AgentTools::new(bad_program), None);
        assert_eq!(AgentTools::new(bad_arg), None);
    }
}

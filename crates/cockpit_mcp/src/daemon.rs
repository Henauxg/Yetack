//! Where the server reads the terminals: the daemon behind its socket, as the agent the daemon
//! started the server for.

use std::env::VarError;

use cockpit_client::{Connection, RequestError, Transport};
use cockpit_daemon::AGENT_ENV;
use cockpit_protocol::{
    AgentId, AgentParams, Request, ResponseBody, TerminalId, TerminalInfo, TerminalReadParams,
};

/// Why the server has no agent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentEnvError {
    #[error(
        "{} is not set: the daemon starts this server for one of its agents",
        AGENT_ENV
    )]
    Missing,
    #[error("{} is not an agent id: {raw}", AGENT_ENV)]
    NotAnId { raw: String },
}

/// A terminal's text, as `terminal.read_text` answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalText {
    pub(crate) lines: Vec<String>,
    pub(crate) total_lines: u32,
}

/// Where the server reads the terminals: the daemon, or a fake one in tests.
pub(crate) trait Terminals {
    /// The terminals the agent may read.
    async fn list(&mut self) -> Result<Vec<TerminalInfo>, String>;
    /// The last `lines` lines of a terminal.
    async fn read(&mut self, terminal: TerminalId, lines: u32) -> Result<TerminalText, String>;
}

/// The daemon, reached through its socket. The connection opens at the first call and opens again
/// after a failed one, so a restarted daemon serves the next call.
pub(crate) struct Daemon {
    transport: Transport,
    agent: AgentId,
    connection: Option<Connection>,
}

impl Daemon {
    pub(crate) fn new(transport: Transport, agent: AgentId) -> Self {
        Self {
            transport,
            agent,
            connection: None,
        }
    }

    async fn request(&mut self, request: Request) -> Result<ResponseBody, String> {
        let connection = match self.connection.take() {
            Some(connection) => connection,
            None => Connection::connect(&self.transport, None)
                .await
                .map(|(connection, _)| connection)
                .map_err(|error| format!("Yetack's daemon cannot be reached: {error}"))?,
        };
        let answer = connection.request(request).await;
        if answer.is_ok() || matches!(answer, Err(RequestError::Failed(_))) {
            self.connection = Some(connection);
        }
        answer.map_err(|error| error.to_string())
    }
}

impl Terminals for Daemon {
    async fn list(&mut self) -> Result<Vec<TerminalInfo>, String> {
        let request = Request::TerminalListShared(AgentParams { agent: self.agent });
        match self.request(request).await? {
            ResponseBody::SharedTerminals { terminals } => Ok(terminals),
            other => Err(format!("unexpected answer from the daemon: {other:?}")),
        }
    }

    async fn read(&mut self, terminal: TerminalId, lines: u32) -> Result<TerminalText, String> {
        let request = Request::TerminalReadText(TerminalReadParams {
            agent: self.agent,
            terminal,
            lines,
        });
        match self.request(request).await? {
            ResponseBody::TerminalText { lines, total_lines } => {
                Ok(TerminalText { lines, total_lines })
            }
            other => Err(format!("unexpected answer from the daemon: {other:?}")),
        }
    }
}

/// The agent the daemon started the server for, from `YETACK_AGENT`.
pub fn agent_from_env() -> Result<AgentId, AgentEnvError> {
    let raw = match std::env::var(AGENT_ENV) {
        Ok(raw) => raw,
        Err(VarError::NotPresent) => return Err(AgentEnvError::Missing),
        Err(VarError::NotUnicode(raw)) => {
            return Err(AgentEnvError::NotAnId {
                raw: raw.to_string_lossy().into_owned(),
            });
        }
    };
    let id: u32 = raw.parse().map_err(|_| AgentEnvError::NotAnId { raw })?;
    Ok(AgentId::new(id))
}

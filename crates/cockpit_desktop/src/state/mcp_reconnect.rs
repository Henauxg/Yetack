//! A server's reconnect, as the server's page in the MCP block shows it. The daemon answers once
//! Claude Code tried, with no word on the outcome: the app then asks for the servers' status, and
//! says what the server's says.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use cockpit_protocol::{
    AgentId, AgentParams, McpServer, McpServerParams, McpServerStatus, Request,
};

use super::app_state::AppState;
use super::requests::{Outgoing, Purpose};
use crate::machine::MachineId;

/// How long "Reconnected" shows: the status chip says the rest.
const RECONNECTED_SHOWS: Duration = Duration::from_secs(5);

/// Where a reconnect stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconnect {
    /// Claude Code tries.
    Running,
    /// Claude Code tried: the app asks for the server's status. `asked` once the request left.
    Checking { asked: bool },
    /// Claude Code, or the daemon, refused the reconnect, for the reason given.
    Refused(String),
    /// The server is connected again, since `at`.
    Reconnected { at: Instant },
    /// The server did not connect: its status, and its error if it gives one.
    NotConnected {
        status: McpServerStatus,
        error: Option<String>,
    },
}

impl Reconnect {
    /// While Claude Code tries, or the app checks: the button waits.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Running | Self::Checking { .. })
    }
}

/// Each agent's reconnects, by server name.
pub type Reconnects = BTreeMap<(AgentId, String), Reconnect>;

/// The reconnect button: asks Claude Code to connect to the server again, unless it is already at
/// it.
pub fn start(state: &mut AppState, agent: AgentId, server: &str) -> Vec<Outgoing> {
    let key = (agent, server.to_owned());
    if state
        .ui
        .mcp_reconnects
        .get(&key)
        .is_some_and(Reconnect::is_busy)
    {
        return Vec::new();
    }
    state.ui.mcp_reconnects.insert(key, Reconnect::Running);
    vec![Outgoing {
        request: Request::AgentMcpReconnect(McpServerParams {
            agent,
            server: server.to_owned(),
        }),
        purpose: Purpose::McpReconnect {
            agent,
            server: server.to_owned(),
        },
        to: MachineId::THIS,
    }]
}

/// The daemon's answer to the reconnect: a refusal ends it, else the app checks the status.
pub fn answered(state: &mut AppState, agent: AgentId, server: &str, refusal: Option<String>) {
    let Some(reconnect) = state.ui.mcp_reconnects.get_mut(&(agent, server.to_owned())) else {
        return;
    };
    *reconnect = match refusal {
        Some(reason) => Reconnect::Refused(reason),
        None => Reconnect::Checking { asked: false },
    };
}

/// The status requests of the reconnects Claude Code finished.
pub fn status_requests(state: &mut AppState) -> Vec<Outgoing> {
    let mut requests = Vec::new();
    for ((agent, server), reconnect) in &mut state.ui.mcp_reconnects {
        if *reconnect == (Reconnect::Checking { asked: false }) {
            *reconnect = Reconnect::Checking { asked: true };
            requests.push(Outgoing {
                request: Request::AgentMcpStatus(AgentParams { agent: *agent }),
                purpose: Purpose::McpReconnected {
                    agent: *agent,
                    server: server.clone(),
                },
                to: MachineId::THIS,
            });
        }
    }
    requests
}

/// The servers' status after the reconnect, or why the daemon could not give it.
pub fn checked(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    servers: Result<&[McpServer], String>,
    now: Instant,
) {
    let Some(reconnect) = state.ui.mcp_reconnects.get_mut(&(agent, server.to_owned())) else {
        return;
    };
    *reconnect = match servers {
        Err(reason) => Reconnect::Refused(reason),
        Ok(servers) => match servers.iter().find(|listed| listed.name == server) {
            Some(listed) if listed.status == McpServerStatus::Connected => {
                Reconnect::Reconnected { at: now }
            }
            Some(listed) => Reconnect::NotConnected {
                status: listed.status,
                error: listed.error.clone(),
            },
            None => Reconnect::Refused("Claude Code no longer lists this server.".to_owned()),
        },
    };
}

/// What the server's page shows of its last reconnect at `now`: "Reconnected" leaves after a few
/// seconds, and an outcome the server's status no longer says, as a server that connected since,
/// leaves at once.
pub fn shown(state: &AppState, agent: AgentId, server: &str, now: Instant) -> Option<Reconnect> {
    let reconnect = state.ui.mcp_reconnects.get(&(agent, server.to_owned()))?;
    let current = state
        .agents
        .get(&agent)
        .and_then(|entry| entry.details.loaded())
        .and_then(|details| {
            details
                .mcp_servers
                .iter()
                .find(|listed| listed.name == server)
        })
        .map(|listed| listed.status);
    match reconnect {
        Reconnect::Reconnected { at }
            if now.saturating_duration_since(*at) >= RECONNECTED_SHOWS
                || current.is_some_and(|status| status != McpServerStatus::Connected) =>
        {
            None
        }
        Reconnect::NotConnected { status, .. } if current.is_some_and(|now| now != *status) => None,
        reconnect => Some(reconnect.clone()),
    }
}

/// Leaving a server's page forgets the outcomes shown there. A reconnect still running stays, so
/// that coming back shows it.
pub fn forget_ended(state: &mut AppState, agent: AgentId) {
    state
        .ui
        .mcp_reconnects
        .retain(|(owner, _), reconnect| *owner != agent || reconnect.is_busy());
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::{state_with, summary};

    const SERVER: &str = "search";

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn server(status: McpServerStatus, error: Option<&str>) -> McpServer {
        McpServer {
            name: SERVER.to_owned(),
            status,
            error: error.map(str::to_owned),
            version: None,
            transport: None,
            scope: None,
            tools: Vec::new(),
        }
    }

    fn now_shown(state: &AppState, now: Instant) -> Option<Reconnect> {
        shown(state, agent(), SERVER, now)
    }

    #[test]
    fn a_reconnect_runs_then_checks_the_status_then_says_it_connected() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let now = Instant::now();

        assert_eq!(start(&mut state, agent(), SERVER).len(), 1);
        assert!(start(&mut state, agent(), SERVER).is_empty());
        assert_eq!(now_shown(&state, now), Some(Reconnect::Running));

        answered(&mut state, agent(), SERVER, None);
        let asked = status_requests(&mut state);
        assert!(matches!(
            asked.as_slice(),
            [Outgoing {
                request: Request::AgentMcpStatus(_),
                ..
            }]
        ));
        assert!(status_requests(&mut state).is_empty());

        let servers = [server(McpServerStatus::Connected, None)];
        checked(&mut state, agent(), SERVER, Ok(&servers), now);
        assert_eq!(
            now_shown(&state, now),
            Some(Reconnect::Reconnected { at: now })
        );
        assert_eq!(now_shown(&state, now + RECONNECTED_SHOWS), None);
    }

    #[test]
    fn a_server_still_down_says_its_status_and_error() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let now = Instant::now();
        start(&mut state, agent(), SERVER);
        answered(&mut state, agent(), SERVER, None);
        status_requests(&mut state);

        let servers = [server(McpServerStatus::Failed, Some("connection refused"))];
        checked(&mut state, agent(), SERVER, Ok(&servers), now);

        assert_eq!(
            now_shown(&state, now + RECONNECTED_SHOWS),
            Some(Reconnect::NotConnected {
                status: McpServerStatus::Failed,
                error: Some("connection refused".to_owned()),
            })
        );
    }

    #[test]
    fn a_refusal_shows_its_reason_until_the_page_is_left() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        start(&mut state, agent(), SERVER);

        answered(
            &mut state,
            agent(),
            SERVER,
            Some("The agent exited.".to_owned()),
        );
        assert_eq!(
            now_shown(&state, Instant::now()),
            Some(Reconnect::Refused("The agent exited.".to_owned()))
        );
        assert!(status_requests(&mut state).is_empty());

        forget_ended(&mut state, agent());
        assert_eq!(now_shown(&state, Instant::now()), None);
        // A refused reconnect can be tried again.
        assert_eq!(start(&mut state, agent(), SERVER).len(), 1);
    }

    #[test]
    fn leaving_the_page_keeps_a_reconnect_that_still_runs() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        start(&mut state, agent(), SERVER);

        forget_ended(&mut state, agent());

        assert_eq!(now_shown(&state, Instant::now()), Some(Reconnect::Running));
    }

    #[test]
    fn an_outcome_leaves_once_the_servers_status_says_otherwise() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let now = Instant::now();
        state.agents.get_mut(&agent()).unwrap().details =
            crate::state::Loadable::Loaded(cockpit_protocol::AgentDetails {
                mcp_servers: vec![server(McpServerStatus::Failed, Some("refused"))],
                ..cockpit_protocol::AgentDetails::default()
            });
        start(&mut state, agent(), SERVER);
        answered(&mut state, agent(), SERVER, None);
        let servers = [server(McpServerStatus::Failed, Some("refused"))];
        checked(&mut state, agent(), SERVER, Ok(&servers), now);
        assert!(matches!(
            now_shown(&state, now),
            Some(Reconnect::NotConnected { .. })
        ));

        // Claude Code connected it on its own since.
        if let Some(details) = state.agents.get_mut(&agent()).unwrap().details.loaded_mut() {
            details.mcp_servers = vec![server(McpServerStatus::Connected, None)];
        }

        assert_eq!(now_shown(&state, now), None);
    }

    #[test]
    fn switching_agents_forgets_the_outcomes_and_keeps_a_running_reconnect() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        start(&mut state, agent(), SERVER);
        start(&mut state, agent(), "tickets");
        answered(
            &mut state,
            agent(),
            "tickets",
            Some("The agent exited.".to_owned()),
        );

        state.ui.leave_agent();

        assert_eq!(now_shown(&state, Instant::now()), Some(Reconnect::Running));
        assert_eq!(shown(&state, agent(), "tickets", Instant::now()), None);
    }
}

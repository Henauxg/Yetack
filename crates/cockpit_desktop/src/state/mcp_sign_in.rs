//! A server's OAuth sign-in, from its page in the MCP block.
//!
//! 1. The app sends `agent.mcp_sign_in`. Claude Code answers with the link of the server's sign-in
//!    page, which the app opens in the browser.
//! 2. Once the user approves, the browser goes to `http://localhost:<port>/callback?code=…`.
//!    - For an agent on this computer, Claude Code listens there and takes the code itself.
//!    - For an agent on another machine, that address points to this computer. The app listens on
//!      the port and passes the address on with `agent.mcp_sign_in_callback`. When the port is
//!      taken here, the user pastes the address from the browser instead.
//! 3. Claude Code then connects to the server. The app asks for the servers' status every few
//!    seconds until the server connects.
//!
//! The sign-in stays in the account's folder on the agent's machine. New agents of the account
//! use it, and running ones once they reconnect.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use cockpit_protocol::{
    AgentId, AgentParams, McpCallbackParams, McpServer, McpServerParams, McpServerStatus,
    McpSignIn, Request,
};
use url::Url;

use super::app_state::AppState;
use super::requests::{Outgoing, Purpose};
use crate::daemon_label::DaemonPlace;
use crate::machine::MachineId;

/// How often the app asks for the servers' status while it waits for the server to connect.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// How long the app waits for the user in the browser.
const BROWSER_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long the server may take to connect once Claude Code holds the token.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// How long "Signed in" shows: the status chip says the rest.
const SIGNED_IN_SHOWS: Duration = Duration::from_secs(5);
/// The query parameter that ties the browser's address to its sign-in.
const STATE_PARAM: &str = "state";

/// Where a sign-in stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignIn {
    /// Claude Code prepares the sign-in page.
    Starting,
    /// The browser shows the server's sign-in page.
    InBrowser(Waiting),
    /// The app passes the browser's address to Claude Code, which asks the server for the token.
    Finishing,
    /// Claude Code holds the token and connects to the server, since `since`.
    Connecting {
        since: Instant,
        poll: Poll,
        reconnect: ReconnectBy,
    },
    /// The server connected, at `at`.
    SignedIn { at: Instant },
    /// Claude Code holds the token, but the server did not connect: its status, and its error if
    /// it gives one. `None` when the server is not listed.
    NotConnected {
        status: Option<McpServerStatus>,
        error: Option<String>,
    },
    /// The sign-in stopped, for the reason given.
    Failed(String),
}

impl SignIn {
    /// Until the server connects or the sign-in ends otherwise.
    pub fn is_running(&self) -> bool {
        !matches!(
            self,
            Self::SignedIn { .. } | Self::NotConnected { .. } | Self::Failed(_)
        )
    }
}

/// Who reconnects the server once Claude Code holds the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectBy {
    /// The daemon, which answered once Claude Code tried: the next status is the outcome.
    Daemon,
    /// Claude Code, on its own and later: the app waits for a while.
    Claude,
}

/// The sign-in page is open, and the app waits for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub url: String,
    pub finish: Finish,
    /// When the link came. It names this attempt, so a listener of an earlier one stops.
    pub since: Instant,
    /// Whether the app opened the link in the browser.
    pub opened: bool,
    pub poll: Poll,
}

/// What gets the browser's address once the user approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The sign-in ends in the browser, as for a claude.ai connector: no address comes back.
    Connector,
    /// Claude Code, on this computer, listens for it.
    ClaudeListens,
    /// The app listens on this computer's `port` for an agent on another machine.
    AppListens { port: u16, listener: Listener },
    /// Nothing listens: the user pastes the address, for the reason given.
    Paste { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    ToStart,
    Started,
}

/// The status requests of a sign-in: when the last one left, and whether its answer came.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Poll {
    last: Option<Instant>,
    answered: bool,
}

impl Poll {
    fn due(&self, now: Instant) -> bool {
        match self.last {
            None => true,
            Some(last) => self.answered && now.saturating_duration_since(last) >= POLL_EVERY,
        }
    }
}

/// Each agent's sign-ins, by server name.
pub type SignIns = BTreeMap<(AgentId, String), SignIn>;

/// What the app does outside the state for the sign-ins: see [`tick`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// The links to open in the browser.
    pub open: Vec<String>,
    pub listen: Vec<Listen>,
}

/// A listener for the browser's address to start on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    pub agent: AgentId,
    pub server: String,
    pub port: u16,
    pub since: Instant,
    /// The `state` of the sign-in link, which the browser's address must carry.
    pub state: Option<String>,
}

/// The "Sign in" button: asks Claude Code for the sign-in page, unless a sign-in runs already.
pub fn start(state: &mut AppState, agent: AgentId, server: &str) -> Vec<Outgoing> {
    let key = (agent, server.to_owned());
    if state
        .ui
        .mcp_sign_ins
        .get(&key)
        .is_some_and(SignIn::is_running)
    {
        return Vec::new();
    }
    state.ui.mcp_sign_ins.insert(key, SignIn::Starting);
    vec![Outgoing {
        request: Request::AgentMcpSignIn(McpServerParams {
            agent,
            server: server.to_owned(),
        }),
        purpose: Purpose::McpSignIn {
            agent,
            server: server.to_owned(),
        },
        to: MachineId::THIS,
    }]
}

/// Claude Code's answer to the start, or why there is none.
pub fn started(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    answer: Result<McpSignIn, String>,
    now: Instant,
) {
    // Machine 0 may be a daemon reached over SSH, whose agents run on another computer.
    let on_this_computer = state
        .machines
        .get(MachineId::of(agent))
        .is_some_and(|machine| machine.daemon.place != DaemonPlace::Ssh);
    let Some(sign_in) = state.ui.mcp_sign_ins.get_mut(&(agent, server.to_owned())) else {
        return;
    };
    if *sign_in != SignIn::Starting {
        return;
    }
    let waiting = |url, finish| {
        SignIn::InBrowser(Waiting {
            url,
            finish,
            since: now,
            opened: false,
            poll: Poll::default(),
        })
    };
    *sign_in = match answer {
        Err(reason) => SignIn::Failed(reason),
        Ok(McpSignIn::NotNeeded) => SignIn::Connecting {
            since: now,
            poll: Poll::default(),
            reconnect: ReconnectBy::Claude,
        },
        Ok(McpSignIn::InBrowser { url }) => waiting(url, Finish::Connector),
        Ok(McpSignIn::Callback { url, port }) => {
            let finish = match port {
                _ if on_this_computer => Finish::ClaudeListens,
                Some(port) => Finish::AppListens {
                    port,
                    listener: Listener::ToStart,
                },
                None => Finish::Paste {
                    reason: "Claude Code did not say where the browser goes back.".to_owned(),
                },
            };
            waiting(url, finish)
        }
    };
}

/// Whether [`tick`] has something to do at `now`. A loop calls it often, and `tick` only when it
/// says so, as a change of the state renders the views again.
pub fn is_due(state: &AppState, now: Instant) -> bool {
    state.ui.mcp_sign_ins.values().any(|sign_in| match sign_in {
        SignIn::InBrowser(waiting) => {
            !waiting.opened
                || matches!(
                    waiting.finish,
                    Finish::AppListens {
                        listener: Listener::ToStart,
                        ..
                    }
                )
                || waiting.poll.due(now)
                || now.saturating_duration_since(waiting.since) >= BROWSER_WAIT
        }
        SignIn::Connecting { since, poll, .. } => {
            poll.due(now) || now.saturating_duration_since(*since) >= CONNECT_WAIT
        }
        SignIn::Starting
        | SignIn::Finishing
        | SignIn::SignedIn { .. }
        | SignIn::NotConnected { .. }
        | SignIn::Failed(_) => false,
    })
}

/// Whether a sign-in still runs, for the loop that calls [`tick`].
pub fn any_running(state: &AppState) -> bool {
    state.ui.mcp_sign_ins.values().any(SignIn::is_running)
}

/// Moves the sign-ins on at `now`: the status requests due, the waits that ran out, and the links
/// and listeners the app has to open.
pub fn tick(state: &mut AppState, now: Instant) -> (Vec<Outgoing>, Effects) {
    let mut requests = Vec::new();
    let mut effects = Effects::default();
    // Read before the loop, which borrows the sign-ins mutably.
    let mut not_connected_outcomes: BTreeMap<(AgentId, String), SignIn> = state
        .ui
        .mcp_sign_ins
        .keys()
        .map(|(agent, server)| {
            let listed = listed_server(state, *agent, server);
            ((*agent, server.clone()), not_connected(listed))
        })
        .collect();
    for ((agent, server), sign_in) in &mut state.ui.mcp_sign_ins {
        let poll = match sign_in {
            SignIn::InBrowser(waiting) => {
                if now.saturating_duration_since(waiting.since) >= BROWSER_WAIT {
                    *sign_in = SignIn::Failed(
                        "No sign-in came back from the browser in 10 minutes.".to_owned(),
                    );
                    continue;
                }
                if !waiting.opened {
                    waiting.opened = true;
                    effects.open.push(waiting.url.clone());
                }
                if let Finish::AppListens { port, listener } = &mut waiting.finish
                    && *listener == Listener::ToStart
                {
                    *listener = Listener::Started;
                    effects.listen.push(Listen {
                        agent: *agent,
                        server: server.clone(),
                        port: *port,
                        since: waiting.since,
                        state: state_of(&waiting.url),
                    });
                }
                &mut waiting.poll
            }
            SignIn::Connecting { since, .. }
                if now.saturating_duration_since(*since) >= CONNECT_WAIT =>
            {
                *sign_in = not_connected_outcomes
                    .remove(&(*agent, server.clone()))
                    .unwrap_or_else(|| not_connected(None));
                continue;
            }
            SignIn::Connecting { poll, .. } => poll,
            SignIn::Starting
            | SignIn::Finishing
            | SignIn::SignedIn { .. }
            | SignIn::NotConnected { .. }
            | SignIn::Failed(_) => {
                continue;
            }
        };
        if poll.due(now) {
            *poll = Poll {
                last: Some(now),
                answered: false,
            };
            requests.push(Outgoing {
                request: Request::AgentMcpStatus(AgentParams { agent: *agent }),
                purpose: Purpose::McpSignInStatus {
                    agent: *agent,
                    server: server.clone(),
                    details_seq: state.agents.get(agent).and_then(|entry| entry.details_seq),
                },
                to: MachineId::THIS,
            });
        }
    }
    (requests, effects)
}

fn not_connected(listed: Option<&McpServer>) -> SignIn {
    SignIn::NotConnected {
        status: listed.map(|listed| listed.status),
        error: listed.and_then(|listed| listed.error.clone()),
    }
}

/// The servers' status during a sign-in, or why the daemon could not give it.
pub fn polled(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    servers: Result<&[McpServer], String>,
    now: Instant,
) {
    let Some(sign_in) = state.ui.mcp_sign_ins.get_mut(&(agent, server.to_owned())) else {
        return;
    };
    let (poll, reconnect) = match sign_in {
        SignIn::InBrowser(waiting) => (&mut waiting.poll, None),
        SignIn::Connecting {
            poll, reconnect, ..
        } => (poll, Some(*reconnect)),
        SignIn::Starting
        | SignIn::Finishing
        | SignIn::SignedIn { .. }
        | SignIn::NotConnected { .. }
        | SignIn::Failed(_) => {
            return;
        }
    };
    poll.answered = true;
    // A failed status request leaves the wait as it is: the next one may work.
    let Ok(servers) = servers else {
        return;
    };
    let listed = servers.iter().find(|listed| listed.name == server);
    let status = listed.map(|listed| listed.status);
    if status == Some(McpServerStatus::Connected) {
        *sign_in = SignIn::SignedIn { at: now };
    } else if reconnect == Some(ReconnectBy::Daemon) && status != Some(McpServerStatus::Pending) {
        *sign_in = not_connected(listed);
    }
}

/// The app's listener could not take the port: the user pastes the address instead.
pub fn listen_failed(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    since: Instant,
    reason: String,
) {
    if let Some(SignIn::InBrowser(waiting)) =
        state.ui.mcp_sign_ins.get_mut(&(agent, server.to_owned()))
        && waiting.since == since
    {
        waiting.finish = Finish::Paste { reason };
    }
}

/// Whether the listener of the attempt that started at `since` still has to wait.
pub fn still_listening(state: &AppState, agent: AgentId, server: &str, since: Instant) -> bool {
    matches!(
        state.ui.mcp_sign_ins.get(&(agent, server.to_owned())),
        Some(SignIn::InBrowser(waiting))
            if waiting.since == since && matches!(waiting.finish, Finish::AppListens { .. })
    )
}

/// The browser's address reached the app's listener of the attempt that started at `since`.
pub fn received(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    since: Instant,
    callback_url: String,
) -> Vec<Outgoing> {
    if !still_listening(state, agent, server, since) {
        return Vec::new();
    }
    finish(state, agent, server, callback_url)
}

/// The address the user pasted, once [`check_pasted`] accepted it.
pub fn pasted(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    callback_url: String,
) -> Vec<Outgoing> {
    let waiting = matches!(
        state.ui.mcp_sign_ins.get(&(agent, server.to_owned())),
        Some(SignIn::InBrowser(waiting)) if waiting.finish != Finish::Connector
    );
    if !waiting {
        return Vec::new();
    }
    finish(state, agent, server, callback_url)
}

fn finish(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    callback_url: String,
) -> Vec<Outgoing> {
    state
        .ui
        .mcp_sign_ins
        .insert((agent, server.to_owned()), SignIn::Finishing);
    vec![Outgoing {
        request: Request::AgentMcpSignInCallback(McpCallbackParams {
            agent,
            server: server.to_owned(),
            callback_url,
        }),
        purpose: Purpose::McpSignInCallback {
            agent,
            server: server.to_owned(),
        },
        to: MachineId::THIS,
    }]
}

/// The `state` of a sign-in link or of the address the browser goes back to, if it has one.
pub fn state_of(address: &str) -> Option<String> {
    let url = Url::parse(address).ok()?;
    url.query_pairs()
        .find(|(name, _)| name == STATE_PARAM)
        .map(|(_, value)| value.into_owned())
}

/// The pasted text, trimmed, when it is an address that carries the browser's answer: an
/// authorization code, or the server's refusal.
pub fn check_pasted(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    let url = Url::parse(text).map_err(|_| "This is not a web address.")?;
    let answers = url
        .query_pairs()
        .any(|(name, _)| name == "code" || name == "error");
    if !answers {
        return Err(
            "This address has no authorization code. Copy the whole address of the page the browser went to after you approved.",
        );
    }
    Ok(text.to_owned())
}

/// Claude Code's answer to the address: it holds the token and connects, or it refused.
pub fn callback_answered(
    state: &mut AppState,
    agent: AgentId,
    server: &str,
    refusal: Option<String>,
    now: Instant,
) {
    let Some(sign_in) = state.ui.mcp_sign_ins.get_mut(&(agent, server.to_owned())) else {
        return;
    };
    if *sign_in != SignIn::Finishing {
        return;
    }
    *sign_in = match refusal {
        Some(reason) => SignIn::Failed(reason),
        None => SignIn::Connecting {
            since: now,
            poll: Poll::default(),
            reconnect: ReconnectBy::Daemon,
        },
    };
}

/// "Cancel": the app stops waiting, and its listener stops. Claude Code drops the sign-in it
/// started when the next one starts.
pub fn cancel(state: &mut AppState, agent: AgentId, server: &str) {
    state.ui.mcp_sign_ins.remove(&(agent, server.to_owned()));
}

/// What the server's page shows of its sign-in at `now`. "Signed in" leaves after a few seconds,
/// or as soon as the server's status no longer says it is connected. "Not connected" leaves once
/// the server connects, as after a reconnect.
pub fn shown(state: &AppState, agent: AgentId, server: &str, now: Instant) -> Option<SignIn> {
    let sign_in = state.ui.mcp_sign_ins.get(&(agent, server.to_owned()))?;
    let connected = listed_server(state, agent, server)
        .map(|listed| listed.status == McpServerStatus::Connected);
    match sign_in {
        SignIn::SignedIn { at }
            if now.saturating_duration_since(*at) >= SIGNED_IN_SHOWS
                || connected == Some(false) =>
        {
            None
        }
        SignIn::NotConnected { .. } if connected == Some(true) => None,
        sign_in => Some(sign_in.clone()),
    }
}

/// Leaving a server's page forgets the outcomes shown there. A sign-in still running stays.
pub fn forget_ended(state: &mut AppState, agent: AgentId) {
    state
        .ui
        .mcp_sign_ins
        .retain(|(owner, _), sign_in| *owner != agent || sign_in.is_running());
}

fn listed_server<'state>(
    state: &'state AppState,
    agent: AgentId,
    server: &str,
) -> Option<&'state McpServer> {
    state
        .agents
        .get(&agent)
        .and_then(|entry| entry.details.loaded())
        .and_then(|details| {
            details
                .mcp_servers
                .iter()
                .find(|listed| listed.name == server)
        })
}

#[cfg(test)]
mod tests {
    use cockpit_client::RequestError;
    use cockpit_protocol::{AgentDetails, AgentStatus, ErrorBody, ErrorCode, Method, ResponseBody};

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::drafts::Drafts;
    use crate::state::{Loadable, apply_response, state_with, summary};

    const SERVER: &str = "wiki";
    const URL: &str = "https://wiki.example/authorize?state=s1";
    const CALLBACK: &str = "http://localhost:50123/callback?code=c1&state=s1";

    fn local() -> AgentId {
        AgentId::new(1)
    }

    /// An agent on another machine.
    fn remote() -> AgentId {
        MachineId::new(1).app_agent(AgentId::new(1))
    }

    fn listed(status: McpServerStatus) -> McpServer {
        McpServer {
            name: SERVER.to_owned(),
            status,
            error: None,
            version: None,
            transport: None,
            scope: None,
            tools: Vec::new(),
        }
    }

    fn callback_answer() -> McpSignIn {
        McpSignIn::Callback {
            url: URL.to_owned(),
            port: Some(50123),
        }
    }

    /// A state whose daemon for this computer runs here, as a socket.
    fn local_state() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.daemon = DaemonLabel::socket();
        state
    }

    /// A state whose sign-in for `agent` shows the page since `at`.
    fn in_browser(agent: AgentId, at: Instant) -> AppState {
        let mut state = local_state();
        start(&mut state, agent, SERVER);
        started(&mut state, agent, SERVER, Ok(callback_answer()), at);
        state
    }

    fn sign_in(state: &AppState, agent: AgentId) -> Option<&SignIn> {
        state.ui.mcp_sign_ins.get(&(agent, SERVER.to_owned()))
    }

    fn callbacks(requests: &[Outgoing]) -> Vec<&str> {
        requests
            .iter()
            .filter_map(|outgoing| match &outgoing.request {
                Request::AgentMcpSignInCallback(params) => Some(params.callback_url.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_remote_agents_sign_in_opens_the_page_and_listens_on_this_computer() {
        let at = Instant::now();
        let mut state = in_browser(remote(), at);

        let (requests, effects) = tick(&mut state, at);

        assert_eq!(effects.open, [URL.to_owned()]);
        assert_eq!(
            effects.listen,
            [Listen {
                agent: remote(),
                server: SERVER.to_owned(),
                port: 50123,
                since: at,
                state: Some("s1".to_owned()),
            }]
        );
        assert!(matches!(
            requests.as_slice(),
            [Outgoing {
                request: Request::AgentMcpStatus(_),
                ..
            }]
        ));
        assert_eq!(tick(&mut state, at), (Vec::new(), Effects::default()));
    }

    #[test]
    fn a_local_agents_sign_in_leaves_the_browsers_answer_to_claude_code() {
        let at = Instant::now();
        let mut state = in_browser(local(), at);

        let (_, effects) = tick(&mut state, at);

        assert_eq!(effects.open, [URL.to_owned()]);
        assert_eq!(effects.listen, []);
        assert!(matches!(
            sign_in(&state, local()),
            Some(SignIn::InBrowser(Waiting {
                finish: Finish::ClaudeListens,
                ..
            }))
        ));
    }

    #[test]
    fn an_agent_of_a_daemon_reached_over_ssh_gets_the_apps_listener() {
        let at = Instant::now();
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.daemon = DaemonLabel::ssh("build-box".to_owned());
        start(&mut state, local(), SERVER);
        started(&mut state, local(), SERVER, Ok(callback_answer()), at);

        let (_, effects) = tick(&mut state, at);

        assert_eq!(effects.listen.len(), 1, "{effects:?}");
    }

    #[test]
    fn the_status_is_asked_every_few_seconds_until_the_server_connects() {
        let at = Instant::now();
        let mut state = in_browser(local(), at);
        tick(&mut state, at);
        polled(
            &mut state,
            local(),
            SERVER,
            Ok(&[listed(McpServerStatus::NeedsAuth)]),
            at,
        );

        assert!(!is_due(&state, at + Duration::from_secs(1)));
        let (requests, _) = tick(&mut state, at + POLL_EVERY);
        assert_eq!(requests.len(), 1);

        let done = at + POLL_EVERY;
        polled(
            &mut state,
            local(),
            SERVER,
            Ok(&[listed(McpServerStatus::Connected)]),
            done,
        );
        assert_eq!(
            sign_in(&state, local()),
            Some(&SignIn::SignedIn { at: done })
        );
        assert!(!any_running(&state));
    }

    #[test]
    fn the_listener_passes_the_browsers_address_to_claude_code() {
        let at = Instant::now();
        let mut state = in_browser(remote(), at);
        tick(&mut state, at);

        let requests = received(&mut state, remote(), SERVER, at, CALLBACK.to_owned());

        assert_eq!(callbacks(&requests), [CALLBACK]);
        assert_eq!(sign_in(&state, remote()), Some(&SignIn::Finishing));
        callback_answered(&mut state, remote(), SERVER, None, at);
        assert!(matches!(
            sign_in(&state, remote()),
            Some(SignIn::Connecting { .. })
        ));
    }

    #[test]
    fn the_listener_of_an_earlier_attempt_passes_nothing() {
        let earlier = Instant::now();
        let later = earlier + Duration::from_secs(3);
        let mut state = in_browser(remote(), earlier);
        cancel(&mut state, remote(), SERVER);
        start(&mut state, remote(), SERVER);
        started(&mut state, remote(), SERVER, Ok(callback_answer()), later);

        let requests = received(&mut state, remote(), SERVER, earlier, CALLBACK.to_owned());

        assert_eq!(callbacks(&requests), Vec::<&str>::new());
        assert!(!still_listening(&state, remote(), SERVER, earlier));
        assert!(still_listening(&state, remote(), SERVER, later));
    }

    #[test]
    fn a_taken_port_asks_for_the_address_instead() {
        let at = Instant::now();
        let mut state = in_browser(remote(), at);
        tick(&mut state, at);

        listen_failed(&mut state, remote(), SERVER, at, "Port taken.".to_owned());

        assert!(!still_listening(&state, remote(), SERVER, at));
        assert!(matches!(
            sign_in(&state, remote()),
            Some(SignIn::InBrowser(Waiting {
                finish: Finish::Paste { reason },
                ..
            })) if reason == "Port taken."
        ));
        let requests = pasted(&mut state, remote(), SERVER, CALLBACK.to_owned());
        assert_eq!(callbacks(&requests), [CALLBACK]);
    }

    #[test]
    fn the_state_reads_from_a_link_or_an_address() {
        assert_eq!(
            state_of("https://wiki.example/authorize?scope=a&state=s%2F1"),
            Some("s/1".to_owned())
        );
        assert_eq!(state_of("http://localhost:50123/callback?code=c1"), None);
    }

    #[test]
    fn only_an_address_with_the_browsers_answer_can_be_pasted() {
        assert_eq!(
            check_pasted(&format!("  {CALLBACK}\n")),
            Ok(CALLBACK.to_owned())
        );
        assert!(check_pasted("http://localhost:50123/callback?error=access_denied").is_ok());
        assert!(check_pasted("http://localhost:50123/callback?state=s1").is_err());
        assert!(check_pasted("code=c1").is_err());
    }

    #[test]
    fn claude_codes_refusal_of_the_address_ends_the_sign_in() {
        let at = Instant::now();
        let mut state = in_browser(local(), at);
        pasted(&mut state, local(), SERVER, CALLBACK.to_owned());

        callback_answered(
            &mut state,
            local(),
            SERVER,
            Some("OAuth state mismatch.".to_owned()),
            at,
        );

        assert_eq!(
            sign_in(&state, local()),
            Some(&SignIn::Failed("OAuth state mismatch.".to_owned()))
        );
    }

    #[test]
    fn the_wait_for_the_browser_runs_out() {
        let at = Instant::now();
        let mut state = in_browser(local(), at);
        tick(&mut state, at);

        assert!(is_due(&state, at + BROWSER_WAIT));
        tick(&mut state, at + BROWSER_WAIT);

        assert!(matches!(sign_in(&state, local()), Some(SignIn::Failed(_))));
    }

    #[test]
    fn a_server_that_does_not_connect_after_the_sign_in_says_its_status() {
        let at = Instant::now();
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        start(&mut state, local(), SERVER);
        started(&mut state, local(), SERVER, Ok(McpSignIn::NotNeeded), at);
        tick(&mut state, at);
        polled(
            &mut state,
            local(),
            SERVER,
            Ok(&[listed(McpServerStatus::Pending)]),
            at,
        );

        tick(&mut state, at + CONNECT_WAIT);

        assert_eq!(
            sign_in(&state, local()),
            Some(&SignIn::NotConnected {
                status: None,
                error: None,
            })
        );
    }

    #[test]
    fn the_status_after_the_daemons_reconnect_is_the_outcome() {
        let at = Instant::now();
        let mut state = in_browser(remote(), at);
        received(&mut state, remote(), SERVER, at, CALLBACK.to_owned());
        callback_answered(&mut state, remote(), SERVER, None, at);
        tick(&mut state, at);

        polled(
            &mut state,
            remote(),
            SERVER,
            Ok(&[listed(McpServerStatus::NeedsAuth)]),
            at,
        );

        assert_eq!(
            sign_in(&state, remote()),
            Some(&SignIn::NotConnected {
                status: Some(McpServerStatus::NeedsAuth),
                error: None,
            })
        );
        assert!(!any_running(&state));
    }

    #[test]
    fn claude_codes_own_reconnect_gets_time_to_finish() {
        let at = Instant::now();
        let mut state = local_state();
        start(&mut state, local(), SERVER);
        started(&mut state, local(), SERVER, Ok(McpSignIn::NotNeeded), at);
        tick(&mut state, at);

        polled(
            &mut state,
            local(),
            SERVER,
            Ok(&[listed(McpServerStatus::NeedsAuth)]),
            at,
        );

        assert!(matches!(
            sign_in(&state, local()),
            Some(SignIn::Connecting { .. })
        ));
    }

    #[test]
    fn not_connected_leaves_once_the_server_connects() {
        let mut state = local_state();
        state.ui.mcp_sign_ins.insert(
            (local(), SERVER.to_owned()),
            SignIn::NotConnected {
                status: Some(McpServerStatus::NeedsAuth),
                error: None,
            },
        );
        let set = |state: &mut AppState, status| {
            let details = AgentDetails {
                mcp_servers: vec![listed(status)],
                ..AgentDetails::default()
            };
            if let Some(entry) = state.agents.get_mut(&local()) {
                entry.details = Loadable::Loaded(details);
            }
        };

        set(&mut state, McpServerStatus::NeedsAuth);
        assert!(shown(&state, local(), SERVER, Instant::now()).is_some());
        set(&mut state, McpServerStatus::Connected);
        assert_eq!(shown(&state, local(), SERVER, Instant::now()), None);
    }

    #[test]
    fn an_older_daemon_asks_for_an_update() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        start(&mut state, local(), SERVER);
        let message = Method::unknown_message(Method::AgentMcpSignIn.as_str());

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentMcpSignIn,
            Purpose::McpSignIn {
                agent: local(),
                server: SERVER.to_owned(),
            },
            Err(RequestError::Failed(ErrorBody::new(
                ErrorCode::InvalidRequest,
                message,
            ))),
            MachineId::THIS,
        );

        assert!(matches!(
            sign_in(&state, local()),
            Some(SignIn::Failed(reason)) if reason.contains("update it")
        ));
        let answered = Ok(ResponseBody::McpSignIn(McpSignIn::NotNeeded));
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentMcpSignIn,
            Purpose::McpSignIn {
                agent: local(),
                server: SERVER.to_owned(),
            },
            answered,
            MachineId::THIS,
        );
        assert!(matches!(sign_in(&state, local()), Some(SignIn::Failed(_))));
    }
}

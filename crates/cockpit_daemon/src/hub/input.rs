use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use cockpit_protocol::{
    AccountId, AccountState, AgentId, CostUsd, ErrorBody, Frame, PermissionMode, Repository,
    Request, ResponseResult, Seq, ShellChoice, SignIn, SlashCommand, Snapshot, TerminalId,
    Timestamp,
};
use tokio::sync::{mpsc, oneshot};

use crate::blocking::BlockingError;
use crate::hub::folders::ForkOf;
use crate::hub::start::{FolderRequest, Start};
use crate::hub::terminals::SpawnedShell;
use crate::process::{ProcessEvent, Spawned};
use crate::sessions::FoundSession;
use crate::store::DetailsFrame;

/// Where the answer to a client's request goes.
pub(crate) type Reply = oneshot::Sender<ResponseResult>;

/// What the hub's other tasks send it.
pub(crate) enum HubInput {
    /// A client passed the handshake. The reply registers it for live events.
    Connect {
        last_seq: Option<Seq>,
        reply: oneshot::Sender<Registration>,
    },
    Request {
        request: Request,
        reply: Reply,
    },
    /// From the process task of one of an agent's processes, `generation` telling which.
    Process {
        agent: AgentId,
        generation: u32,
        event: ProcessEvent<ControlPurpose>,
    },
    /// `sessions.resume` found the session on disk, and read its transcript.
    SessionFound {
        found: Box<FoundSession>,
        /// The session's folder, resolved off the hub: a new agent for the session starts there.
        folder: Result<PathBuf, ErrorBody>,
        /// When the request came, from which a closed agent's choices count their time.
        asked_at: Instant,
        reply: Reply,
    },
    /// `accounts.list` read the accounts' folders. The hub answers once the agents that waited
    /// for a sign-in that happened since no longer wait.
    AccountsChecked {
        accounts: Vec<AccountState>,
        logins: Vec<AccountLogin>,
        reply: Reply,
    },
    /// The folder the app sent with `agent.start`, `agent.fork` or `agent.change_folder`, resolved
    /// off the hub.
    FolderResolved {
        folder: Result<PathBuf, ErrorBody>,
        request: FolderRequest,
        reply: Reply,
    },
    /// A task checked an agent's folder and started `claude` in it, or could not.
    ProcessStarted {
        start: Box<Start>,
        started: Result<Spawned<ControlPurpose>, ErrorBody>,
    },
    /// `agent.fork` copied the session to the fork's folder, and read its transcript there.
    ForkCopied {
        fork: Box<ForkOf>,
        found: Result<Box<FoundSession>, String>,
        reply: Reply,
    },
    /// `agent.change_folder` copied the agent's session to its new folder, or failed to.
    MoveCopied {
        agent: AgentId,
        folder: PathBuf,
        /// The agent ran before the move, and runs again after it.
        restart: bool,
        copied: Result<(), String>,
        reply: Reply,
    },
    /// The periodic check read the accounts' folders. `None` when it could not run.
    SignInsChecked(Option<SignInCheck>),
    /// The cost Claude Code restored as one of the agent's processes resumed its session.
    RestoredCost {
        agent: AgentId,
        generation: u32,
        cost: CostUsd,
    },
    /// A task found the shells a terminal can run, for `agent.terminals`.
    ShellsFound {
        agent: AgentId,
        shells: Vec<ShellChoice>,
        reply: Reply,
    },
    /// A task started a terminal's shell in its agent's folder, or could not.
    TerminalSpawned {
        agent: AgentId,
        terminal: TerminalId,
        spawned: Result<SpawnedShell, ErrorBody>,
        reply: Reply,
    },
    /// A terminal's shell wrote `data`.
    TerminalOutput {
        terminal: TerminalId,
        data: Vec<u8>,
    },
    /// A terminal's shell ended, with its exit code when it has one.
    TerminalExited {
        terminal: TerminalId,
        code: Option<i32>,
    },
    /// An agent's details change that waited for its turn may go out.
    DetailsDue {
        agent: AgentId,
    },
    /// The repository of `folder`, an agent's folder, read off the hub.
    RepositoryRead {
        folder: PathBuf,
        read: Result<Option<Repository>, BlockingError>,
    },
}

/// Whether each account is signed in, and when its login was written.
pub(crate) struct SignInCheck {
    pub(crate) sign_ins: BTreeMap<AccountId, SignIn>,
    pub(crate) logins: Vec<AccountLogin>,
}

/// When an account's login was last written.
pub(crate) struct AccountLogin {
    pub(crate) account: AccountId,
    pub(crate) written: Timestamp,
}

/// A client's place among the hub's clients.
pub(crate) struct Registration {
    pub(crate) joined: Joined,
    /// Live events, each after the last one the catch-up covers. The hub drops a client whose
    /// queue is full, which ends this channel.
    pub(crate) frames: mpsc::Receiver<Frame>,
}

/// How the client catches up.
pub(crate) enum Joined {
    /// The logs in `events` hold every event after `after`, up to `upto`, but the details:
    /// `details` holds each agent's latest `DetailsChanged` frame and its sequence number.
    Replay {
        after: Seq,
        upto: Option<Seq>,
        events: PathBuf,
        details: Vec<DetailsFrame>,
    },
    Snapshot(Box<Snapshot>),
}

/// Why the hub sent a control request to Claude Code, and where the answer goes.
#[derive(Debug)]
pub(crate) enum ControlPurpose {
    Initialize,
    /// The context usage for the summary, after a start or a turn.
    ContextRefresh,
    /// The effort Claude Code applies to an agent that was given none, after a start or a model
    /// change.
    DefaultEffort,
    /// A request answered by `Done`.
    Done(Reply),
    /// The user's stop, answered by `Done`. Its answer lists the prompts Claude Code still holds.
    Interrupt(Reply),
    SetModel {
        model: String,
        reply: Reply,
    },
    /// `reply` is `None` for the mode change after an approved plan.
    SetMode {
        mode: PermissionMode,
        reply: Option<Reply>,
    },
    ContextUsage(Reply),
    /// `reply` is `None` for a refresh after an MCP change.
    McpStatus(Option<Reply>),
    /// A reconnect or a toggle, answered by `Done` and followed by a refresh.
    McpChanged(Reply),
    /// The start of a sign-in, answered by `McpSignIn`.
    McpSignIn(Reply),
    /// The end of a sign-in. Claude Code then holds the token but does not reconnect the server
    /// when the code came this way, so the daemon asks it to, and answers `Done` once it tried.
    McpSignInFinished {
        server: String,
        reply: Reply,
    },
    /// The reconnect after a sign-in, answered by `Done` whatever its outcome: the token is kept,
    /// and the refresh that follows says whether the server connected.
    McpSignInReconnect(Reply),
    /// `agent.reload_skills`, answered by `SkillsReloaded`. `before` holds the commands as the
    /// request leaves: Claude Code sends the changed ones before it answers.
    ReloadSkills {
        reply: Reply,
        before: Vec<SlashCommand>,
    },
    /// `agent.reload_plugins`, answered by `PluginsReloaded`, with `before` as for `ReloadSkills`.
    ReloadPlugins {
        reply: Reply,
        before: Vec<SlashCommand>,
    },
    /// The interrupt that ends a working agent's turn as it closes. The agent is gone when the
    /// answer comes.
    Closing,
}

use std::collections::BTreeMap;
use std::path::PathBuf;

use cockpit_protocol::{
    AccountId, AccountState, AgentId, CostUsd, Frame, PermissionMode, Request, ResponseResult, Seq,
    SignIn, Snapshot, Timestamp,
};
use tokio::sync::{mpsc, oneshot};

use crate::hub::folders::ForkOf;
use crate::process::ProcessEvent;
use crate::sessions::FoundSession;

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
        reply: Reply,
    },
    /// `accounts.list` read the accounts' folders. The hub answers once the agents that waited
    /// for a sign-in that happened since no longer wait.
    AccountsChecked {
        accounts: Vec<AccountState>,
        logins: Vec<AccountLogin>,
        reply: Reply,
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
    /// The logs in `events` hold every event after `after`, up to `upto`.
    Replay {
        after: Seq,
        upto: Option<Seq>,
        events: PathBuf,
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
    /// A reconnect or toggle, answered by `Done` and followed by a refresh.
    McpChanged(Reply),
    /// The interrupt that ends a working agent's turn as it closes. The agent is gone when the
    /// answer comes.
    Closing,
}

use std::path::PathBuf;

use cockpit_protocol::{AgentId, Frame, PermissionMode, Request, ResponseResult, Seq, Snapshot};
use tokio::sync::{mpsc, oneshot};

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
    /// The cost Claude Code restores with the session one of an agent's processes resumed.
    RestoredCost {
        agent: AgentId,
        generation: u32,
        cost: f64,
    },
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
}

//! The messages of a connection.
//!
//! 1. The client sends `Hello`.
//! 2. The daemon answers `Welcome`, or `Refused` when the protocol versions differ.
//! 3. The client sends `Request`s; the daemon sends `Response`s and `Event`s, in any order.

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::agent::AgentSummary;
use crate::event::EventRecord;
use crate::ids::{RequestId, Seq};
use crate::request::Request;
use crate::response::ResponseResult;
use crate::settings::Settings;
use crate::usage::{AccountPlanUsage, PlanUsage};

/// Changes whenever a message changes shape. Client and daemon speak only the same version.
pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello {
        protocol_version: u16,
        /// The last sequence number this client saw, from an earlier connection.
        last_seq: Option<Seq>,
    },
    Request {
        id: RequestId,
        request: Request,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        protocol_version: u16,
        catchup: Catchup,
    },
    /// The daemon closes the connection after it.
    Refused {
        reason: String,
    },
    Response {
        id: RequestId,
        result: ResponseResult,
    },
    Event(EventRecord),
}

/// How a client catches up after `Welcome`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Catchup {
    /// The daemon sends every event after the client's `last_seq`, then live events. The client
    /// keeps its state and applies them.
    Replay,
    /// The client replaces its state with the snapshot. Live events after `snapshot.last_seq`
    /// follow.
    Snapshot(Snapshot),
}

/// The daemon's state, without the transcripts: a client asks for those with `agent.history`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub agents: Vec<AgentSummary>,
    /// The default account's plan usage.
    pub plan_usage: PlanUsage,
    pub settings: Settings,
    /// `None` before the daemon's first event.
    pub last_seq: Option<Seq>,
    /// Every account, the default one first. Empty from a daemon that predates accounts, which
    /// has the default account alone.
    #[serde(default)]
    pub accounts: Vec<Account>,
    /// The plan usage of each account other than the default one, once one of its agents
    /// reported it.
    #[serde(default)]
    pub other_plan_usage: Vec<AccountPlanUsage>,
}

//! The messages of a connection.
//!
//! 1. The client sends `Hello`.
//! 2. The daemon answers `Welcome`, or `Refused` when the protocol versions differ.
//! 3. The client sends `Request`s; the daemon sends `Response`s and `Event`s, in any order.

use std::fmt;

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
pub const PROTOCOL_VERSION: u16 = 10;

/// How many characters of a commit id git shows in its short form.
const SHORT_COMMIT_CHARS: usize = 7;

/// A program's release, its Cargo version: "0.1.0". Every build between two releases carries the
/// earlier one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReleaseVersion(String);

impl ReleaseVersion {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The git commit a program was built from: its full id, as `git rev-parse HEAD` writes it. Two
/// builds of one release can come from different commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BuildCommit(String);

impl BuildCommit {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 7 characters, as git shows a commit.
    #[must_use]
    pub fn short(&self) -> &str {
        self.0.get(..SHORT_COMMIT_CHARS).unwrap_or(&self.0)
    }
}

impl fmt::Display for BuildCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

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
        daemon_version: ReleaseVersion,
        /// The commit of the program that runs the daemon. `None` when its build did not know it.
        daemon_commit: Option<BuildCommit>,
        catchup: Catchup,
    },
    /// The daemon closes the connection after it. Its shape stays the same in every protocol, so
    /// that a client of another protocol reads it.
    Refused {
        reason: String,
        /// The daemon's release and commit, so that a client can say which daemon refused it.
        /// Absent from a daemon of protocol 8 or older.
        #[serde(default)]
        daemon_version: Option<ReleaseVersion>,
        #[serde(default)]
        daemon_commit: Option<BuildCommit>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_shows_short_as_git_does() {
        let full = BuildCommit::new("3f9c2e1a7b4d5c6e8f9a0b1c2d3e4f5a6b7c8d9e".to_owned());
        let shorter = BuildCommit::new("3f9c2".to_owned());

        assert_eq!(full.short(), "3f9c2e1");
        assert_eq!(shorter.short(), "3f9c2");
    }
}

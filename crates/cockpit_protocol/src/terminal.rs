//! Terminals: shells the daemon runs on a PTY in an agent's folder, which a client shows and types
//! into.
//!
//! Their output does not go out as events: events are numbered, logged and replayed, and a shell
//! can write megabytes a second. A [`TerminalUpdate`] goes to every client as it comes, and a
//! client that missed some asks `terminal.attach` for the output the daemon keeps.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::AgentId;

/// A terminal of the daemon, numbered from 1 for the daemon's life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(u32);

impl TerminalId {
    #[must_use]
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }

    /// The id after this one, or `None` past `u32::MAX`.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// A terminal's size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

/// The size a terminal took once its shell had written `offset` bytes in all. A client that shows
/// the kept output again writes each part at the size the shell wrote it for, so that its cursor
/// moves land where they did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSizeAt {
    pub offset: u64,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOpenParams {
    pub agent: AgentId,
    pub size: TerminalSize,
    /// The program of one of the shells `agent.terminals` lists. `None` runs the default one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<PathBuf>,
}

/// A shell the daemon found on its machine, which a terminal can run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellChoice {
    /// The program, by its path on the daemon's machine.
    pub program: PathBuf,
    /// What the app shows, such as "PowerShell 7" or "zsh".
    pub name: String,
    /// Whether a terminal opened without a shell runs this one.
    pub default: bool,
}

/// Names a terminal. `agent` routes the request to the agent's daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalParams {
    pub agent: AgentId,
    pub terminal: TerminalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInputParams {
    pub agent: AgentId,
    pub terminal: TerminalId,
    /// What the user typed, as the terminal encodes it: text and escape sequences.
    pub data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResizeParams {
    pub agent: AgentId,
    pub terminal: TerminalId,
    pub size: TerminalSize,
}

/// A terminal as `agent.terminals` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub terminal: TerminalId,
    /// The shell's name, such as "zsh".
    pub title: String,
    /// `Some` once the shell ended.
    pub exited: Option<TerminalExit>,
}

/// How a terminal's shell ended. It is a struct, not an `Option`, since JSON writes `Some(None)`
/// and `None` both as `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalExit {
    /// `None` when it ended without one, as on a signal.
    pub code: Option<i32>,
}

/// What happened in a terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalUpdate {
    pub agent: AgentId,
    pub terminal: TerminalId,
    pub change: TerminalChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalChange {
    /// A client opened it.
    Opened { title: String },
    /// The shell wrote `data`, base64, which starts at byte `offset` of everything it wrote.
    Output { offset: u64, data: String },
    /// The shell ended. The terminal stays, with its output, until a client closes it.
    Exited { code: Option<i32> },
    /// A client closed it.
    Closed,
}

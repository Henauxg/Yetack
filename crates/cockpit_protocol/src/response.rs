//! The daemon's answers to requests.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::account::{AccountState, EnvironmentApiKey};
use crate::details::{AgentDetails, ContextUsage, McpServer, SlashCommand};
use crate::ids::{AccountId, AgentId, ClaudeSessionId};
use crate::machine_load::MachineLoad;
use crate::time::Timestamp;
use crate::transcript::TranscriptItem;

/// The result of one request: a body, or an error. The connection stays open either way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseResult {
    Ok(ResponseBody),
    Error(ErrorBody),
}

impl ResponseResult {
    pub fn into_result(self) -> Result<ResponseBody, ErrorBody> {
        match self {
            Self::Ok(body) => Ok(body),
            Self::Error(error) => Err(error),
        }
    }
}

impl From<Result<ResponseBody, ErrorBody>> for ResponseResult {
    fn from(result: Result<ResponseBody, ErrorBody>) -> Self {
        match result {
            Ok(body) => Self::Ok(body),
            Err(error) => Self::Error(error),
        }
    }
}

/// `Request` says which body answers each method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBody {
    /// The request was carried out and returns nothing.
    Done,
    AgentStarted {
        agent: AgentId,
    },
    History(HistoryPage),
    Details(AgentDetails),
    ContextUsage(ContextUsage),
    McpServers {
        servers: Vec<McpServer>,
    },
    Commands {
        commands: Vec<SlashCommand>,
    },
    /// `path` is where the file was read, absolute: the path asked for, or the file the daemon
    /// found for it. `None` from a daemon that predates it, which reads only the path asked for.
    File {
        #[serde(default)]
        path: Option<PathBuf>,
        text: String,
    },
    /// For `agent.read_file`, several files match the relative path asked for, and none was
    /// read. For `agent.find_files`, the files found. Absolute paths.
    FileMatches {
        matches: Vec<PathBuf>,
    },
    Directory(DirListing),
    Sessions {
        sessions: Vec<PastSession>,
    },
    MachineLoad(MachineLoad),
    /// Every account, the default one first.
    Accounts {
        accounts: Vec<AccountState>,
        /// Whether the daemon's environment has `ANTHROPIC_API_KEY`, which an account that uses
        /// it passes to its agents.
        environment_api_key: EnvironmentApiKey,
    },
    /// The account added or changed.
    Account(AccountState),
}

/// A folder's entries: its folders first, then its other entries, each group by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirListing {
    /// The folder listed, absolute: the path asked for, joined to the agent's folder when relative
    /// for `agent.list_dir`.
    pub path: PathBuf,
    pub entries: Vec<DirEntry>,
    pub completeness: Completeness,
}

/// Whether a listing holds every entry of the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    Complete,
    /// The folder holds more entries than the listing's cap: some are left out.
    Truncated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    /// What the entry is, or for a symbolic link, what it points to.
    pub kind: EntryKind,
    /// In bytes, for a file.
    pub size: Option<u64>,
    pub link: EntryLink,
}

/// Whether a folder entry is a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum EntryLink {
    /// The entry is the folder, file or other thing its kind names.
    Direct,
    /// The entry is a symbolic link to what its kind names.
    SymbolicLink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Folder,
    File,
    /// A socket, a device, a FIFO, or a symbolic link that leads nowhere.
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryPage {
    /// Oldest first.
    pub items: Vec<TranscriptItem>,
    pub older_items: OlderItems,
}

/// Whether a transcript holds items older than a history page's first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum OlderItems {
    Exist,
    None,
}

/// A Claude session on the daemon's machine, which `sessions.resume` can bring back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PastSession {
    pub id: ClaudeSessionId,
    pub title: String,
    pub folder: PathBuf,
    pub last_activity: Timestamp,
    pub turns: u32,
    /// The account whose folder holds the session. The default account when missing, as from a
    /// daemon that predates accounts.
    #[serde(default)]
    pub account: AccountId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// For a person to read.
    pub message: String,
}

impl ErrorBody {
    #[must_use]
    pub fn new(code: ErrorCode, message: String) -> Self {
        Self { code, message }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request does not parse, or its parameters do not fit, such as a question answer to a
    /// pending approval.
    InvalidRequest,
    /// The agent, pending answer, queued message, task, MCP server, session or file it names
    /// does not exist, or the process it names is not one of the agent's.
    NotFound,
    /// The request needs a running `claude` process: `agent.resume` first.
    AgentExited,
    /// The request needs an exited agent, such as `agent.resume` or `agent.remove`.
    AgentRunning,
    /// The file is too large to send, or is not text.
    FileUnreadable,
    /// Claude Code answered with an error, or could not start.
    ClaudeFailed,
    /// Claude Code did not answer in time.
    Timeout,
    /// The daemon's machine cannot do it, such as measuring its load without `/proc`.
    Unsupported,
    /// The account cannot go: an agent uses it.
    AccountInUse,
    /// A fault in the daemon.
    Internal,
}

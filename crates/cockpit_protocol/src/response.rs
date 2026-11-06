//! The daemon's answers to requests.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::details::{AgentDetails, ContextUsage, McpServer, SlashCommand};
use crate::ids::{AgentId, ClaudeSessionId};
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
    /// found for it.
    File {
        path: PathBuf,
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
}

/// A folder's entries: its folders first, then its other entries, each group by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirListing {
    /// The folder listed, absolute: the path asked for, joined to the agent's folder when relative.
    pub path: PathBuf,
    pub entries: Vec<DirEntry>,
    /// The folder holds more entries than the listing's cap: some are left out.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    /// What the entry is, or for a symbolic link, what it points to.
    pub kind: EntryKind,
    /// In bytes, for a file.
    pub size: Option<u64>,
    pub symlink: bool,
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
    /// Whether items older than the first one exist.
    pub has_older: bool,
}

/// A Claude session on the daemon's machine, which `sessions.resume` can bring back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PastSession {
    pub id: ClaudeSessionId,
    pub title: String,
    pub folder: PathBuf,
    pub last_activity: Timestamp,
    pub turns: u32,
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
    /// does not exist.
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
    /// A fault in the daemon.
    Internal,
}

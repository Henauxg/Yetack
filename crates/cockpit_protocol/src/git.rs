//! What the daemon reads of git for the app: a file's last committed text, its blame and its
//! history, a commit's change to it, and the state of a checkout. Every request only reads.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::AgentId;
use crate::repository::Repository;
use crate::response::Completeness;
use crate::time::Timestamp;

/// The most commits `agent.git_log` lists.
pub const MAX_LOG_COMMITS: u16 = 50;
/// The most files `agent.git_status` lists per checkout.
pub const MAX_STATUS_FILES: usize = 2_000;
/// The most unpushed commits `agent.git_status` lists per checkout.
pub const MAX_UNPUSHED_COMMITS: usize = 100;
/// The most bytes of a commit's message, past its title, that a [`Commit`] carries.
pub const MAX_COMMIT_BODY_BYTES: usize = 2_048;

/// A commit's full hash, as git writes it: 40 hexadecimal digits, or 64 in a SHA-256
/// repository. A message that holds anything else does not read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct CommitHash(String);

impl From<CommitHash> for String {
    fn from(hash: CommitHash) -> Self {
        hash.0
    }
}

impl TryFrom<String> for CommitHash {
    type Error = NotAHash;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(&text).ok_or(NotAHash)
    }
}

/// A text that is not a commit's full hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a commit's full hash: 40 or 64 hexadecimal digits")]
pub struct NotAHash;

impl CommitHash {
    /// `None` unless `text` is 40 or 64 hexadecimal digits. A hash from a client never reaches
    /// git as an option or a revision expression.
    pub fn new(text: &str) -> Option<Self> {
        let is_hash =
            matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        is_hash.then(|| Self(text.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its first 7 digits, as people quote a commit.
    pub fn short(&self) -> &str {
        self.0.get(..7).unwrap_or(&self.0)
    }
}

impl fmt::Display for CommitHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A commit as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub hash: CommitHash,
    /// The first line of its message.
    pub title: String,
    /// The rest of its message, at most [`MAX_COMMIT_BODY_BYTES`]. Empty where the answer leaves
    /// it out, as in a log.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    pub author: String,
    /// When it was authored.
    pub time: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LineCounts {
    pub added: u32,
    pub removed: u32,
}

/// For the git requests on one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileParams {
    pub agent: AgentId,
    /// Absolute: the path `agent.read_file` answered with.
    pub path: PathBuf,
}

/// For `agent.git_show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitShowParams {
    pub agent: AgentId,
    /// Absolute: the path `agent.read_file` answered with.
    pub path: PathBuf,
    pub commit: CommitHash,
}

/// A file's last committed text, which the app compares with the file on disk. Answers
/// `agent.git_file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GitFile {
    /// The file is in no git checkout, or git is not installed.
    Outside,
    /// The checkout's last commit holds no such file: it is new since. `head` is `None` in a
    /// checkout without a commit.
    New { head: Option<Commit> },
    /// The file's text in the last commit.
    Committed { head: Commit, text: String },
    /// The committed file is binary, or larger than the file view shows.
    NotText { head: Commit },
}

/// A run of lines of the last committed text that one commit last changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameRun {
    /// The first line, from 1, in the committed text.
    pub start: u32,
    pub count: u32,
    /// Its index in the answer's `commits`.
    pub commit: u32,
}

/// Who last changed each line of the last committed text. The app maps it onto the file on disk
/// with its diff, which also tells the lines not committed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBlame {
    /// The commit blamed: the one whose text `agent.git_file` gave, or a later one.
    pub head: CommitHash,
    /// Each commit the runs name, once, with its body.
    pub commits: Vec<Commit>,
    /// In the order of the text, without gaps.
    pub runs: Vec<BlameRun>,
}

/// A commit that changed a file, newest first in a log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub commit: Commit,
    /// `None` for a binary file, or for a merge, which shows no diff.
    pub counts: Option<LineCounts>,
}

/// The file before and after one commit, for its diff. Answers `agent.git_show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitFile {
    pub commit: Commit,
    pub texts: CommitTexts,
    /// Whether the remote [`CheckoutState::remote`] names holds the commit.
    pub on_remote: OnRemote,
}

/// Whether a checkout's remote held a commit at the last fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum OnRemote {
    /// One of its branches held it.
    Held,
    /// It may lack it: none of its branches held it, git could not say, or the checkout has no
    /// remote.
    MayLack,
}

/// The file's text on each side of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommitTexts {
    Text {
        /// In the commit's first parent. `None` when the commit added the file, or for a first
        /// commit.
        before: Option<String>,
        /// `None` when the commit deleted the file.
        after: Option<String>,
    },
    /// The file is binary or too large to show, on one side or both.
    NotText,
}

/// How a file differs from the last commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Modified,
    /// Staged as new.
    Added,
    Deleted,
    Renamed,
    /// New, and not tracked yet.
    Untracked,
    /// In a merge or a rebase that stopped on it.
    Conflicted,
}

/// A file that differs from the last commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStatus {
    /// Absolute.
    pub path: PathBuf,
    pub change: FileChange,
    /// Against the last commit, staged and not. `None` for a binary file, or one the daemon did
    /// not count.
    pub counts: Option<LineCounts>,
}

/// The branch a checkout is on, and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchState {
    /// `None` when the checkout is on no branch (a detached HEAD).
    pub name: Option<String>,
    /// `None` in a checkout without a commit.
    pub head: Option<Commit>,
    /// The branch it pushes to and pulls from, when it has one.
    pub upstream: Option<Upstream>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    /// As git names it: `origin/main`.
    pub name: String,
    /// Commits to push.
    pub ahead: u32,
    /// Commits to pull.
    pub behind: u32,
}

/// One git checkout: the agent's folder's, or one the agent worked in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutState {
    /// Its top folder, absolute.
    pub root: PathBuf,
    /// Its `origin` remote, else its first. `None` without a remote.
    pub remote: Option<Remote>,
    pub kind: CheckoutKind,
    pub branch: BranchState,
    /// The files that differ from the last commit, at most [`MAX_STATUS_FILES`], by path.
    pub files: Vec<FileStatus>,
    /// `Truncated` when more files differ than `files` lists.
    pub completeness: Completeness,
}

/// A checkout's remote, for links to its repository's web pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    /// As the config names it: `origin`.
    pub name: String,
    pub repository: Repository,
    /// The commits of HEAD that none of the remote's branches held at the last fetch, newest
    /// first, at most [`MAX_UNPUSHED_COMMITS`]: the repository's web pages lack them.
    pub unpushed: Vec<CommitHash>,
    /// `Truncated` when HEAD has more such commits than `unpushed` lists, or when the daemon
    /// could not list them: any commit may then be unpushed.
    pub unpushed_completeness: Completeness,
}

/// Which of a repository's checkouts one is, as git names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutKind {
    /// The checkout that holds the `.git` folder.
    MainWorktree,
    /// A checkout made with `git worktree add`, beside the main one.
    LinkedWorktree,
}

/// Answers `agent.git_status`: the checkout of the agent's folder first, if it is in one, then
/// the other checkouts the agent read or changed a file in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub checkouts: Vec<CheckoutState>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_hash_is_40_or_64_hexadecimal_digits() {
        let sha1 = "9F8E7D6C5B4A39281706F5E4D3C2B1A098765432";

        assert_eq!(
            CommitHash::new(sha1).map(|hash| hash.short().to_owned()),
            Some("9f8e7d6".to_owned())
        );
        assert!(CommitHash::new(&"a".repeat(64)).is_some());
        assert_eq!(
            CommitHash::new("9f8e7d6"),
            None,
            "a short hash is ambiguous"
        );
        assert_eq!(CommitHash::new(&format!("--{}", &sha1[2..])), None);
        assert_eq!(CommitHash::new(&"g".repeat(40)), None);
    }

    #[test]
    fn a_commit_hash_read_from_a_message_is_checked_too() {
        let read: Result<CommitHash, _> = serde_json::from_str("\"HEAD\"");
        let upper: Result<CommitHash, _> = serde_json::from_str(&format!("\"{}\"", "A".repeat(40)));

        assert!(read.is_err(), "a client cannot send a revision expression");
        assert_eq!(
            upper.ok().map(|hash| hash.to_string()),
            Some("a".repeat(40))
        );
    }
}

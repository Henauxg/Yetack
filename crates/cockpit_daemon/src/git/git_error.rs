use std::io;
use std::path::PathBuf;

use cockpit_protocol::{CommitHash, ErrorBody, ErrorCode};
use tokio::task::JoinError;

use crate::read_file::ReadFileError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitError {
    #[error("the path goes up with `..`")]
    GoesUp,
    /// The checks of `agent.read_file` refuse the path.
    #[error(transparent)]
    Access(#[from] ReadFileError),
    #[error("{} is in no git checkout", .0.display())]
    Outside(PathBuf),
    #[error("the checkout {} has no commit yet", .0.display())]
    NoCommit(PathBuf),
    #[error("{} is not in the last commit", .0.display())]
    NotCommitted(PathBuf),
    #[error("no commit {} in the checkout", .0.short())]
    NoSuchCommit(CommitHash),
    #[error("the committed file is larger than {0} bytes")]
    FileTooLarge(u64),
    /// git takes a setting's name up to its first `=`, so the daemon cannot turn this filter off.
    #[error("the checkout {} defines a git filter whose name the daemon cannot turn off: {name}", .checkout.display())]
    Filter { checkout: PathBuf, name: String },
    #[error("git is not installed")]
    NotInstalled,
    #[error("cannot run git: {0}")]
    Start(#[source] io::Error),
    #[error("git did not answer in time")]
    TimedOut,
    #[error("git's answer is larger than {0} bytes")]
    TooLarge(usize),
    /// git's last line of error output, or its exit code.
    #[error("git failed: {0}")]
    Failed(String),
    /// Names what does not parse.
    #[error("git's answer does not parse: {0}")]
    Unparsable(&'static str),
    #[error("cannot read git's answer: {0}")]
    Read(#[source] io::Error),
    #[error("the request's task failed: {0}")]
    Task(#[from] JoinError),
}

impl GitError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::GoesUp => ErrorCode::InvalidRequest,
            Self::Access(error) => return error.to_error_body(),
            Self::Outside(_)
            | Self::NoCommit(_)
            | Self::NotCommitted(_)
            | Self::NoSuchCommit(_) => ErrorCode::NotFound,
            Self::FileTooLarge(_) | Self::TooLarge(_) => ErrorCode::FileUnreadable,
            Self::Filter { .. } | Self::NotInstalled => ErrorCode::Unsupported,
            Self::TimedOut => ErrorCode::Timeout,
            Self::Start(_)
            | Self::Failed(_)
            | Self::Unparsable(_)
            | Self::Read(_)
            | Self::Task(_) => ErrorCode::Internal,
        };
        ErrorBody::new(code, self.to_string())
    }
}

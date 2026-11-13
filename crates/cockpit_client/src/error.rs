//! Why connecting, a connection or a request failed.

use std::io;
use std::path::PathBuf;

use cockpit_protocol::{BuildCommit, ErrorBody, FrameError, PROTOCOL_VERSION, ReleaseVersion};

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("cannot connect to the daemon's socket {}", path.display())]
    Socket {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the daemon in this app has stopped")]
    InAppStopped,
    #[error("the ssh host `{host}` is empty or starts with `-`")]
    InvalidHost { host: String },
    #[error("cannot start ssh")]
    Spawn(#[source] io::Error),
    /// ssh ended before the daemon answered. `stderr` is the end of what it wrote, such as
    /// "Permission denied (publickey)".
    #[error("ssh failed: {stderr}")]
    Ssh { stderr: String },
    #[error("the daemon refused the connection: {reason}")]
    Refused {
        reason: String,
        /// The release and commit the daemon names, which a daemon of protocol 8 or older does
        /// not.
        daemon_version: Option<ReleaseVersion>,
        daemon_commit: Option<BuildCommit>,
    },
    #[error("the daemon speaks protocol {daemon}, this client {PROTOCOL_VERSION}")]
    VersionMismatch { daemon: u16 },
    #[error("the daemon closed the connection before it answered")]
    Closed,
    #[error("the daemon answered with something other than a welcome")]
    Unexpected,
    #[error("the daemon did not answer in time")]
    Timeout,
    #[error("the connection failed before the daemon answered")]
    Frame(#[from] FrameError),
}

/// How a daemon's refusal starts when the versions differ.
const REFUSED_PROTOCOL_PREFIX: &str = "the daemon speaks protocol ";

impl ConnectError {
    /// The protocol of a daemon that does not speak this client's. A daemon that refuses says so
    /// only in the text of its reason.
    #[must_use]
    pub fn daemon_protocol(&self) -> Option<u16> {
        match self {
            Self::VersionMismatch { daemon } => Some(*daemon),
            Self::Refused { reason, .. } => {
                let rest = reason.strip_prefix(REFUSED_PROTOCOL_PREFIX)?;
                rest.split(|character: char| !character.is_ascii_digit())
                    .next()?
                    .parse()
                    .ok()
            }
            Self::Socket { .. }
            | Self::InAppStopped
            | Self::InvalidHost { .. }
            | Self::Spawn(_)
            | Self::Ssh { .. }
            | Self::Closed
            | Self::Unexpected
            | Self::Timeout
            | Self::Frame(_) => None,
        }
    }
}

/// Why an established connection ended, or lost events.
#[derive(Debug, thiserror::Error)]
pub enum CloseError {
    #[error("the daemon closed the connection")]
    Closed,
    #[error("the connection failed")]
    Frame(#[from] FrameError),
    /// The reason was returned by an earlier call.
    #[error("the connection is closed")]
    AlreadyClosed,
    /// Events came faster than they were read, so the ones waiting were dropped. A `Connection`
    /// stays open after it. Reconnecting without a last `Seq` gives a snapshot.
    #[error("events came faster than they were read, so the waiting ones were dropped")]
    EventsLost,
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// The daemon answered with an error.
    #[error("{}", .0.message)]
    Failed(ErrorBody),
    /// The request was not sent: there is no connection.
    #[error("not connected to the daemon")]
    NotConnected,
    /// The connection closed after the request was sent: the daemon may have carried it out.
    #[error("the connection closed before the daemon answered")]
    Disconnected,
    /// The request was not sent.
    #[error("the request cannot be sent")]
    Encode(#[source] FrameError),
    #[error("the daemon's answer does not decode")]
    InvalidResponse(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(reason: &str) -> ConnectError {
        ConnectError::Refused {
            reason: reason.to_owned(),
            daemon_version: None,
            daemon_commit: None,
        }
    }

    #[test]
    fn a_refusal_or_a_mismatch_names_the_daemons_protocol() {
        assert_eq!(
            refused("the daemon speaks protocol 1, the client 4").daemon_protocol(),
            Some(1)
        );
        assert_eq!(
            refused("the daemon speaks protocol 12").daemon_protocol(),
            Some(12)
        );
        assert_eq!(
            ConnectError::VersionMismatch { daemon: 5 }.daemon_protocol(),
            Some(5)
        );
    }

    #[test]
    fn any_other_error_names_no_protocol() {
        assert_eq!(
            refused("the first message must be a hello").daemon_protocol(),
            None
        );
        assert_eq!(
            refused("the daemon speaks protocol two").daemon_protocol(),
            None
        );
        assert_eq!(ConnectError::Closed.daemon_protocol(), None);
    }
}

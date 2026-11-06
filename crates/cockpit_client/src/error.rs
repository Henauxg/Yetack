//! Why connecting, a connection or a request failed.

use std::io;
use std::path::PathBuf;

use cockpit_protocol::{ErrorBody, FrameError, PROTOCOL_VERSION};

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
    Refused { reason: String },
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

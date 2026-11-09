//! A connection to an agents-cockpit daemon, over its local socket, an SSH bridge, or an
//! in-memory stream to a daemon in the same process.
//!
//! [`Session`] is what an app keeps: it reconnects after a drop and reports every change of
//! connection. [`Connection`] is one connection, for a command that runs once.

mod connection;
mod error;
mod lossy_channel;
mod requester;
mod session;
#[cfg(test)]
#[cfg(unix)]
mod test_daemon;
mod transport;

pub use connection::Connection;
pub use error::{CloseError, ConnectError, RequestError};
pub use requester::Requester;
pub use session::{Backoff, Session, SessionError, SessionUpdate};
pub use transport::{BRIDGE_SUBCOMMAND, InAppLink, Transport, ssh_script_command};

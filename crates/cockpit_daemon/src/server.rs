//! One task per connection, and on Unix, the socket and the checks on each peer.

mod connection;
#[cfg(unix)]
mod listener;
#[cfg(unix)]
mod peer;

pub(crate) use connection::serve as serve_connection;
#[cfg(unix)]
pub(crate) use listener::{accept_loop, bind};
#[cfg(unix)]
pub(crate) use peer::DaemonIdentity;
#[cfg(unix)]
pub use peer::{PeerError, UserId};

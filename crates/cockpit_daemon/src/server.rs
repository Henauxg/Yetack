//! The Unix socket, the checks on each peer, and one task per connection.

mod connection;
mod listener;
mod peer;

pub(crate) use connection::serve as serve_connection;
pub(crate) use listener::{accept_loop, bind};
pub(crate) use peer::DaemonIdentity;
pub use peer::{PeerError, UserId};

//! The agents-cockpit daemon: Claude Code processes, the hub, the store and the socket server.

mod store;
mod transcript;

pub use store::{StoreError, default_state_dir, socket_path};

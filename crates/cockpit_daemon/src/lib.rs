//! The agents-cockpit daemon: Claude Code processes, the hub, the store and the socket server.
//!
//! [`serve`] runs it. The `agents-cockpit` binary calls it for `agents-cockpit serve`.

mod browse;
mod daemon_config;
mod error_chain;
mod hub;
mod mcp_remove;
mod process;
mod read_file;
mod serve;
mod server;
mod sessions;
mod store;
mod transcript;

pub use daemon_config::{CLAUDE_PROGRAM_ENV, DaemonConfig, HOME_ENV};
pub use process::ProcessError;
pub use serve::{ServeError, serve};
pub use server::{PeerError, ProcessId, UserId};
pub use store::{StoreError, default_state_dir, socket_path};

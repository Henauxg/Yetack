//! The agents-cockpit daemon: Claude Code processes, the hub, the store and the socket server.
//!
//! [`serve`] runs it. The `agents-cockpit` binary calls it for `agents-cockpit serve`.

#![cfg_attr(
    windows,
    allow(
        dead_code,
        unused_imports,
        reason = "on Windows, nothing runs the daemon until the desktop app hosts it"
    )
)]

mod browse;
mod daemon_config;
mod error_chain;
mod hub;
mod machine_load;
mod mcp_remove;
mod process;
mod read_file;
#[cfg(unix)]
mod serve;
#[cfg(unix)]
mod server;
mod sessions;
mod store;
#[cfg(test)]
#[cfg(unix)]
mod test_script;
mod transcript;

pub use daemon_config::{CLAUDE_PROGRAM_ENV, DaemonConfig, HOME_ENV};
pub use process::ProcessError;
#[cfg(unix)]
pub use serve::{ServeError, serve};
#[cfg(unix)]
pub use server::{PeerError, UserId};
pub use store::{StoreError, default_state_dir, socket_path};

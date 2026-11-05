//! The daemon's entry point.

use std::io;
use std::path::PathBuf;

use rustix::fs::Mode;
use tokio::sync::mpsc;

use crate::daemon_config::DaemonConfig;
use crate::hub::Hub;
use crate::server::DaemonIdentity;
use crate::store::{StateDir, StoreError};
use crate::{hub, server};

/// New files and folders are for the owner only.
const UMASK: u32 = 0o077;
/// Inputs waiting for the hub. A full queue makes the process and connection tasks wait.
const HUB_INPUTS: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("cannot listen on {}", path.display())]
    Listen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the daemon's {0} task stopped")]
    TaskEnded(&'static str),
}

/// Runs the daemon until `shutdown` completes.
///
/// It takes the state folder's lock first, so a second daemon fails with
/// `StoreError::AlreadyRunning` without touching the first one's socket. On shutdown the agents
/// stay marked as running, so the next daemon resumes them.
pub async fn serve(
    config: DaemonConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    rustix::process::umask(Mode::from_raw_mode(UMASK));
    let state = StateDir::create(&config.state_dir)?;
    let _lock = state.lock()?;
    let restored = hub::restore(&state)?;
    let socket = state.socket();
    let listener = server::bind(&socket).map_err(|source| ServeError::Listen {
        path: socket.clone(),
        source,
    })?;
    let (inputs, received) = mpsc::channel(HUB_INPUTS);
    let hub = Hub::new(state, config, restored, inputs.clone());
    let mut hub_task = tokio::spawn(hub.run(received));
    let mut server_task = tokio::spawn(server::accept_loop(
        listener,
        inputs,
        DaemonIdentity::current(),
    ));
    tracing::info!(socket = %socket.display(), "serving");
    let result = tokio::select! {
        () = shutdown => Ok(()),
        _ = &mut hub_task => Err(ServeError::TaskEnded("hub")),
        _ = &mut server_task => Err(ServeError::TaskEnded("server")),
    };
    server_task.abort();
    hub_task.abort();
    if let Err(error) = std::fs::remove_file(&socket) {
        tracing::warn!(%error, "cannot remove the socket");
    }
    result
}

//! `agents-cockpit serve`: the daemon behind its Unix socket.

use std::io;
use std::path::PathBuf;

use crate::daemon::Daemon;
use crate::daemon_config::DaemonConfig;
use crate::server;
use crate::server::DaemonIdentity;
use crate::store::StoreError;

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
    #[error("cannot start the daemon's runtime")]
    Runtime(#[source] io::Error),
    #[error("the daemon's {0} task stopped")]
    TaskEnded(&'static str),
}

/// Runs the daemon until `shutdown` completes.
///
/// It takes the state folder's lock first, so a second daemon fails with
/// `StoreError::AlreadyRunning` without touching the first one's socket. On shutdown the agents
/// stay marked as running, so the next daemon resumes them. Their processes end with the Tokio
/// runtime that runs this future.
///
/// It leaves the umask alone: `claude` and the commands the agents run inherit the user's. Each
/// file and folder of the state folder is private by the mode it is created with.
pub async fn serve(
    config: DaemonConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let prepared = Daemon::prepare(config)?;
    let socket = prepared.socket();
    let listener = server::bind(&socket).map_err(|source| ServeError::Listen {
        path: socket.clone(),
        source,
    })?;
    let mut daemon = prepared.start();
    let mut server_task = tokio::spawn(server::accept_loop(
        listener,
        daemon.inputs(),
        DaemonIdentity::current(),
    ));
    tracing::info!(socket = %socket.display(), "serving");
    let result = tokio::select! {
        () = shutdown => Ok(()),
        () = daemon.hub_ended() => Err(ServeError::TaskEnded("hub")),
        _ = &mut server_task => Err(ServeError::TaskEnded("server")),
    };
    server_task.abort();
    daemon.stop_hub();
    if let Err(error) = std::fs::remove_file(&socket) {
        tracing::warn!(%error, "cannot remove the socket");
    }
    result
}

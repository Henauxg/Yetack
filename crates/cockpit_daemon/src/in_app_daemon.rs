//! The daemon inside the desktop app's process: no socket, and each client an in-memory stream.

use std::time::Duration;

use tokio::io::DuplexStream;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::daemon::Daemon;
use crate::daemon_config::DaemonConfig;
use crate::hub::HubInput;
use crate::serve::ServeError;
use crate::server;

const THREAD_NAME: &str = "agents-cockpit-daemon";

/// The daemon, run by another program on a Tokio runtime of its own.
///
/// It uses the state folder and the lock a background service uses, so the two never serve the
/// same folder at once, and each resumes the agents the other left running.
///
/// No socket listens, so no peer check applies: only the holder of the channel's sending end
/// reaches it, and an agent's command has no endpoint to connect to.
pub struct InAppDaemon {
    /// Owns every task of the daemon, the agents' processes included.
    runtime: Option<Runtime>,
    /// Holds the lock, released once the runtime is gone.
    daemon: Option<Daemon>,
}

impl InAppDaemon {
    /// Starts the daemon, which resumes the agents that were running, and serves each stream
    /// `connections` brings as one client: the protocol's hello comes first, as on the socket.
    ///
    /// Fails with `StoreError::AlreadyRunning` when another daemon serves the state folder.
    pub async fn start(
        config: DaemonConfig,
        connections: mpsc::UnboundedReceiver<DuplexStream>,
    ) -> Result<Self, ServeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name(THREAD_NAME)
            .build()
            .map_err(ServeError::Runtime)?;
        let started = runtime.spawn(async move {
            let daemon = Daemon::prepare(config)?.start();
            tokio::spawn(accept(connections, daemon.inputs()));
            Ok::<Daemon, ServeError>(daemon)
        });
        let started = started.await.unwrap_or(Err(ServeError::TaskEnded("start")));
        match started {
            Ok(daemon) => Ok(Self {
                runtime: Some(runtime),
                daemon: Some(daemon),
            }),
            Err(error) => {
                // Dropping a runtime waits for its threads, which an async caller must not do.
                runtime.shutdown_background();
                Err(error)
            }
        }
    }

    /// Stops the daemon as a background service stops: the agents stay marked as running, and
    /// their processes are killed, so the next daemon resumes their cut-off turns. Waits at most
    /// `wait` for the daemon's tasks, then releases the lock.
    ///
    /// It blocks: call it from a thread outside any Tokio runtime.
    pub fn shutdown(mut self, wait: Duration) {
        if let Some(daemon) = &self.daemon {
            daemon.stop_hub();
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(wait);
        }
        self.daemon = None;
    }
}

impl Drop for InAppDaemon {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

async fn accept(
    mut connections: mpsc::UnboundedReceiver<DuplexStream>,
    hub: mpsc::Sender<HubInput>,
) {
    while let Some(stream) = connections.recv().await {
        let (read, write) = tokio::io::split(stream);
        tokio::spawn(server::serve_connection(read, write, hub.clone()));
    }
}

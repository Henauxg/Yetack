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
        // Its drop stops the runtime without waiting, should this future be dropped or fail.
        let mut this = Self {
            runtime: Some(runtime),
            daemon: None,
        };
        let daemon = started
            .await
            .unwrap_or(Err(ServeError::TaskEnded("start")))?;
        this.daemon = Some(daemon);
        Ok(this)
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
        // Dropping a runtime waits for its threads, which an async caller must not do.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Serves each stream as a client until the hub ends. Then it drops `connections`, so that the
/// clients learn the daemon stopped.
async fn accept(
    mut connections: mpsc::UnboundedReceiver<DuplexStream>,
    hub: mpsc::Sender<HubInput>,
) {
    loop {
        let stream = tokio::select! {
            stream = connections.recv() => stream,
            () = hub.closed() => {
                tracing::info!("the daemon's hub stopped: it takes no more clients");
                return;
            }
        };
        let Some(stream) = stream else {
            return;
        };
        let (read, write) = tokio::io::split(stream);
        tokio::spawn(server::serve_connection(read, write, hub.clone()));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn config(temp: &Path) -> DaemonConfig {
        DaemonConfig {
            state_dir: temp.join("state"),
            claude_program: temp.join("no-claude"),
            claude_config_dir: temp.join("claude"),
        }
    }

    /// What the app does on every platform, with no agent: it connects, finds the folder taken
    /// by its own daemon, stops it, and runs it again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_daemon_refuses_clients_and_frees_its_folder() {
        use cockpit_client::{ConnectError, Connection, InAppLink, Transport};
        use cockpit_protocol::Catchup;

        use crate::store::StoreError;

        let wait = Duration::from_secs(10);
        let shut_down = |daemon: InAppDaemon| {
            std::thread::spawn(move || daemon.shutdown(wait))
                .join()
                .unwrap();
        };
        let temp = tempfile::tempdir().unwrap();
        let (link, connections) = InAppLink::new();
        let daemon = InAppDaemon::start(config(temp.path()), connections)
            .await
            .unwrap();
        let transport = Transport::InApp(link);
        let (mut connection, catchup) = Connection::connect(&transport, None).await.unwrap();
        assert!(matches!(catchup, Catchup::Snapshot(_)));

        let (_other, connections) = InAppLink::new();
        let refused = InAppDaemon::start(config(temp.path()), connections)
            .await
            .err();
        assert!(
            matches!(
                refused,
                Some(ServeError::Store(StoreError::AlreadyRunning { .. }))
            ),
            "{refused:?}"
        );

        shut_down(daemon);
        tokio::time::timeout(wait, async {
            while connection.next_event().await.is_ok() {}
        })
        .await
        .expect("the connection ends");
        assert!(matches!(
            Connection::connect(&transport, None).await,
            Err(ConnectError::InAppStopped)
        ));

        let (_link, connections) = InAppLink::new();
        let again = InAppDaemon::start(config(temp.path()), connections)
            .await
            .unwrap();
        shut_down(again);
    }

    /// A client of a daemon whose hub failed learns that the daemon stopped, as after a shutdown,
    /// instead of reconnecting to a hub that never answers.
    #[tokio::test]
    async fn a_hub_that_ended_takes_no_more_clients() {
        let (streams, connections) = mpsc::unbounded_channel();
        let (hub, inputs) = mpsc::channel(1);
        drop(inputs);

        tokio::time::timeout(Duration::from_secs(2), accept(connections, hub))
            .await
            .expect("accept returns");
        assert!(streams.is_closed());
    }

    /// The app drops a start it no longer waits for, as when it closes meanwhile.
    #[tokio::test]
    async fn a_start_dropped_before_it_ends_releases_the_folder() {
        use std::task::{Context, Poll, Waker};
        use std::time::Instant;

        let temp = tempfile::tempdir().unwrap();
        let (_streams, connections) = mpsc::unbounded_channel();
        let mut start = Box::pin(InAppDaemon::start(config(temp.path()), connections));
        let polled = start.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        assert!(matches!(polled, Poll::Pending));
        drop(start);

        let deadline = Instant::now() + Duration::from_secs(10);
        let daemon = loop {
            let (_streams, connections) = mpsc::unbounded_channel();
            match InAppDaemon::start(config(temp.path()), connections).await {
                Ok(daemon) => break daemon,
                Err(error) => assert!(Instant::now() < deadline, "{error:?}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        drop(daemon);
    }
}

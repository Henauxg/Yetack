//! The daemon this app runs in its own process, once it runs one.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cockpit_client::InAppLink;
use cockpit_daemon::{DaemonConfig, InAppDaemon, ServeError};

/// How long the app's close waits for the daemon's tasks, after it killed the agents' processes.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);

/// Holds the daemon the app runs, shared between the connection task, which starts it, and the
/// window's close, which stops it.
#[derive(Clone, Default)]
pub struct InAppHost {
    running: Arc<Mutex<Option<Running>>>,
}

struct Running {
    daemon: InAppDaemon,
    link: InAppLink,
}

impl InAppHost {
    /// The link to the app's daemon, which starts first unless it runs already.
    pub async fn start(&self, config: DaemonConfig) -> Result<InAppLink, ServeError> {
        if let Some(link) = self.link() {
            return Ok(link);
        }
        let (link, connections) = InAppLink::new();
        let daemon = InAppDaemon::start(config, connections).await?;
        let running = Running {
            daemon,
            link: link.clone(),
        };
        match self.running.lock() {
            Ok(mut slot) => *slot = Some(running),
            Err(_) => tracing::error!("the in-app daemon's slot is poisoned"),
        }
        Ok(link)
    }

    fn link(&self) -> Option<InAppLink> {
        let slot = self.running.lock().ok()?;
        slot.as_ref().map(|running| running.link.clone())
    }

    /// Stops the daemon as a background service stops, so its agents resume the next time the
    /// app runs them. Blocks until it stopped, at most `SHUTDOWN_WAIT` after the agents.
    pub fn shutdown(&self) {
        let running = self.running.lock().ok().and_then(|mut slot| slot.take());
        let Some(Running { daemon, .. }) = running else {
            return;
        };
        tracing::info!("stopping the daemon in this app");
        // The caller may be inside a Tokio runtime, where waiting for another one is refused.
        let stopping = std::thread::spawn(move || daemon.shutdown(SHUTDOWN_WAIT));
        match stopping.join() {
            Ok(()) => tracing::info!("stopped the daemon in this app"),
            Err(_) => tracing::error!("the in-app daemon's shutdown failed"),
        }
    }
}

/// Two hosts are equal when they hold the same daemon.
impl PartialEq for InAppHost {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.running, &other.running)
    }
}

impl fmt::Debug for InAppHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let running = self.link().is_some();
        formatter
            .debug_struct("InAppHost")
            .field("running", &running)
            .finish()
    }
}

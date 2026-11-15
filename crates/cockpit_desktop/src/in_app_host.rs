//! The daemon this app runs in its own process, once it runs one.

use std::panic::PanicHookInfo;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};
use std::{fmt, io};

use cockpit_client::InAppLink;
use cockpit_daemon::{DaemonConfig, InAppDaemon, START_TASK, ServeError};
use tokio::sync::{Notify, oneshot};

/// How long the app's close waits for the daemon's tasks, after it killed the agents' processes.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(5);
/// How long the app's close waits for a daemon that is still reading its state folder back.
const START_WAIT: Duration = Duration::from_secs(5);
/// How long a stop after a panic waits for the host's lock, which the panicking thread may hold.
const PANIC_LOCK_WAIT: Duration = Duration::from_secs(1);
/// How often that stop tries the lock again.
const PANIC_LOCK_RETRY: Duration = Duration::from_millis(10);
const START_THREAD: &str = "yetack-start";
const SIGNAL_THREAD: &str = "yetack-signals";
/// How long the signal thread waits for the window's thread to end the app before it does.
#[cfg(target_os = "linux")]
const WINDOW_EXIT_WAIT: Duration = Duration::from_secs(5);
const PANIC_THREAD: &str = "yetack-panic";
/// The thread `main` runs on, which runs the window.
const MAIN_THREAD: &str = "main";

/// The app's exit status after a panic, as Rust's own for a panic that ends `main`.
const EXIT_PANICKED: i32 = 101;

/// The app's exit status after a signal, as a shell reports a process the signal ended.
#[cfg(unix)]
const EXIT_HUNG_UP: i32 = 129;
const EXIT_INTERRUPTED: i32 = 130;
#[cfg(unix)]
const EXIT_TERMINATED: i32 = 143;

/// Holds the daemon the app runs, shared between the connection task, which starts it, and the
/// window's close, which stops it.
#[derive(Clone, Default)]
pub struct InAppHost {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    hosted: Mutex<Hosted>,
    /// Wakes the window's close when a start ends.
    start_ended: Condvar,
    /// Wakes a connection task that waits for another start.
    start_ended_for_tasks: Notify,
}

#[derive(Default)]
enum Hosted {
    #[default]
    Stopped,
    /// A thread of its own starts it, so the connection task can go away meanwhile. `stop`: the
    /// window closed first, and the thread stops the daemon once it started.
    Starting {
        stop: bool,
    },
    Running(Running),
}

struct Running {
    daemon: InAppDaemon,
    link: InAppLink,
}

impl InAppHost {
    /// The link to the app's daemon, which starts first unless it runs already.
    pub async fn start(&self, config: DaemonConfig) -> Result<InAppLink, ServeError> {
        loop {
            let mut start_ended = pin!(self.shared.start_ended_for_tasks.notified());
            start_ended.as_mut().enable();
            {
                let mut hosted = self.shared.lock();
                match &*hosted {
                    Hosted::Running(running) => return Ok(running.link.clone()),
                    Hosted::Starting { .. } => {}
                    Hosted::Stopped => {
                        *hosted = Hosted::Starting { stop: false };
                        break;
                    }
                }
            }
            start_ended.await;
        }
        let (sender, started) = oneshot::channel();
        let shared = Arc::clone(&self.shared);
        let thread = std::thread::Builder::new()
            .name(START_THREAD.to_owned())
            .spawn(move || {
                let link = shared.finish_start(start_daemon(config));
                // The connection task may be gone, when the window closed meanwhile.
                let _ = sender.send(link);
            });
        if let Err(source) = thread {
            return self.shared.finish_start(Err(ServeError::Runtime(source)));
        }
        started
            .await
            .unwrap_or(Err(ServeError::TaskEnded(START_TASK)))
    }

    /// Stops the daemon as a background service stops, so its agents resume the next time the
    /// app runs them. Blocks until it stopped, at most `START_WAIT` for a start under way, then
    /// `SHUTDOWN_WAIT` after the agents.
    pub fn shutdown(&self) {
        self.stop(self.shared.lock());
    }

    /// As `shutdown`, from a panic: the panicking thread may hold the lock, so this waits for it
    /// at most `PANIC_LOCK_WAIT`, and leaves the daemon running when it stays held.
    pub fn shutdown_after_panic(&self) {
        let deadline = Instant::now() + PANIC_LOCK_WAIT;
        loop {
            match self.shared.hosted.try_lock() {
                Ok(hosted) => return self.stop(hosted),
                // A stop must run: any value of `Hosted` is valid after a panicking holder.
                Err(TryLockError::Poisoned(poisoned)) => return self.stop(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(PANIC_LOCK_RETRY);
                }
                Err(TryLockError::WouldBlock) => {
                    tracing::error!("cannot stop the daemon in this app: its lock stays held");
                    return;
                }
            }
        }
    }

    /// Whether the app may run a daemon: one runs or starts, or the lock is held, so that a stop
    /// after a panic cannot tell.
    fn may_host(&self) -> bool {
        match self.shared.hosted.try_lock() {
            Ok(hosted) => !matches!(*hosted, Hosted::Stopped),
            // Any value of `Hosted` is valid after a panicking holder.
            Err(TryLockError::Poisoned(poisoned)) => {
                !matches!(*poisoned.into_inner(), Hosted::Stopped)
            }
            Err(TryLockError::WouldBlock) => true,
        }
    }

    fn stop(&self, hosted: MutexGuard<'_, Hosted>) {
        let running = {
            let (mut hosted, _) = self
                .shared
                .start_ended
                .wait_timeout_while(hosted, START_WAIT, |hosted| {
                    matches!(hosted, Hosted::Starting { .. })
                })
                // A stop must run: any value of `Hosted` is valid after a panicking holder.
                .unwrap_or_else(PoisonError::into_inner);
            match std::mem::take(&mut *hosted) {
                Hosted::Running(running) => running,
                Hosted::Starting { .. } => {
                    tracing::warn!(
                        "the daemon in this app is still starting: it stops once started"
                    );
                    *hosted = Hosted::Starting { stop: true };
                    return;
                }
                Hosted::Stopped => return,
            }
        };
        tracing::info!("stopping the daemon in this app");
        // The caller may be inside a Tokio runtime, where waiting for another one is refused.
        let stopping = std::thread::Builder::new()
            .spawn(move || running.daemon.shutdown(SHUTDOWN_WAIT))
            .map(std::thread::JoinHandle::join);
        match stopping {
            Ok(Ok(())) => tracing::info!("stopped the daemon in this app"),
            Ok(Err(_)) => tracing::error!("the in-app daemon's shutdown failed"),
            Err(error) => tracing::error!("cannot stop the daemon in this app: {error}"),
        }
    }

    fn link(&self) -> Option<InAppLink> {
        match &*self.shared.lock() {
            Hosted::Running(running) => Some(running.link.clone()),
            Hosted::Stopped | Hosted::Starting { .. } => None,
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Hosted> {
        // Starts and stops must run: any value of `Hosted` is valid after a panicking holder.
        self.hosted.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the daemon a start gave, or stops it when the window closed meanwhile.
    fn finish_start(
        &self,
        started: Result<(InAppDaemon, InAppLink), ServeError>,
    ) -> Result<InAppLink, ServeError> {
        let mut hosted = self.lock();
        let stop = matches!(*hosted, Hosted::Starting { stop: true });
        let result = match started {
            Ok((daemon, link)) if stop => {
                drop(hosted);
                daemon.shutdown(SHUTDOWN_WAIT);
                hosted = self.lock();
                *hosted = Hosted::Stopped;
                Ok(link)
            }
            Ok((daemon, link)) => {
                *hosted = Hosted::Running(Running {
                    daemon,
                    link: link.clone(),
                });
                Ok(link)
            }
            Err(error) => {
                *hosted = Hosted::Stopped;
                Err(error)
            }
        };
        drop(hosted);
        self.start_ended.notify_all();
        self.start_ended_for_tasks.notify_waiters();
        result
    }
}

/// Runs on a thread of its own, so that the start goes on when the window's close drops the
/// connection task, and `shutdown` can stop the daemon it starts.
fn start_daemon(config: DaemonConfig) -> Result<(InAppDaemon, InAppLink), ServeError> {
    let (link, connections) = InAppLink::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(ServeError::Runtime)?;
    let daemon = runtime.block_on(InAppDaemon::start(config, connections))?;
    Ok((daemon, link))
}

/// Stops the daemon in the app, as the window's close does, when the terminal or the system stops
/// the app: the agents' processes lead process groups of their own, which those signals miss.
pub fn stop_on_signals(host: InAppHost) {
    let watcher = std::thread::Builder::new()
        .name(SIGNAL_THREAD.to_owned())
        .spawn(move || {
            let received = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .and_then(|runtime| runtime.block_on(stop_signal()));
            match received {
                Ok(status) => {
                    host.shutdown();
                    exit_on_window_thread(status);
                }
                Err(error) => {
                    tracing::warn!("cannot watch for the signals that stop the app: {error}")
                }
            }
        });
    if let Err(error) = watcher {
        tracing::warn!("cannot watch for the signals that stop the app: {error}");
    }
}

/// Ends the process on the window's thread. `exit` on another thread unloads the graphics libraries
/// while the window's thread may be drawing with them: WebKitGTK then crashes as it draws, and the
/// window stays frozen, black, while the core dump is written.
#[cfg(target_os = "linux")]
fn exit_on_window_thread(status: i32) -> ! {
    // An idle callback runs on the thread that runs the default main context: the window's,
    // between two frames.
    webkit2gtk::glib::idle_add_once(move || std::process::exit(status));
    // A window's loop that never gets to it, such as one not started yet, must not keep the app.
    std::thread::sleep(WINDOW_EXIT_WAIT);
    std::process::exit(status)
}

#[cfg(not(target_os = "linux"))]
fn exit_on_window_thread(status: i32) -> ! {
    std::process::exit(status)
}

/// What a panic does once the default hook has reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnPanic {
    /// No daemon runs in the app, or an earlier panic stops it already.
    Nothing,
    /// Stops the daemon in the app, then ends the app.
    StopAndExit,
}

/// The first panic while the app may run a daemon stops it. `stopping` keeps a later one, such as
/// a panic in the stop itself, from starting another stop.
fn on_panic(host: &InAppHost, stopping: &AtomicBool) -> OnPanic {
    if !host.may_host() || stopping.swap(true, Ordering::SeqCst) {
        return OnPanic::Nothing;
    }
    OnPanic::StopAndExit
}

/// Stops the daemon in the app and ends the app on a panic in any thread: on Unix the agents'
/// processes lead process groups of their own, which outlive the app, and the next start would
/// resume their sessions a second time. The default hook still reports the panic, and the log
/// gets it too, since a GUI program on Windows has no stderr.
pub fn stop_on_panic(host: InAppHost) {
    let report = std::panic::take_hook();
    let stopping = AtomicBool::new(false);
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        report(info);
        tracing::error!("{info}");
        let current = std::thread::current();
        if current.name() == Some(PANIC_THREAD) {
            // The stop itself failed.
            std::process::exit(EXIT_PANICKED);
        }
        if on_panic(&host, &stopping) == OnPanic::Nothing {
            return;
        }
        let stop_host = host.clone();
        let stop = move || {
            stop_host.shutdown_after_panic();
            std::process::exit(EXIT_PANICKED);
        };
        let stopper = std::thread::Builder::new()
            .name(PANIC_THREAD.to_owned())
            .spawn(stop);
        match stopper {
            // The window's thread would end the process as it unwinds. Another thread unwinds
            // at once, so that the drop of its task kills the processes the task holds.
            Ok(stopper) if current.name() == Some(MAIN_THREAD) => {
                let _ = stopper.join();
                std::process::exit(EXIT_PANICKED);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!("cannot start the stop after a panic: {error}");
                host.shutdown_after_panic();
                std::process::exit(EXIT_PANICKED);
            }
        }
    }));
}

/// Waits for Ctrl+C, a hang-up or a request to terminate, and returns the exit status it calls for.
#[cfg(unix)]
async fn stop_signal() -> io::Result<i32> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hang_up = signal(SignalKind::hangup())?;
    let status = tokio::select! {
        _ = interrupt.recv() => EXIT_INTERRUPTED,
        _ = terminate.recv() => EXIT_TERMINATED,
        _ = hang_up.recv() => EXIT_HUNG_UP,
    };
    Ok(status)
}

#[cfg(windows)]
async fn stop_signal() -> io::Result<i32> {
    tokio::signal::ctrl_c().await?;
    Ok(EXIT_INTERRUPTED)
}

/// Two hosts are equal when they hold the same daemon.
impl PartialEq for InAppHost {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cockpit_client::Transport;

    use super::*;

    fn config(state: &Path) -> DaemonConfig {
        DaemonConfig {
            state_dir: state.join("state"),
            claude_program: state.join("no-claude"),
            claude_config_dir: state.join("claude"),
        }
    }

    /// The window drops the connection task, which may wait for the daemon to start, inside the
    /// app's Tokio runtime, then stops the daemon from its last event.
    #[test]
    fn closing_the_window_while_the_daemon_starts_stops_it() {
        let temp = tempfile::tempdir().unwrap();
        let host = InAppHost::default();
        let app = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        app.block_on(async {
            let mut starting = Box::pin(host.start(config(temp.path())));
            let first = futures_util::poll!(&mut starting);
            assert!(
                first.is_pending(),
                "the daemon starts on a thread of its own"
            );
            drop(starting);
        });
        host.shutdown();

        let (_link, connections) = InAppLink::new();
        let again = app.block_on(InAppDaemon::start(config(temp.path()), connections));
        assert!(again.is_ok(), "the lock is free again: {:?}", again.err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_starts_at_once_share_one_daemon() {
        let temp = tempfile::tempdir().unwrap();
        let host = InAppHost::default();

        let (first, second) = tokio::join!(
            host.start(config(temp.path())),
            host.start(config(temp.path()))
        );

        assert_eq!(
            Transport::InApp(first.unwrap()),
            Transport::InApp(second.unwrap())
        );
        host.shutdown();
    }

    #[test]
    fn only_the_first_panic_while_a_daemon_may_run_stops_it() {
        let host = InAppHost::default();
        let stopping = AtomicBool::new(false);

        assert_eq!(on_panic(&host, &stopping), OnPanic::Nothing);

        *host.shared.lock() = Hosted::Starting { stop: false };
        assert_eq!(on_panic(&host, &stopping), OnPanic::StopAndExit);
        assert_eq!(on_panic(&host, &stopping), OnPanic::Nothing);
    }

    #[test]
    fn a_stop_after_a_panic_gives_up_on_a_lock_its_own_thread_holds() {
        let host = InAppHost::default();
        let held = host.shared.lock();
        let started = Instant::now();

        host.shutdown_after_panic();

        assert!(started.elapsed() < PANIC_LOCK_WAIT * 3);
        drop(held);
    }

    #[test]
    fn a_stop_after_a_panic_stops_the_daemon_through_a_poisoned_lock() {
        let temp = tempfile::tempdir().unwrap();
        let host = InAppHost::default();
        let app = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        app.block_on(host.start(config(temp.path()))).unwrap();
        let poisoner = host.clone();
        let panicked = std::thread::spawn(move || {
            let _held = poisoner.shared.lock();
            panic!("a panic while the lock is held");
        })
        .join();
        assert!(panicked.is_err());
        assert!(host.shared.hosted.is_poisoned());

        host.shutdown_after_panic();

        assert!(!host.may_host());
        let (_link, connections) = InAppLink::new();
        let again = app.block_on(InAppDaemon::start(config(temp.path()), connections));
        assert!(again.is_ok(), "the lock is free again: {:?}", again.err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hang_up_stops_the_app() {
        use std::process::Command;

        let mut waiting = Box::pin(stop_signal());
        assert!(futures_util::poll!(&mut waiting).is_pending());

        let sent = Command::new("kill")
            .args(["-HUP", &std::process::id().to_string()])
            .status()
            .unwrap();

        assert!(sent.success());
        let status = tokio::time::timeout(Duration::from_secs(10), waiting).await;
        assert_eq!(status.unwrap().unwrap(), EXIT_HUNG_UP);
    }
}

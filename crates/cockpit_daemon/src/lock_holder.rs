//! The daemon that holds a state folder's `serve.lock`. Once it holds the lock, a daemon writes its
//! process into the file, and it empties the file as it lets the lock go. Another program can then
//! tell which process serves the folder, and stop it: the app restarts this computer's background
//! service this way, to run it with its own program.
//!
//! The file holds one line of JSON. The start time and the boot tell the process from a later one
//! that gets its pid, as for an agent's `claude` in `agents.json`. A system that cannot tell a
//! start time records the pid alone, and such a daemon cannot be stopped this way.
//!
//! Only Unix reads the file: on Windows, the lock keeps every other program from reading it.

#[cfg(unix)]
use std::fs::{File, TryLockError};
#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant, SystemTime};

use cockpit_protocol::ProcessId;
use serde::{Deserialize, Serialize};

use crate::machine_load;
#[cfg(unix)]
use crate::store;
use crate::store::{BootId, RecordedProcess};

/// How long a stop waits for the daemon to let the lock go.
#[cfg(unix)]
const STOP_WAIT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const STOP_POLL: Duration = Duration::from_millis(100);

/// A daemon's process, as it records itself in `serve.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockHolder {
    pid: ProcessId,
    /// In the system's own unit, as in `agents.json`. `None` where the system cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boot: Option<BootId>,
}

impl LockHolder {
    pub(crate) fn this_process() -> Self {
        let pid = ProcessId::new(std::process::id());
        match machine_load::record_process(pid) {
            Some(RecordedProcess { pid, started, boot }) => Self {
                pid,
                started: Some(started),
                boot,
            },
            None => Self {
                pid,
                started: None,
                boot: None,
            },
        }
    }

    /// The line the file holds.
    pub(crate) fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        Ok(line)
    }

    /// `None` for a file that names no process: empty, as a daemon older than the record leaves
    /// it, or cut short.
    #[cfg(any(unix, test))]
    fn decode(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

#[cfg(unix)]
impl LockHolder {
    #[must_use]
    pub fn pid(&self) -> ProcessId {
        self.pid
    }

    /// Whether the recorded process still runs: one of this boot with its pid and start time.
    /// `false` for a record without a start time, which cannot tell it from a later process.
    #[must_use]
    pub fn runs(&self) -> bool {
        self.started.is_some_and(|started| {
            machine_load::still_runs(&RecordedProcess {
                pid: self.pid,
                started,
                boot: self.boot.clone(),
            })
        })
    }
}

/// Why the holder of `serve.lock` cannot be read.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
pub enum LockHolderError {
    /// A daemon holds the lock, and the file does not name it: a daemon older than the record,
    /// or one that has just taken the lock.
    #[error("{} names no process", path.display())]
    Unnamed { path: PathBuf },
    #[error("cannot read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Why the daemon that holds `serve.lock` was not stopped.
#[cfg(unix)]
#[derive(Debug, thiserror::Error)]
pub enum StopLockHolderError {
    #[error("no daemon holds the state folder")]
    NoDaemon,
    #[error(transparent)]
    Holder(#[from] LockHolderError),
    /// The file names the process that asks for the stop: the daemon runs in the app.
    #[error("the daemon runs in this process")]
    ThisProcess,
    /// The process the file names has ended, or may be a later one that got its pid.
    #[error("cannot tell that process {} is the daemon", .pid.get())]
    Unverified { pid: ProcessId },
    #[error("cannot signal the daemon, process {}", .pid.get())]
    Signal {
        pid: ProcessId,
        #[source]
        source: io::Error,
    },
    #[error(
        "the daemon, process {}, did not stop within {} seconds",
        .pid.get(),
        .waited.as_secs()
    )]
    StillRunning { pid: ProcessId, waited: Duration },
}

/// The daemon that holds the lock of the state folder `state_dir`, or `None` when no daemon does.
///
/// It takes a free lock for a moment to tell, so a daemon that starts at that moment fails with
/// `StoreError::AlreadyRunning`.
#[cfg(unix)]
pub fn lock_holder(state_dir: &Path) -> Result<Option<LockHolder>, LockHolderError> {
    use std::io::Read;

    let path = store::lock_path(state_dir);
    let read_error = |source| LockHolderError::Read {
        path: path.clone(),
        source,
    };
    let Some(mut file) = held_lock(&path).map_err(read_error)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(read_error)?;
    match LockHolder::decode(&bytes) {
        Some(holder) => Ok(Some(holder)),
        None => Err(LockHolderError::Unnamed { path }),
    }
}

/// A moment no later than the start of the daemon that holds the lock of `state_dir`, or `None`
/// when no daemon holds it. It is the last write to the lock's file: a daemon writes its record
/// there as it takes the lock, before it starts any agent. A daemon older than the record writes
/// nothing, so the moment is earlier then.
///
/// It takes a free lock for a moment, as [`lock_holder`] does.
#[cfg(unix)]
pub fn lock_held_since(state_dir: &Path) -> Result<Option<SystemTime>, LockHolderError> {
    let path = store::lock_path(state_dir);
    let read_error = |source| LockHolderError::Read {
        path: path.clone(),
        source,
    };
    let Some(file) = held_lock(&path).map_err(read_error)? else {
        return Ok(None);
    };
    let written = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map_err(read_error)?;
    Ok(Some(written))
}

/// Stops the daemon that holds the lock of `state_dir` as systemd stops the background service,
/// with SIGTERM, then waits up to 10 seconds for the lock to go free. The daemon leaves its agents
/// marked as running, so the next daemon resumes them.
///
/// The signal goes to the process the file names, once its start time shows it is still that
/// process. It blocks.
#[cfg(unix)]
pub fn stop_lock_holder(state_dir: &Path) -> Result<(), StopLockHolderError> {
    stop_within(state_dir, STOP_WAIT)
}

#[cfg(unix)]
fn stop_within(state_dir: &Path, wait: Duration) -> Result<(), StopLockHolderError> {
    use rustix::io::Errno;
    use rustix::process::{Pid, PidfdFlags, Signal};

    let holder = lock_holder(state_dir)?.ok_or(StopLockHolderError::NoDaemon)?;
    let pid = holder.pid;
    if pid.get() == std::process::id() {
        return Err(StopLockHolderError::ThisProcess);
    }
    let unverified = || StopLockHolderError::Unverified { pid };
    let signal_failed = |errno: Errno| StopLockHolderError::Signal {
        pid,
        source: errno.into(),
    };
    let raw = i32::try_from(pid.get())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(unverified)?;
    // Opened before the check, the pidfd stays on that process: one that gets its pid after the
    // check gets no signal.
    let pidfd = match rustix::process::pidfd_open(raw, PidfdFlags::empty()) {
        Ok(pidfd) => pidfd,
        Err(errno) if errno == Errno::SRCH => return Err(unverified()),
        Err(errno) => return Err(signal_failed(errno)),
    };
    if !holder.runs() {
        return Err(unverified());
    }
    match rustix::process::pidfd_send_signal(&pidfd, Signal::TERM) {
        Ok(()) => {}
        // It ended since the check, and let the lock go.
        Err(errno) if errno == Errno::SRCH => {}
        Err(errno) => return Err(signal_failed(errno)),
    }
    tracing::info!(
        pid = pid.get(),
        "stopping the daemon that holds the state folder"
    );
    let path = store::lock_path(state_dir);
    let deadline = Instant::now() + wait;
    loop {
        let held = held_lock(&path).map_err(|source| LockHolderError::Read {
            path: path.clone(),
            source,
        })?;
        if held.is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(StopLockHolderError::StillRunning { pid, waited: wait });
        }
        std::thread::sleep(STOP_POLL);
    }
}

/// The lock's file, open, while a daemon holds the lock. `None` when the lock is free: it was
/// taken and let go at once.
#[cfg(unix)]
fn held_lock(path: &Path) -> io::Result<Option<File>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => file.unlock().map(|()| None),
        Err(TryLockError::WouldBlock) => Ok(Some(file)),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What another program reads.
    const RECORD: &str =
        "{\"pid\":4242,\"started\":987654,\"boot\":\"6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b\"}\n";

    #[test]
    fn the_record_is_one_line_of_json() {
        let holder = LockHolder {
            pid: ProcessId::new(4242),
            started: Some(987_654),
            boot: Some(BootId::new(
                "6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b".to_owned(),
            )),
        };

        assert_eq!(String::from_utf8(holder.encode().unwrap()).unwrap(), RECORD);
        assert_eq!(LockHolder::decode(RECORD.as_bytes()), Some(holder));
    }

    #[test]
    fn a_record_without_a_start_time_holds_the_pid_alone() {
        let holder = LockHolder {
            pid: ProcessId::new(4242),
            started: None,
            boot: None,
        };

        assert_eq!(holder.encode().unwrap(), b"{\"pid\":4242}\n");
        assert_eq!(LockHolder::decode(b"{\"pid\":4242}\n"), Some(holder));
    }

    #[test]
    fn an_empty_or_cut_file_names_no_process() {
        assert_eq!(LockHolder::decode(b""), None);
        assert_eq!(LockHolder::decode(b"{\"pid\":42"), None);
    }

    #[test]
    fn this_process_records_its_pid() {
        let holder = LockHolder::this_process();

        assert_eq!(holder.pid, ProcessId::new(std::process::id()));
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Child, Command, Stdio};

        use rustix::process::Signal;

        use super::*;
        use crate::store::{StateDir, StoreError};

        fn sleeper() -> Child {
            Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .spawn()
                .unwrap()
        }

        /// The record of `child`, as a daemon in that process would write it.
        fn record_of(child: &Child) -> LockHolder {
            let RecordedProcess { pid, started, boot } =
                machine_load::record_process(ProcessId::new(child.id())).unwrap();
            LockHolder {
                pid,
                started: Some(started),
                boot,
            }
        }

        /// Replaces the record of the lock the test holds, as another daemon would have written it.
        fn write_record(state_dir: &Path, holder: &LockHolder) {
            std::fs::write(store::lock_path(state_dir), holder.encode().unwrap()).unwrap();
        }

        #[test]
        fn a_folder_without_a_daemon_has_no_holder() {
            let temp = tempfile::tempdir().unwrap();
            let missing = lock_holder(temp.path());
            write_record(temp.path(), &LockHolder::this_process());
            let unlocked = lock_holder(temp.path());

            assert_eq!(missing.unwrap(), None);
            assert_eq!(unlocked.unwrap(), None, "a record without its lock");
        }

        #[test]
        fn a_held_lock_names_its_holder() {
            let temp = tempfile::tempdir().unwrap();
            let state = StateDir::create(temp.path()).unwrap();
            let _lock = state.lock().unwrap();

            let holder = lock_holder(temp.path()).unwrap().unwrap();

            assert_eq!(holder, LockHolder::this_process());
            assert_eq!(holder.pid(), ProcessId::new(std::process::id()));
            assert!(holder.runs());
            let again = state.lock();
            assert!(
                matches!(again, Err(StoreError::AlreadyRunning { .. })),
                "the read leaves the lock to its holder: {again:?}"
            );
        }

        #[test]
        fn a_held_lock_without_a_record_names_no_process() {
            let temp = tempfile::tempdir().unwrap();
            let state = StateDir::create(temp.path()).unwrap();
            let _lock = state.lock().unwrap();
            std::fs::write(store::lock_path(temp.path()), b"").unwrap();

            let read = lock_holder(temp.path());

            assert!(
                matches!(read, Err(LockHolderError::Unnamed { .. })),
                "{read:?}"
            );
        }

        #[test]
        fn a_held_lock_was_written_as_its_daemon_took_it() {
            let temp = tempfile::tempdir().unwrap();
            let state = StateDir::create(temp.path()).unwrap();
            let before = SystemTime::now() - Duration::from_secs(1);
            let missing = lock_held_since(temp.path());
            let _lock = state.lock().unwrap();

            let since = lock_held_since(temp.path()).unwrap().unwrap();

            assert_eq!(missing.unwrap(), None);
            assert!(since >= before, "{since:?} is before {before:?}");
            assert!(since <= SystemTime::now());
        }

        #[test]
        fn only_the_recorded_process_runs() {
            let mut child = sleeper();
            let holder = record_of(&child);
            let later = LockHolder {
                started: holder.started.map(|started| started + 1),
                ..holder.clone()
            };
            let unknown_start = LockHolder {
                started: None,
                ..holder.clone()
            };
            let other_boot = LockHolder {
                boot: Some(BootId::new("another boot".to_owned())),
                ..holder.clone()
            };

            let runs = [&holder, &later, &unknown_start, &other_boot].map(LockHolder::runs);
            let _ = child.kill();
            let _ = child.wait();

            assert_eq!(runs, [true, false, false, false]);
            assert!(!holder.runs(), "an ended process");
        }

        #[test]
        fn a_stop_without_a_daemon_fails() {
            let temp = tempfile::tempdir().unwrap();

            let stopped = stop_lock_holder(temp.path());

            assert!(
                matches!(stopped, Err(StopLockHolderError::NoDaemon)),
                "{stopped:?}"
            );
        }

        #[test]
        fn a_stop_leaves_the_daemon_in_this_process_alone() {
            let temp = tempfile::tempdir().unwrap();
            let state = StateDir::create(temp.path()).unwrap();
            let _lock = state.lock().unwrap();

            let stopped = stop_lock_holder(temp.path());

            assert!(
                matches!(stopped, Err(StopLockHolderError::ThisProcess)),
                "{stopped:?}"
            );
        }

        #[test]
        fn a_stop_leaves_a_process_with_another_start_time_alone() {
            let temp = tempfile::tempdir().unwrap();
            let state = StateDir::create(temp.path()).unwrap();
            let _lock = state.lock().unwrap();
            let mut child = sleeper();
            let mut holder = record_of(&child);
            holder.started = holder.started.map(|started| started - 1);
            write_record(temp.path(), &holder);

            let stopped = stop_lock_holder(temp.path());

            let running = child.try_wait().unwrap().is_none();
            let _ = child.kill();
            let _ = child.wait();
            assert!(
                matches!(stopped, Err(StopLockHolderError::Unverified { .. })),
                "{stopped:?}"
            );
            assert!(running, "a process that got the pid runs on");
        }

        /// The test holds the lock, so the signalled process cannot let it go.
        #[test]
        fn a_stop_signals_the_holder_and_waits_for_the_lock() {
            let temp = tempfile::tempdir().unwrap();
            let state = StateDir::create(temp.path()).unwrap();
            let _lock = state.lock().unwrap();
            let mut child = sleeper();
            write_record(temp.path(), &record_of(&child));
            let wait = Duration::from_millis(300);

            let started = Instant::now();
            let stopped = stop_within(temp.path(), wait);
            let waited = started.elapsed();

            let status = child.wait().unwrap();
            assert_eq!(status.signal(), Some(Signal::TERM.as_raw()));
            assert!(
                matches!(stopped, Err(StopLockHolderError::StillRunning { .. })),
                "{stopped:?}"
            );
            assert!(waited >= wait, "{waited:?}");
        }
    }
}

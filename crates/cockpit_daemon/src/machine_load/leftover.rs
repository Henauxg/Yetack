//! The `claude` a daemon that died left running. Its stdin closed with the daemon, so it exits by
//! itself, but only once its turn ends, with its builds still running: a daemon that resumed
//! its session meanwhile would run two `claude` on one session.
//!
//! `agents.json` records each running `claude` by its id and its start time. A daemon that
//! starts stops each recorded process that still runs with the same start time, and leaves
//! alone a process that got the id since. On Linux, the start time comes from `/proc`; on a
//! Unix without `/proc`, nothing is recorded and nothing is stopped.

use std::time::Duration;

use cockpit_protocol::ProcessId;

use crate::store::RecordedProcess;

/// How long the leftover processes have to end after the polite stop.
const STOP_WAIT: Duration = Duration::from_secs(5);
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);

/// The process `pid` with its start time, or `None` when it no longer runs.
pub(crate) fn record_process(pid: ProcessId) -> Option<RecordedProcess> {
    started(pid).map(|started| RecordedProcess { pid, started })
}

/// When `pid` started, unless it has ended.
#[cfg(unix)]
fn started(pid: ProcessId) -> Option<u64> {
    use crate::machine_load::proc_files::{PROC_ROOT, ProcessStat, STAT_FILE};

    let line = std::fs::read(format!("{PROC_ROOT}/{}/{STAT_FILE}", pid.get())).ok()?;
    let stat = ProcessStat::parse(&String::from_utf8_lossy(&line))?;
    (!stat.zombie).then_some(stat.key.started)
}

/// Stops each of `recorded` that still runs, with the processes it started: SIGTERM to its
/// process group, then SIGKILL to the groups whose `claude` still runs after [`STOP_WAIT`].
/// It blocks for that long at most.
#[cfg(unix)]
pub(crate) fn stop_leftovers(recorded: &[RecordedProcess]) {
    use rustix::process::{Pid, Signal};

    let to_pid = |process: &RecordedProcess| {
        i32::try_from(process.pid.get())
            .ok()
            .and_then(Pid::from_raw)
    };
    let mut stopping: Vec<RecordedProcess> = Vec::new();
    for process in recorded {
        let Some(pid) = to_pid(process) else {
            continue;
        };
        if started(process.pid) != Some(process.started) {
            continue;
        }
        tracing::warn!(
            pid = process.pid.get(),
            "stopping the claude a previous daemon left running, and its process group"
        );
        // `claude` leads its own group. A failure means the group ended meanwhile.
        if rustix::process::kill_process_group(pid, Signal::TERM).is_err() {
            let _ = rustix::process::kill_process(pid, Signal::TERM);
        }
        stopping.push(*process);
    }
    let deadline = std::time::Instant::now() + STOP_WAIT;
    while std::time::Instant::now() < deadline {
        stopping.retain(|process| started(process.pid) == Some(process.started));
        if stopping.is_empty() {
            return;
        }
        std::thread::sleep(POLL);
    }
    for process in &stopping {
        let Some(pid) = to_pid(process) else {
            continue;
        };
        tracing::warn!(
            pid = process.pid.get(),
            "the leftover claude did not stop: killing it"
        );
        if rustix::process::kill_process_group(pid, Signal::KILL).is_err() {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
        }
    }
}

/// When `pid` started, unless it has ended.
#[cfg(windows)]
fn started(pid: ProcessId) -> Option<u64> {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    use crate::machine_load::opened_process::OpenedProcess;

    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let process = OpenedProcess::open(pid, access).ok()?;
    if process.has_ended() {
        return None;
    }
    process.times().ok().map(|times| times.started)
}

/// Ends each of `recorded` that still runs, and waits up to [`STOP_WAIT`] for it. A `claude` in a
/// job died with the daemon that held the job, so only one that ran without a job is left: it
/// ends alone.
#[cfg(windows)]
pub(crate) fn stop_leftovers(recorded: &[RecordedProcess]) {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    use crate::machine_load::opened_process::OpenedProcess;

    let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    for recorded in recorded {
        // The open handle keeps the id from going to another process before the stop.
        let Ok(process) = OpenedProcess::open(recorded.pid, access) else {
            continue;
        };
        let same = process
            .times()
            .is_ok_and(|times| times.started == recorded.started);
        if !same || process.has_ended() {
            continue;
        }
        tracing::warn!(
            pid = recorded.pid.get(),
            "stopping the claude a previous daemon left running"
        );
        if let Err(error) = process.terminate() {
            tracing::warn!(%error, pid = recorded.pid.get(), "cannot stop the leftover claude");
            continue;
        }
        process.wait(STOP_WAIT);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, Stdio};

    use rustix::process::Signal;

    use super::*;

    /// A `sleep` that leads its own process group, as `claude` does.
    fn leader() -> Child {
        Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_recorded_process_that_still_runs_is_stopped() {
        let mut child = leader();
        let recorded = record_process(ProcessId::new(child.id())).unwrap();

        stop_leftovers(&[recorded]);

        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(Signal::TERM.as_raw()));
        assert_eq!(record_process(recorded.pid), None);
    }

    #[test]
    fn a_process_that_got_the_id_since_is_left_alone() {
        let mut child = leader();
        let mut recorded = record_process(ProcessId::new(child.id())).unwrap();
        recorded.started -= 1;

        stop_leftovers(&[recorded]);

        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(running, "a process with another start time runs on");
    }
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    use super::*;

    fn waiting_shell() -> Child {
        Command::new("cmd.exe")
            .raw_arg("/d /c \"ping -n 30 127.0.0.1 >nul\"")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_recorded_process_that_still_runs_is_stopped_and_another_left_alone() {
        let mut leftover = waiting_shell();
        let mut other = waiting_shell();
        let recorded = record_process(ProcessId::new(leftover.id())).unwrap();
        let mut reused = record_process(ProcessId::new(other.id())).unwrap();
        reused.started -= 1;

        stop_leftovers(&[recorded, reused]);

        let other_runs = other.try_wait().unwrap().is_none();
        let _ = other.kill();
        let _ = other.wait();
        let stopped = leftover.try_wait().unwrap().is_some();
        let _ = leftover.kill();
        let _ = leftover.wait();
        assert!(stopped, "the leftover ends");
        assert!(other_runs, "a process with another start time runs on");
    }

    /// The child's handle keeps the ended process's id from going to another process.
    #[test]
    fn an_ended_process_is_not_recorded() {
        let mut ended = waiting_shell();
        ended.kill().unwrap();
        ended.wait().unwrap();

        assert_eq!(record_process(ProcessId::new(ended.id())), None);
    }
}

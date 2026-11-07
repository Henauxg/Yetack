//! The `claude` a daemon that died left running. Its stdin closed with the daemon, so it exits by
//! itself, but only once its turn ends, with its builds still running: a daemon that resumed
//! its session meanwhile would run two `claude` on one session.
//!
//! `agents.json` records each running `claude` by its id and its start time. A daemon that
//! starts stops each recorded process that still runs with the same start time, and leaves
//! alone a process that got the id since. On Linux, the start time comes from `/proc`; on a
//! Unix without `/proc`, nothing is recorded and nothing is stopped.
//!
//! On Linux, the stop reaches the processes `claude` started through its process group. On
//! Windows, a `claude` without a job is stopped with every process under it, which a snapshot of
//! the machine's processes finds: for an npm `claude.cmd`, the recorded process is `cmd.exe`, and
//! the real `claude` is its child.

use std::time::Duration;

use cockpit_protocol::ProcessId;

use crate::store::{BootId, RecordedProcess};

/// How long the leftover processes have to end after the polite stop.
const STOP_WAIT: Duration = Duration::from_secs(5);
/// The most processes stopped under one leftover `claude`.
#[cfg(any(windows, test))]
const MAX_TREE: usize = 4096;
#[cfg(unix)]
const POLL: Duration = Duration::from_millis(50);

/// The process `pid` with its start time and the boot it runs in, or `None` when it no longer
/// runs.
pub(crate) fn record_process(pid: ProcessId) -> Option<RecordedProcess> {
    started(pid).map(|started| RecordedProcess {
        pid,
        started,
        boot: boot(),
    })
}

/// This boot of the machine. A start time on Linux counts clock ticks from the boot, so a
/// process of a later boot can have the recorded one's id and start time.
#[cfg(unix)]
fn boot() -> Option<BootId> {
    const BOOT_ID_FILE: &str = "/proc/sys/kernel/random/boot_id";

    let id = std::fs::read_to_string(BOOT_ID_FILE).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| BootId::new(id.to_owned()))
}

/// Windows dates a process's start in 100 ns units since 1601: a process of a later boot cannot
/// have the recorded start, so the start time alone tells the boots apart.
#[cfg(windows)]
fn boot() -> Option<BootId> {
    None
}

/// Whether a process recorded in `recorded` can be one of this boot, `current`. Either one
/// unknown leaves the start time alone to decide.
#[cfg(any(unix, test))]
fn same_boot(recorded: Option<&BootId>, current: Option<&BootId>) -> bool {
    match (recorded, current) {
        (Some(recorded), Some(current)) => recorded == current,
        (None, _) | (_, None) => true,
    }
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
    let current = boot();
    let mut stopping: Vec<&RecordedProcess> = Vec::new();
    for process in recorded {
        let Some(pid) = to_pid(process) else {
            continue;
        };
        if !same_boot(process.boot.as_ref(), current.as_ref()) {
            tracing::info!(
                pid = process.pid.get(),
                "the recorded claude ran before the machine restarted"
            );
            continue;
        }
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
        stopping.push(process);
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

/// Ends each of `recorded` that still runs, with every process under it, and waits up to
/// [`STOP_WAIT`] for them. A `claude` in a job died with the daemon that held the job, so only
/// one that ran without a job is left.
#[cfg(windows)]
pub(crate) fn stop_leftovers(recorded: &[RecordedProcess]) {
    use windows_sys::Win32::System::Threading::{
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    use crate::machine_load::opened_process::OpenedProcess;

    let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    let open = |pid: ProcessId| {
        let process = OpenedProcess::open(pid, access).ok()?;
        let started = process.times().ok()?.started;
        (!process.has_ended()).then_some((process, started))
    };
    let mut links: Option<Vec<ParentLink>> = None;
    for recorded in recorded {
        // The open handles keep the ids from going to other processes before the stop.
        let Some((process, started)) = open(recorded.pid) else {
            continue;
        };
        if started != recorded.started {
            continue;
        }
        let links = links.get_or_insert_with(|| {
            parent_links()
                .inspect_err(|error| tracing::warn!(%error, "cannot list the processes"))
                .unwrap_or_default()
        });
        let under = descendants(links, recorded.pid, started, open);
        tracing::warn!(
            pid = recorded.pid.get(),
            processes = under.len(),
            "stopping the claude a previous daemon left running, and the processes under it"
        );
        let deadline = std::time::Instant::now() + STOP_WAIT;
        let tree: Vec<OpenedProcess> = std::iter::once(process).chain(under).collect();
        for process in &tree {
            if let Err(error) = process.terminate()
                && !process.has_ended()
            {
                tracing::warn!(%error, pid = recorded.pid.get(), "cannot stop a leftover process");
            }
        }
        for process in &tree {
            process.wait(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }
}

/// A process and the one that started it, as a snapshot of the machine's processes lists them.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParentLink {
    pid: ProcessId,
    parent: ProcessId,
}

/// Every process of the machine with its parent.
#[cfg(windows)]
fn parent_links() -> std::io::Result<Vec<ParentLink>> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: the flag asks for a snapshot of the processes; the process id is ignored for them.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the handle is open and nothing else owns it.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..PROCESSENTRY32W::default()
    };
    let mut links = Vec::new();
    // SAFETY: the snapshot is open, and `entry` is a `PROCESSENTRY32W` whose size field is set.
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    while more {
        links.push(ParentLink {
            pid: ProcessId::new(entry.th32ProcessID),
            parent: ProcessId::new(entry.th32ParentProcessID),
        });
        // SAFETY: as for `Process32FirstW`.
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    Ok(links)
}

/// The processes under `root`, which started at `root_started`, each as `open` gives it with its
/// start, at most [`MAX_TREE`]. A process that started before the process its parent id names
/// is left out: its parent ended, and the id went to that other process.
#[cfg(any(windows, test))]
fn descendants<P>(
    links: &[ParentLink],
    root: ProcessId,
    root_started: u64,
    mut open: impl FnMut(ProcessId) -> Option<(P, u64)>,
) -> Vec<P> {
    use std::collections::{HashSet, VecDeque};

    let mut found = Vec::new();
    let mut seen: HashSet<ProcessId> = HashSet::from([root]);
    let mut parents: VecDeque<(ProcessId, u64)> = VecDeque::from([(root, root_started)]);
    while let Some((parent, parent_started)) = parents.pop_front() {
        let children = links.iter().filter(|link| link.parent == parent);
        for child in children {
            if found.len() >= MAX_TREE || !seen.insert(child.pid) {
                continue;
            }
            let Some((process, started)) = open(child.pid) else {
                continue;
            };
            if started < parent_started {
                continue;
            }
            parents.push_back((child.pid, started));
            found.push(process);
        }
    }
    found
}

#[cfg(test)]
mod logic_tests {
    use std::collections::HashMap;

    use super::*;

    fn link(pid: u32, parent: u32) -> ParentLink {
        ParentLink {
            pid: ProcessId::new(pid),
            parent: ProcessId::new(parent),
        }
    }

    #[test]
    fn the_tree_holds_the_processes_started_under_the_root_since_it_started() {
        // 10 cmd.exe, 11 node under it, 12 a build node runs, 13 its compiler; 20 a process
        // that took the id of an ended child of 11 before 11 started; 30 another program.
        let links = [
            link(10, 1),
            link(11, 10),
            link(12, 11),
            link(13, 12),
            link(21, 20),
            link(20, 11),
            link(30, 1),
        ];
        let started: HashMap<u32, u64> = HashMap::from([
            (10, 100),
            (11, 101),
            (12, 150),
            (13, 160),
            (20, 50),
            (21, 60),
            (30, 90),
        ]);
        let open = |pid: ProcessId| started.get(&pid.get()).map(|&time| (pid.get(), time));

        let mut tree = descendants(&links, ProcessId::new(10), 100, open);
        tree.sort_unstable();

        assert_eq!(tree, [11, 12, 13]);
    }

    #[test]
    fn only_a_known_other_boot_rules_a_record_out() {
        let this = BootId::new("this".to_owned());
        let other = BootId::new("other".to_owned());

        assert!(same_boot(Some(&this), Some(&this)));
        assert!(!same_boot(Some(&other), Some(&this)));
        assert!(same_boot(None, Some(&this)), "a file saved before boots");
        assert!(same_boot(Some(&other), None), "a boot that cannot be read");
    }

    #[test]
    fn a_loop_in_the_links_ends_the_walk() {
        let links = [link(10, 11), link(11, 10)];

        let tree = descendants(&links, ProcessId::new(10), 0, |pid| Some((pid, 1)));

        assert_eq!(tree, [ProcessId::new(11)]);
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

        stop_leftovers(std::slice::from_ref(&recorded));

        let status = child.wait().unwrap();
        assert!(recorded.boot.is_some(), "Linux records the boot");
        assert_eq!(status.signal(), Some(Signal::TERM.as_raw()));
        assert_eq!(record_process(recorded.pid), None);
    }

    #[test]
    fn a_process_recorded_in_another_boot_is_left_alone() {
        let mut child = leader();
        let mut recorded = record_process(ProcessId::new(child.id())).unwrap();
        recorded.boot = Some(BootId::new("another boot".to_owned()));

        stop_leftovers(&[recorded]);

        let running = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(running, "a process of this boot runs on");
    }

    #[test]
    fn a_record_without_a_boot_is_stopped_by_its_start_time() {
        let mut child = leader();
        let mut recorded = record_process(ProcessId::new(child.id())).unwrap();
        recorded.boot = None;

        stop_leftovers(&[recorded]);

        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(Signal::TERM.as_raw()));
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
    use std::time::Duration;

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

    #[test]
    fn a_leftover_without_a_job_is_stopped_with_the_processes_under_it() {
        let mut leftover = waiting_shell();
        let root = ProcessId::new(leftover.id());
        let recorded = record_process(root).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let ping = loop {
            let links = parent_links().unwrap();
            if let Some(link) = links.iter().find(|link| link.parent == root) {
                break link.pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the shell started no command"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let ping_started = record_process(ping).unwrap();

        stop_leftovers(&[recorded]);

        let stopped = leftover.try_wait().unwrap().is_some();
        let _ = leftover.kill();
        let _ = leftover.wait();
        assert!(stopped, "the leftover ends");
        assert_ne!(
            record_process(ping),
            Some(ping_started),
            "the command under it ends too"
        );
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

//! Who may connect: a process of the daemon's user that does not descend from the daemon, so a
//! command an agent runs cannot drive the daemon.
//!
//! The parent chain stops a process that simply connects. A process that detaches on purpose
//! (double fork, `setsid`) gets a new parent and passes: Claude Code's sandbox is what keeps the
//! agents' commands out of the state folder.
//!
//! The walk up the chain stops, and accepts, at a process of another user or one that `/proc`
//! hides (`hidepid=1` or `2`), such as root's `sshd` above the SSH bridge. The daemon's
//! descendants all run as its user, and `/proc` always shows a user's own processes.

use std::io;
use std::path::Path;

use cockpit_protocol::ProcessId;
use tokio::net::UnixStream;

use crate::machine_load::{PROC_ROOT, STAT_FILE};

/// Longer than any real chain: a loop in a corrupt `/proc` cannot run forever.
const MAX_CHAIN: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(u32);

impl UserId {
    #[must_use]
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("cannot read the peer's credentials")]
    Credentials(#[source] io::Error),
    /// The peer is in a PID namespace the daemon cannot see.
    #[error("the peer's credentials carry no process id")]
    NoPid,
    #[error("the peer runs as user {}, not as the daemon's user", .uid.get())]
    OtherUser { uid: UserId },
    #[error("process {} descends from the daemon", .peer.get())]
    DescendsFromDaemon { peer: ProcessId },
    #[error("cannot read the parent of process {}", .pid.get())]
    Proc {
        pid: ProcessId,
        #[source]
        source: io::Error,
    },
    #[error("the stat of process {} holds no parent", .pid.get())]
    MalformedStat { pid: ProcessId },
    #[error("the parent chain of process {} does not end", .peer.get())]
    ChainTooLong { peer: ProcessId },
}

/// The daemon's own user and process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonIdentity {
    uid: UserId,
    pid: ProcessId,
}

impl DaemonIdentity {
    pub(crate) fn current() -> Self {
        Self {
            uid: UserId::new(rustix::process::getuid().as_raw()),
            pid: ProcessId::new(std::process::id()),
        }
    }
}

/// What the walk reads of a process in `/proc`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcEntry {
    /// The owner of `/proc/<pid>`: the process's user.
    owner: UserId,
    /// The content of `/proc/<pid>/stat`.
    stat: String,
}

/// Runs before any byte of the connection is read.
pub(crate) fn check(stream: &UnixStream, daemon: DaemonIdentity) -> Result<(), PeerError> {
    let credentials = stream.peer_cred().map_err(PeerError::Credentials)?;
    let uid = UserId::new(credentials.uid());
    if uid != daemon.uid {
        return Err(PeerError::OtherUser { uid });
    }
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|&pid| pid > 0)
        .map(ProcessId::new)
        .ok_or(PeerError::NoPid)?;
    check_ancestry(pid, daemon, read_proc_entry)
}

/// Walks from `peer` up its parents, read by `read_entry`, and refuses it when one is the daemon.
fn check_ancestry(
    peer: ProcessId,
    daemon: DaemonIdentity,
    read_entry: impl Fn(ProcessId) -> io::Result<ProcEntry>,
) -> Result<(), PeerError> {
    let mut pid = peer;
    for _ in 0..MAX_CHAIN {
        if pid == daemon.pid {
            return Err(PeerError::DescendsFromDaemon { peer });
        }
        // 1 is init; 0 is the kernel, the parent of init.
        if pid.get() <= 1 {
            return Ok(());
        }
        let entry = match read_entry(pid) {
            Ok(entry) => entry,
            // `hidepid=2` hides another user's process; `hidepid=1` refuses to show its files.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(());
            }
            Err(source) => return Err(PeerError::Proc { pid, source }),
        };
        if entry.owner != daemon.uid {
            return Ok(());
        }
        pid = parent(&entry.stat).ok_or(PeerError::MalformedStat { pid })?;
    }
    Err(PeerError::ChainTooLong { peer })
}

fn read_proc_entry(pid: ProcessId) -> io::Result<ProcEntry> {
    use std::os::unix::fs::MetadataExt;

    let folder = Path::new(PROC_ROOT).join(pid.get().to_string());
    let owner = UserId::new(std::fs::metadata(&folder)?.uid());
    // A name the kernel cut at 15 bytes can end inside a character.
    let stat = String::from_utf8_lossy(&std::fs::read(folder.join(STAT_FILE))?).into_owned();
    Ok(ProcEntry { owner, stat })
}

/// The parent in a `stat` line: `pid (name) state ppid ...`. The name may hold spaces and
/// parentheses, so the fields count from its last `)`.
fn parent(stat: &str) -> Option<ProcessId> {
    let after_name = stat.rfind(')').and_then(|end| stat.get(end + 1..))?;
    let mut fields = after_name.split_whitespace();
    let _state = fields.next();
    let parent: u32 = fields.next()?.parse().ok()?;
    Some(ProcessId::new(parent))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const USER: UserId = UserId(1000);
    const ROOT: UserId = UserId(0);
    const DAEMON: DaemonIdentity = DaemonIdentity {
        uid: USER,
        pid: ProcessId::new(50),
    };

    fn entry(owner: UserId, stat: &str) -> Result<ProcEntry, io::ErrorKind> {
        Ok(ProcEntry {
            owner,
            stat: stat.to_owned(),
        })
    }

    /// A `/proc` with these entries, where any other process is missing.
    fn fake_proc(
        entries: Vec<(ProcessId, Result<ProcEntry, io::ErrorKind>)>,
    ) -> impl Fn(ProcessId) -> io::Result<ProcEntry> {
        let entries: HashMap<ProcessId, Result<ProcEntry, io::ErrorKind>> =
            entries.into_iter().collect();
        move |pid| match entries.get(&pid) {
            Some(Ok(entry)) => Ok(entry.clone()),
            Some(Err(kind)) => Err(io::Error::from(*kind)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    /// 1 init and 40 sshd, of root; 41 the bridge; 50 the daemon, 60 its claude, and 70 a shell
    /// with a strange name. `sshd` is as `/proc` shows it.
    fn processes(
        sshd: Result<ProcEntry, io::ErrorKind>,
    ) -> impl Fn(ProcessId) -> io::Result<ProcEntry> {
        fake_proc(vec![
            (ProcessId::new(1), entry(ROOT, "1 (init) S 0 1 1")),
            (ProcessId::new(40), sshd),
            (
                ProcessId::new(41),
                entry(USER, "41 (agents-cockpit) S 40 41 41"),
            ),
            (
                ProcessId::new(50),
                entry(USER, "50 (agents-cockpit) S 1 50 50"),
            ),
            (ProcessId::new(60), entry(USER, "60 (claude) S 50 60 60")),
            (ProcessId::new(70), entry(USER, "70 (sh (x) y) R 60 70 70")),
        ])
    }

    #[test]
    fn a_descendant_of_the_daemon_is_refused_and_others_pass() {
        let proc = processes(entry(ROOT, "40 (sshd) S 1 40 40"));

        let bridge = check_ancestry(ProcessId::new(41), DAEMON, &proc);
        let agent_shell = check_ancestry(ProcessId::new(70), DAEMON, &proc);
        let daemon_itself = check_ancestry(ProcessId::new(50), DAEMON, &proc);

        assert!(bridge.is_ok(), "{bridge:?}");
        assert!(
            matches!(
                agent_shell,
                Err(PeerError::DescendsFromDaemon { peer }) if peer == ProcessId::new(70)
            ),
            "{agent_shell:?}"
        );
        assert!(matches!(
            daemon_itself,
            Err(PeerError::DescendsFromDaemon { peer }) if peer == ProcessId::new(50)
        ));
    }

    /// With `hidepid`, root's `sshd` above the bridge is missing or unreadable.
    #[test]
    fn the_walk_accepts_at_a_hidden_process_or_one_of_another_user() {
        let hidden = [
            Err(io::ErrorKind::NotFound),
            Err(io::ErrorKind::PermissionDenied),
            entry(ROOT, "40 (sshd) S 1 40 40"),
        ];

        for sshd in hidden {
            let shown = format!("{sshd:?}");
            let proc = processes(sshd);

            let bridge = check_ancestry(ProcessId::new(41), DAEMON, &proc);
            let agent_shell = check_ancestry(ProcessId::new(70), DAEMON, &proc);

            assert!(bridge.is_ok(), "{shown}: {bridge:?}");
            assert!(
                matches!(
                    agent_shell,
                    Err(PeerError::DescendsFromDaemon { peer }) if peer == ProcessId::new(70)
                ),
                "{shown}: {agent_shell:?}"
            );
        }
    }

    #[test]
    fn an_unreadable_chain_is_refused() {
        let proc = fake_proc(vec![
            (
                ProcessId::new(30),
                entry(USER, "30 (failing-parent) S 31 30 30"),
            ),
            (ProcessId::new(31), Err(io::ErrorKind::Other)),
            (ProcessId::new(32), entry(USER, "32 (broken")),
            (ProcessId::new(33), entry(USER, "33 (self) S 33 33 33")),
        ]);

        let failing = check_ancestry(ProcessId::new(30), DAEMON, &proc);
        let malformed = check_ancestry(ProcessId::new(32), DAEMON, &proc);
        let looping = check_ancestry(ProcessId::new(33), DAEMON, &proc);

        assert!(
            matches!(failing, Err(PeerError::Proc { pid, .. }) if pid == ProcessId::new(31)),
            "{failing:?}"
        );
        assert!(
            matches!(malformed, Err(PeerError::MalformedStat { pid }) if pid == ProcessId::new(32)),
            "{malformed:?}"
        );
        assert!(
            matches!(looping, Err(PeerError::ChainTooLong { peer }) if peer == ProcessId::new(33)),
            "{looping:?}"
        );
    }

    #[test]
    fn the_real_proc_shows_this_test_descends_from_itself_only() {
        let me = DaemonIdentity::current();
        let another = DaemonIdentity {
            pid: ProcessId::new(u32::MAX),
            ..me
        };

        let from_itself = check_ancestry(me.pid, me, read_proc_entry);
        let from_another = check_ancestry(me.pid, another, read_proc_entry);

        assert!(matches!(
            from_itself,
            Err(PeerError::DescendsFromDaemon { .. })
        ));
        assert!(from_another.is_ok(), "{from_another:?}");
    }

    #[test]
    fn a_parent_whose_name_is_not_utf8_is_walked_through() {
        use std::ffi::OsStr;
        use std::io::{BufRead, BufReader};
        use std::os::unix::ffi::OsStrExt;
        use std::process::{Command, Stdio};

        let folder = tempfile::tempdir().unwrap();
        let shell = folder.path().join(OsStr::from_bytes(b"sh\xff"));
        std::os::unix::fs::symlink("/bin/sh", &shell).unwrap();
        let mut child = Command::new(&shell)
            .arg("-c")
            .arg("sleep 30 & echo $!; wait")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let sleep: u32 = lines.next().unwrap().unwrap().parse().unwrap();
        let me = DaemonIdentity::current();
        let another = DaemonIdentity {
            pid: ProcessId::new(u32::MAX),
            ..me
        };

        let from_me = check_ancestry(ProcessId::new(sleep), me, read_proc_entry);
        let from_another = check_ancestry(ProcessId::new(sleep), another, read_proc_entry);

        let _ = Command::new("kill").arg(sleep.to_string()).status();
        let _ = child.wait();
        assert!(
            matches!(from_me, Err(PeerError::DescendsFromDaemon { .. })),
            "{from_me:?}"
        );
        assert!(from_another.is_ok(), "{from_another:?}");
    }
}

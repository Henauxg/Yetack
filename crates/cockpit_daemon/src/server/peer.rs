//! Who may connect: a process of the daemon's user that does not descend from the daemon, so a
//! command an agent runs cannot drive the daemon.
//!
//! The parent chain stops a process that simply connects. A process that detaches on purpose
//! (double fork, `setsid`) gets a new parent and passes: Claude Code's sandbox is what keeps the
//! agents' commands out of the state folder.

use std::io;
use std::path::Path;

use tokio::net::UnixStream;

const PROC_ROOT: &str = "/proc";
const STAT_FILE: &str = "stat";
/// Longer than any real chain: a loop in a corrupt `/proc` cannot run forever.
const MAX_CHAIN: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("cannot read the peer's credentials")]
    Credentials(#[source] io::Error),
    /// The peer is in a PID namespace the daemon cannot see.
    #[error("the peer's credentials carry no process id")]
    NoPid,
    #[error("the peer runs as user {uid}, not as the daemon's user")]
    OtherUser { uid: u32 },
    #[error("process {peer} descends from the daemon")]
    DescendsFromDaemon { peer: u32 },
    #[error("cannot read the parent of process {pid}")]
    Proc {
        pid: u32,
        #[source]
        source: io::Error,
    },
    #[error("the stat of process {pid} holds no parent")]
    MalformedStat { pid: u32 },
    #[error("the parent chain of process {peer} does not end")]
    ChainTooLong { peer: u32 },
}

/// The daemon's own user and process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaemonIdentity {
    uid: u32,
    pid: u32,
}

impl DaemonIdentity {
    pub(crate) fn current() -> Self {
        Self {
            uid: rustix::process::getuid().as_raw(),
            pid: std::process::id(),
        }
    }
}

/// Runs before any byte of the connection is read.
pub(crate) fn check(stream: &UnixStream, daemon: DaemonIdentity) -> Result<(), PeerError> {
    let credentials = stream.peer_cred().map_err(PeerError::Credentials)?;
    if credentials.uid() != daemon.uid {
        return Err(PeerError::OtherUser {
            uid: credentials.uid(),
        });
    }
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|&pid| pid > 0)
        .ok_or(PeerError::NoPid)?;
    check_ancestry(Path::new(PROC_ROOT), pid, daemon.pid)
}

/// Walks from `peer` up its parents in `proc_root` and refuses it when one is `daemon`.
pub(crate) fn check_ancestry(proc_root: &Path, peer: u32, daemon: u32) -> Result<(), PeerError> {
    let mut pid = peer;
    for _ in 0..MAX_CHAIN {
        if pid == daemon {
            return Err(PeerError::DescendsFromDaemon { peer });
        }
        // 1 is init; 0 is the kernel, the parent of init.
        if pid <= 1 {
            return Ok(());
        }
        pid = parent(proc_root, pid)?;
    }
    Err(PeerError::ChainTooLong { peer })
}

/// Reads the parent from `<proc_root>/<pid>/stat`: `pid (name) state ppid ...`. The name may hold
/// spaces and parentheses, so the fields count from its last `)`.
fn parent(proc_root: &Path, pid: u32) -> Result<u32, PeerError> {
    let path = proc_root.join(pid.to_string()).join(STAT_FILE);
    let stat = std::fs::read_to_string(&path).map_err(|source| PeerError::Proc { pid, source })?;
    let after_name = stat
        .rfind(')')
        .and_then(|end| stat.get(end + 1..))
        .ok_or(PeerError::MalformedStat { pid })?;
    let mut fields = after_name.split_whitespace();
    let _state = fields.next();
    fields
        .next()
        .and_then(|ppid| ppid.parse().ok())
        .ok_or(PeerError::MalformedStat { pid })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    /// A `/proc` with the given (pid, stat line) pairs.
    fn fake_proc(processes: &[(u32, &str)]) -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        for (pid, stat) in processes {
            let folder = temp.path().join(pid.to_string());
            std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join(STAT_FILE), stat).unwrap();
        }
        let root = temp.path().to_owned();
        (temp, root)
    }

    #[test]
    fn a_descendant_of_the_daemon_is_refused_and_others_pass() {
        // 1 init, 40 sshd, 41 bridge; 50 daemon, 60 claude, 70 a shell with a strange name.
        let (_temp, root) = fake_proc(&[
            (1, "1 (init) S 0 1 1"),
            (40, "40 (sshd) S 1 40 40"),
            (41, "41 (agents-cockpit) S 40 41 41"),
            (50, "50 (agents-cockpit) S 1 50 50"),
            (60, "60 (claude) S 50 60 60"),
            (70, "70 (sh (x) y) R 60 70 70"),
        ]);

        let bridge = check_ancestry(&root, 41, 50);
        let agent_shell = check_ancestry(&root, 70, 50);
        let daemon_itself = check_ancestry(&root, 50, 50);

        assert!(bridge.is_ok(), "{bridge:?}");
        assert!(
            matches!(agent_shell, Err(PeerError::DescendsFromDaemon { peer: 70 })),
            "{agent_shell:?}"
        );
        assert!(matches!(
            daemon_itself,
            Err(PeerError::DescendsFromDaemon { peer: 50 })
        ));
    }

    #[test]
    fn an_unreadable_chain_is_refused() {
        let (_temp, root) = fake_proc(&[
            (30, "30 (gone-parent) S 31 30 30"),
            (32, "32 (broken"),
            (33, "33 (self) S 33 33 33"),
        ]);

        let missing = check_ancestry(&root, 30, 50);
        let malformed = check_ancestry(&root, 32, 50);
        let looping = check_ancestry(&root, 33, 50);

        assert!(
            matches!(missing, Err(PeerError::Proc { pid: 31, .. })),
            "{missing:?}"
        );
        assert!(
            matches!(malformed, Err(PeerError::MalformedStat { pid: 32 })),
            "{malformed:?}"
        );
        assert!(
            matches!(looping, Err(PeerError::ChainTooLong { peer: 33 })),
            "{looping:?}"
        );
    }

    #[test]
    fn the_real_proc_shows_this_test_descends_from_itself_only() {
        let me = std::process::id();

        let from_itself = check_ancestry(Path::new(PROC_ROOT), me, me);
        let from_another = check_ancestry(Path::new(PROC_ROOT), me, u32::MAX);

        assert!(matches!(
            from_itself,
            Err(PeerError::DescendsFromDaemon { .. })
        ));
        assert!(from_another.is_ok(), "{from_another:?}");
    }
}

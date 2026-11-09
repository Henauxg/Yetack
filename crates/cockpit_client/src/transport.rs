//! The ways to reach a daemon: its Unix socket, `agents-cockpit bridge` over SSH, or an
//! in-memory stream to a daemon in this process.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::DuplexStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};

use crate::error::ConnectError;

/// The `agents-cockpit` subcommand that copies bytes between its stdin and stdout and the
/// daemon's socket.
pub const BRIDGE_SUBCOMMAND: &str = "bridge";

const SSH_PROGRAM: &str = "ssh";

/// How much of ssh's stderr an error quotes.
const STDERR_TAIL_BYTES: usize = 4 * 1024;

/// How long a failed connection waits for ssh to exit and close its stderr.
const STDERR_WAIT: Duration = Duration::from_secs(2);

/// The bytes an in-memory stream holds in each direction before its writer waits.
const IN_APP_BUFFER: usize = 64 * 1024;

/// Where the daemon is. It comes from the app's config or the app's own choice, never from the
/// daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// The daemon runs on this machine.
    Local { socket: PathBuf },
    /// Runs `ssh <host> <remote_command> bridge` and speaks the protocol over its stdin and
    /// stdout. `remote_command` is the absolute path of `agents-cockpit` on the host, since a
    /// non-interactive SSH command may not have it on its `PATH`. The host's shell reads it as
    /// it is: it expands no `~` and no variable.
    Ssh {
        host: String,
        remote_command: String,
    },
    /// The daemon runs in this process. Each connection is an in-memory stream that carries the
    /// same frames as a socket.
    InApp(InAppLink),
}

/// The way to a daemon in this process: each connection sends the daemon its end of a new
/// in-memory stream.
#[derive(Debug, Clone)]
pub struct InAppLink {
    streams: mpsc::UnboundedSender<DuplexStream>,
}

impl InAppLink {
    /// The link, and the receiver the daemon takes each connection's stream from.
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<DuplexStream>) {
        let (streams, received) = mpsc::unbounded_channel();
        (Self { streams }, received)
    }
}

/// Two links are equal when they reach the same daemon.
impl PartialEq for InAppLink {
    fn eq(&self, other: &Self) -> bool {
        self.streams.same_channel(&other.streams)
    }
}

impl Eq for InAppLink {}

pub(crate) struct SshLink {
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: ChildStdout,
    pub(crate) process: SshProcess,
}

/// The ssh child process. Dropping it kills ssh.
pub(crate) struct SshProcess {
    _child: Child,
    stderr_tail: oneshot::Receiver<String>,
}

impl SshProcess {
    /// The end of what ssh wrote to stderr, once ssh closed it. `None` when ssh wrote nothing or
    /// still runs.
    pub(crate) async fn stderr_tail(self) -> Option<String> {
        let tail = tokio::time::timeout(STDERR_WAIT, self.stderr_tail)
            .await
            .ok()?
            .ok()?;
        (!tail.is_empty()).then_some(tail)
    }
}

#[cfg(unix)]
pub(crate) async fn connect_socket(socket: &Path) -> Result<UnixStream, ConnectError> {
    UnixStream::connect(socket)
        .await
        .map_err(|source| ConnectError::Socket {
            path: socket.to_owned(),
            source,
        })
}

/// The tokio runtime has Unix sockets on Unix only.
#[cfg(windows)]
pub(crate) fn socket_unsupported(socket: &Path) -> ConnectError {
    ConnectError::Socket {
        path: socket.to_owned(),
        source: io::Error::new(
            io::ErrorKind::Unsupported,
            "the app reaches a daemon's socket on Unix only",
        ),
    }
}

/// Opens an in-memory stream and hands its other end to the daemon.
pub(crate) fn connect_in_app(link: &InAppLink) -> Result<DuplexStream, ConnectError> {
    let (client, daemon) = tokio::io::duplex(IN_APP_BUFFER);
    link.streams
        .send(daemon)
        .map_err(|_| ConnectError::InAppStopped)?;
    Ok(client)
}

/// ssh would read such a host as an option, such as `-oProxyCommand=...`.
fn check_host(host: &str) -> Result<(), ConnectError> {
    if host.is_empty() || host.starts_with('-') {
        return Err(ConnectError::InvalidHost {
            host: host.to_owned(),
        });
    }
    Ok(())
}

/// Starts ssh. Its stdin and stdout carry the protocol.
pub(crate) fn spawn_ssh(host: &str, remote_command: &str) -> Result<SshLink, ConnectError> {
    check_host(host)?;
    let mut child = ssh_command(host, remote_command)
        .spawn()
        .map_err(ConnectError::Spawn)?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(ConnectError::Spawn(io::Error::other(
            "ssh started without its pipes",
        )));
    };
    let (tail_sender, stderr_tail) = oneshot::channel();
    tokio::spawn(collect_stderr(stderr, tail_sender));
    Ok(SshLink {
        stdin,
        stdout,
        process: SshProcess {
            _child: child,
            stderr_tail,
        },
    })
}

/// ssh to `host` running a script that is written to its stdin, as `sh -s -- <arguments>`. It
/// needs no daemon, so it reaches a server whose daemon refuses this app.
pub fn ssh_script_command(host: &str, arguments: &[&str]) -> Result<Command, ConnectError> {
    check_host(host)?;
    let mut command = ssh_to(host);
    command.args(["sh", "-s", "--"]);
    command.args(arguments.iter().map(|argument| shell_quoted(argument)));
    Ok(command)
}

fn ssh_command(host: &str, remote_command: &str) -> Command {
    let mut command = ssh_to(host);
    command
        .arg(shell_quoted(remote_command))
        .arg(BRIDGE_SUBCOMMAND);
    command
}

/// `BatchMode` makes a missing key fail at once, instead of waiting for a password the app cannot
/// show. `ForwardAgent=no` keeps the host from using this machine's keys. `-x` and
/// `ClearAllForwardings` open no X11 or port forwarding to this machine, even when the user's ssh
/// config asks for one. `ServerAliveInterval` and `ServerAliveCountMax` end ssh after a minute of
/// silence, where a dead link would otherwise look alive for many minutes. `--` ends the options,
/// since ssh reads options after the host too.
fn ssh_to(host: &str) -> Command {
    let mut command = Command::new(SSH_PROGRAM);
    command
        .args([
            "-T",
            "-x",
            "-o",
            "BatchMode=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=4",
            "--",
        ])
        .arg(host)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        // The desktop app has no console to share: Windows would open a window for ssh.
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Quotes `word` for a POSIX shell: ssh hands the remote command to the host's shell.
fn shell_quoted(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Logs what ssh writes to stderr, and sends the end of it once ssh closes it.
async fn collect_stderr(mut stderr: ChildStderr, tail_sender: oneshot::Sender<String>) {
    use tokio::io::AsyncReadExt;

    let mut tail = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let read = match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let Some(bytes) = chunk.get(..read) else {
            break;
        };
        tracing::warn!("ssh: {}", String::from_utf8_lossy(bytes).trim_end());
        tail.extend_from_slice(bytes);
        let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
        tail.drain(..excess);
    }
    // The connection may not wait for it.
    let _ = tail_sender.send(String::from_utf8_lossy(&tail).trim().to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_runs_the_bridge_without_a_password_prompt_or_any_forwarding() {
        let command = ssh_command("build-box", "/opt/agents-cockpit/bin/agents-cockpit");

        let std_command = command.as_std();
        let args: Vec<&str> = std_command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(std_command.get_program(), "ssh");
        assert_eq!(
            args,
            [
                "-T",
                "-x",
                "-o",
                "BatchMode=yes",
                "-o",
                "ForwardAgent=no",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=4",
                "--",
                "build-box",
                "'/opt/agents-cockpit/bin/agents-cockpit'",
                "bridge",
            ]
        );
    }

    #[test]
    fn the_bridge_and_a_script_both_send_keepalives() {
        let bridge = ssh_command("build-box", "/opt/agents-cockpit");
        let script = ssh_script_command("build-box", &["--cancel"]).unwrap();

        for command in [bridge, script] {
            let args: Vec<&str> = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect();
            let options = args.split(|&arg| arg == "--").next().unwrap();
            for keepalive in ["ServerAliveInterval=15", "ServerAliveCountMax=4"] {
                assert!(
                    options.windows(2).any(|pair| pair == ["-o", keepalive]),
                    "{keepalive}: {args:?}"
                );
            }
        }
    }

    #[test]
    fn options_end_before_the_host() {
        // After the host, ssh reads options again: this one would run `sh` on this machine.
        let command = ssh_command("build-box", "-oProxyCommand=sh");

        let args: Vec<&str> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        let host = args.iter().position(|&arg| arg == "build-box").unwrap();
        assert_eq!(args.get(host - 1), Some(&"--"), "{args:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_host_shell_reads_the_remote_command_as_it_is() {
        for path in [
            "/opt/my tools/agents-cockpit",
            "/opt/x;printf injected",
            "/opt/it's/$HOME/agents-cockpit",
        ] {
            let command = ssh_command("build-box", path);

            // ssh joins the words after the host with spaces, and the host's shell runs that.
            let remote: Vec<&str> = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .skip_while(|&arg| arg != "build-box")
                .skip(1)
                .collect();
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("printf '%s\\n' {}", remote.join(" ")))
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{path}\nbridge\n")
            );
        }
    }

    #[test]
    fn a_script_runs_in_the_hosts_shell_with_its_arguments_quoted() {
        let command = ssh_script_command("build-box", &["--binary", "/opt/my tools/x"]).unwrap();

        let args: Vec<&str> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args.get(12..).unwrap(),
            [
                "--",
                "build-box",
                "sh",
                "-s",
                "--",
                "'--binary'",
                "'/opt/my tools/x'",
            ]
        );
    }

    #[test]
    fn a_script_refuses_a_host_that_reads_as_an_option() {
        assert!(matches!(
            ssh_script_command("-oProxyCommand=sh", &[]),
            Err(ConnectError::InvalidHost { .. })
        ));
    }

    #[tokio::test]
    async fn an_in_app_link_hands_the_daemon_its_end_of_each_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (link, mut streams) = InAppLink::new();
        let (other, _) = InAppLink::new();
        assert_eq!(link, link.clone());
        assert_ne!(link, other);

        let mut client = connect_in_app(&link).unwrap();
        let mut daemon = streams.recv().await.unwrap();
        client.write_all(b"hello\n").await.unwrap();
        let mut read = [0; 6];
        daemon.read_exact(&mut read).await.unwrap();
        assert_eq!(&read, b"hello\n");

        drop(streams);
        assert!(matches!(
            connect_in_app(&link),
            Err(ConnectError::InAppStopped)
        ));
    }

    #[test]
    fn a_host_that_reads_as_an_option_is_refused() {
        for host in ["-oProxyCommand=sh", ""] {
            let result = spawn_ssh(host, "agents-cockpit");

            assert!(
                matches!(&result, Err(ConnectError::InvalidHost { host: refused }) if refused == host),
                "{host}"
            );
        }
    }
}

//! Runs `contrib/update-daemon.sh` on a server, over plain SSH: a daemon of an older protocol
//! refuses the app's own connection.

use std::error::Error;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use cockpit_client::ssh_script_command;
use cockpit_protocol::BuildCommit;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::server_daemon::SentDaemon;
use crate::state::daemon_update::{OutputLine, OutputStream};
use crate::state::error_chain;
use crate::state::update_plan::{BusyAgents, ServerPlan};

const SCRIPT: &str = include_str!("../../../contrib/update-daemon.sh");

const CANCEL_OPTION: &str = "--cancel";

// The options both scripts take for a daemon the app sent: its file, its checksum, and the
// version it must print.
pub const BUILT_OPTION: &str = "--built";
pub const SHA256_OPTION: &str = "--sha256";
pub const VERSION_OPTION: &str = "--version";

/// Makes `contrib/install-daemon.sh` only say what the daemon needs.
pub const CHECK_OPTION: &str = "--check";

/// Makes `contrib/install-daemon.sh` add the daemon's start at boot to the user's crontab.
pub const BOOT_START_OPTION: &str = "--add-boot-start";

/// Makes `contrib/install-daemon.sh` start the daemon installed at the path after it.
pub const START_OPTION: &str = "--start";

/// Skips the script's refusal for busy agents: "Update now".
const EVEN_IF_BUSY_OPTION: &str = "--even-if-busy";

/// How long the output waits before the next lines show, so that a build renders the window a few
/// times a second at most.
const OUTPUT_PAUSE: Duration = Duration::from_millis(150);

/// The most lines shown at once.
const OUTPUT_BATCH: usize = 256;

/// The longest line kept, in bytes: the rest of a longer line is dropped, so that a server that
/// never ends its line cannot fill the app's memory.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// The release this app was built from. The app builds the daemon from its own tree, so the daemon
/// in the app tells the same release.
pub const APP_VERSION: &str = cockpit_daemon::RELEASE_VERSION;

/// The commit this app was built from, when its build knew it. The app builds the daemon from its
/// own tree, so the daemon in the app tells the same commit.
pub fn app_commit() -> Option<BuildCommit> {
    cockpit_daemon::build_commit()
}

/// Runs the update on its server, streaming the lines to `on_lines`, and returns the script's exit
/// code: 255 or `None` when ssh lost the connection or could not start. With `sent`, the script
/// installs that daemon in place of a build.
pub async fn update(
    plan: &ServerPlan,
    busy: BusyAgents,
    sent: Option<&SentDaemon>,
    mut on_lines: impl FnMut(Vec<OutputLine>),
) -> Option<i32> {
    let version = cockpit_daemon::version_text(APP_VERSION, Some(&plan.commit));
    let arguments = update_arguments(plan, busy, sent, &version);
    let command = match ssh_script_command(&plan.host, &arguments) {
        Ok(command) => command,
        Err(error) => {
            on_lines(vec![error_line(&error)]);
            return None;
        }
    };
    match run(command, SCRIPT, &mut on_lines).await {
        Ok(status) => status.code(),
        Err(error) => {
            on_lines(vec![error_line(&error)]);
            None
        }
    }
}

/// Asks the server's running update to stop before it installs. `Err` says why that failed.
pub async fn cancel(plan: &ServerPlan) -> Result<(), String> {
    let command =
        ssh_script_command(&plan.host, &[CANCEL_OPTION]).map_err(|error| error.to_string())?;
    let mut lines = Vec::new();
    let status = run(command, SCRIPT, &mut |batch| lines.extend(batch))
        .await
        .map_err(|error| error.to_string())?;
    if status.success() {
        return Ok(());
    }
    let said: Vec<String> = lines.into_iter().map(|line| line.text).collect();
    Err(said.join(" "))
}

/// `error` as a line of the script's stderr.
pub fn error_line(error: &dyn Error) -> OutputLine {
    OutputLine {
        stream: OutputStream::Stderr,
        text: error_chain(error),
    }
}

/// The script's arguments. `version` is what the sent daemon must print after its name.
fn update_arguments<'a>(
    plan: &'a ServerPlan,
    busy: BusyAgents,
    sent: Option<&'a SentDaemon>,
    version: &'a str,
) -> Vec<&'a str> {
    let mut arguments = vec![
        "--commit",
        plan.commit.as_str(),
        "--binary",
        plan.remote_command.as_str(),
    ];
    match sent {
        Some(sent) => arguments.extend([
            BUILT_OPTION,
            sent.remote_file.as_str(),
            SHA256_OPTION,
            sent.sha256.as_str(),
            VERSION_OPTION,
            version,
        ]),
        None => {
            if let Some(source_dir) = &plan.source_dir {
                arguments.extend(["--source-dir", source_dir.as_str()]);
            }
        }
    }
    match busy {
        BusyAgents::Refuse => {}
        BusyAgents::Interrupt => arguments.push(EVEN_IF_BUSY_OPTION),
    }
    arguments
}

/// Runs `command` with `script` on its stdin, and gives each batch of its output to `on_lines`.
pub async fn run(
    mut command: Command,
    script: &'static str,
    on_lines: &mut impl FnMut(Vec<OutputLine>),
) -> io::Result<ExitStatus> {
    let mut child = command.spawn()?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(io::Error::other("ssh started without its pipes"));
    };
    let (sender, mut lines) = mpsc::unbounded_channel();
    tokio::spawn(read_lines(stdout, OutputStream::Stdout, sender.clone()));
    tokio::spawn(read_lines(stderr, OutputStream::Stderr, sender));
    // Closing stdin ends the script for the remote shell. ssh can end first, as when the host
    // refuses it: its exit status then says so.
    tokio::spawn(async move {
        let _ = stdin.write_all(script.as_bytes()).await;
    });
    let mut batch = Vec::new();
    while lines.recv_many(&mut batch, OUTPUT_BATCH).await > 0 {
        on_lines(std::mem::take(&mut batch));
        tokio::time::sleep(OUTPUT_PAUSE).await;
    }
    child.wait().await
}

/// Sends each line of `stream` to `sender`, until `stream` ends. A line keeps its first
/// `MAX_LINE_BYTES` bytes.
async fn read_lines(
    stream: impl AsyncRead + Unpin,
    output: OutputStream,
    sender: mpsc::UnboundedSender<OutputLine>,
) {
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    loop {
        let Ok(chunk) = reader.fill_buf().await else {
            return;
        };
        if chunk.is_empty() {
            if !bytes.is_empty() {
                let _ = sender.send(output_line(output, &bytes));
            }
            return;
        }
        let newline = chunk.iter().position(|&byte| byte == b'\n');
        let part = newline.map_or(chunk, |at| chunk.get(..at).unwrap_or(chunk));
        let room = MAX_LINE_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(part.get(..room).unwrap_or(part));
        let consumed = newline.map_or(chunk.len(), |at| at + 1);
        reader.consume(consumed);
        if newline.is_some() {
            if sender.send(output_line(output, &bytes)).is_err() {
                return;
            }
            bytes.clear();
        }
    }
}

fn output_line(stream: OutputStream, bytes: &[u8]) -> OutputLine {
    OutputLine {
        stream,
        text: String::from_utf8_lossy(bytes).trim_end().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::update_plan::{
        BUILDING_LINE, CHECKING_LINE, CarriedDaemons, DAEMON_RUNS_LINE, RESTARTING_LINE,
    };

    fn plan(source_dir: Option<&str>) -> ServerPlan {
        ServerPlan {
            host: "build-box".to_owned(),
            remote_command: "/opt/yetack".to_owned(),
            source_dir: source_dir.map(str::to_owned),
            commit: BuildCommit::new("3f9c2e1".to_owned()),
            carried: CarriedDaemons::default(),
            daemon: None,
        }
    }

    #[test]
    fn the_steps_follow_the_lines_the_embedded_script_prints() {
        let prefix = "update-daemon: ";
        assert!(SCRIPT.contains(&format!("printf '{prefix}%s\\n'")));
        for line in [
            BUILDING_LINE,
            CHECKING_LINE,
            RESTARTING_LINE,
            DAEMON_RUNS_LINE,
        ] {
            let said = line.strip_prefix(prefix).unwrap();
            assert!(SCRIPT.contains(&format!("say \"{said}")), "{line}");
        }
    }

    #[test]
    fn the_embedded_script_has_no_carriage_return() {
        assert!(!SCRIPT.contains('\r'));
    }

    #[test]
    fn the_arguments_name_the_commit_the_binary_and_the_source_dir() {
        let (without_a_folder, with_a_folder) = (plan(None), plan(Some("/srv/cockpit")));

        assert_eq!(
            update_arguments(&without_a_folder, BusyAgents::Refuse, None, ""),
            ["--commit", "3f9c2e1", "--binary", "/opt/yetack"]
        );
        assert_eq!(
            update_arguments(&with_a_folder, BusyAgents::Refuse, None, ""),
            [
                "--commit",
                "3f9c2e1",
                "--binary",
                "/opt/yetack",
                "--source-dir",
                "/srv/cockpit"
            ]
        );
    }

    #[test]
    fn update_now_runs_the_script_even_if_agents_are_busy() {
        assert_eq!(
            update_arguments(&plan(None), BusyAgents::Interrupt, None, ""),
            [
                "--commit",
                "3f9c2e1",
                "--binary",
                "/opt/yetack",
                "--even-if-busy"
            ]
        );
    }

    #[test]
    fn a_sent_daemon_replaces_the_build_with_its_file_checksum_and_version() {
        let sent = SentDaemon {
            remote_file: "/home/someone/.cache/yetack/sent/yetack.a1b2c3".to_owned(),
            sha256: "ba7816bf".to_owned(),
        };

        assert_eq!(
            update_arguments(
                &plan(Some("/srv/cockpit")),
                BusyAgents::Refuse,
                Some(&sent),
                "0.2.0 (3f9c2e1)"
            ),
            [
                "--commit",
                "3f9c2e1",
                "--binary",
                "/opt/yetack",
                "--built",
                "/home/someone/.cache/yetack/sent/yetack.a1b2c3",
                "--sha256",
                "ba7816bf",
                "--version",
                "0.2.0 (3f9c2e1)"
            ]
        );
        for option in [BUILT_OPTION, SHA256_OPTION, VERSION_OPTION] {
            assert!(
                SCRIPT.contains(&format!("{option}) ")),
                "the script parses {option}"
            );
        }
    }

    #[test]
    fn the_embedded_script_takes_the_option_that_skips_busy_agents() {
        assert!(
            SCRIPT.contains(&format!("[ \"$1\" = {EVEN_IF_BUSY_OPTION} ]")),
            "the script parses it"
        );
    }

    #[cfg(unix)]
    fn script_runner(script: &str) -> Command {
        use std::process::Stdio;

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!("cat >/dev/null; {script}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_output_of_both_streams_comes_back_with_the_exit_code() {
        let command = script_runner("echo out; echo err >&2; printf 'no newline'; exit 3");
        let mut lines = Vec::new();

        let status = run(command, SCRIPT, &mut |batch| lines.extend(batch))
            .await
            .unwrap();

        assert_eq!(status.code(), Some(3));
        let text = |stream| -> Vec<&str> {
            lines
                .iter()
                .filter(|line: &&OutputLine| line.stream == stream)
                .map(|line| line.text.as_str())
                .collect()
        };
        assert_eq!(text(OutputStream::Stdout), ["out", "no newline"]);
        assert_eq!(text(OutputStream::Stderr), ["err"]);
    }

    #[tokio::test]
    async fn a_long_line_keeps_its_start_and_the_next_line_comes_whole() {
        let long = "a".repeat(3 * MAX_LINE_BYTES);
        let output = format!("{long}\nnext\n{long}");
        let (sender, mut received) = mpsc::unbounded_channel();

        read_lines(output.as_bytes(), OutputStream::Stdout, sender).await;

        let mut lines = Vec::new();
        received.recv_many(&mut lines, 10).await;
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        let kept = "a".repeat(MAX_LINE_BYTES);
        assert_eq!(texts, [kept.as_str(), "next", kept.as_str()]);
    }
}

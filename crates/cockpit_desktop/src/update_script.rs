//! Runs `contrib/update-daemon.sh` on a server, over plain SSH: a daemon of an older protocol
//! refuses the app's own connection.

use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use cockpit_client::ssh_script_command;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::state::daemon_update::{OutputLine, OutputStream, UpdatePlan};
use crate::state::error_chain;

const SCRIPT: &str = include_str!("../../../contrib/update-daemon.sh");

const CANCEL_OPTION: &str = "--cancel";

/// How long the output waits before the next lines show, so that a build renders the window a few
/// times a second at most.
const OUTPUT_PAUSE: Duration = Duration::from_millis(150);

/// The most lines shown at once.
const OUTPUT_BATCH: usize = 256;

/// The commit this app was built from, when its build knew it. A Nix build of a tree with
/// uncommitted changes sets it empty.
pub fn app_commit() -> Option<&'static str> {
    known_commit(option_env!("AGENTS_COCKPIT_COMMIT"))
}

fn known_commit(built_with: Option<&'static str>) -> Option<&'static str> {
    built_with.filter(|commit| !commit.is_empty())
}

/// Runs the update on its server, streaming the lines to `on_lines`, and returns the script's exit
/// code: 255 or `None` when ssh lost the connection or could not start.
pub async fn update(plan: &UpdatePlan, mut on_lines: impl FnMut(Vec<OutputLine>)) -> Option<i32> {
    let command = match ssh_script_command(&plan.host, &update_arguments(plan)) {
        Ok(command) => command,
        Err(error) => {
            on_lines(vec![error_line(&error)]);
            return None;
        }
    };
    match run(command, &mut on_lines).await {
        Ok(status) => status.code(),
        Err(error) => {
            on_lines(vec![error_line(&error)]);
            None
        }
    }
}

/// Asks the server's running update to stop before it installs. `Err` says why that failed.
pub async fn cancel(plan: &UpdatePlan) -> Result<(), String> {
    let command =
        ssh_script_command(&plan.host, &[CANCEL_OPTION]).map_err(|error| error.to_string())?;
    let mut lines = Vec::new();
    let status = run(command, &mut |batch| lines.extend(batch))
        .await
        .map_err(|error| error.to_string())?;
    if status.success() {
        return Ok(());
    }
    let said: Vec<String> = lines.into_iter().map(|line| line.text).collect();
    Err(said.join(" "))
}

fn error_line(error: &dyn std::error::Error) -> OutputLine {
    OutputLine {
        stream: OutputStream::Stderr,
        text: error_chain(error),
    }
}

fn update_arguments(plan: &UpdatePlan) -> Vec<&str> {
    let mut arguments = vec![
        "--commit",
        plan.commit.as_str(),
        "--binary",
        plan.remote_command.as_str(),
    ];
    if let Some(source_dir) = &plan.source_dir {
        arguments.extend(["--source-dir", source_dir.as_str()]);
    }
    arguments
}

/// Runs `command` with the script on its stdin, and gives each batch of its output to `on_lines`.
async fn run(
    mut command: Command,
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
        let _ = stdin.write_all(SCRIPT.as_bytes()).await;
    });
    let mut batch = Vec::new();
    while lines.recv_many(&mut batch, OUTPUT_BATCH).await > 0 {
        on_lines(std::mem::take(&mut batch));
        tokio::time::sleep(OUTPUT_PAUSE).await;
    }
    child.wait().await
}

/// Sends each line of `stream` to `sender`, until `stream` ends.
async fn read_lines(
    stream: impl AsyncRead + Unpin,
    output: OutputStream,
    sender: mpsc::UnboundedSender<OutputLine>,
) {
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let line = OutputLine {
                    stream: output,
                    text: String::from_utf8_lossy(&bytes).trim_end().to_owned(),
                };
                if sender.send(line).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::daemon_update::{BUILDING_LINE, DAEMON_RUNS_LINE, RESTARTING_LINE};

    fn plan(source_dir: Option<&str>) -> UpdatePlan {
        UpdatePlan {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: source_dir.map(str::to_owned),
            commit: "3f9c2e1".to_owned(),
        }
    }

    #[test]
    fn an_empty_commit_is_no_commit() {
        assert_eq!(known_commit(None), None);
        assert_eq!(known_commit(Some("")), None);
        assert_eq!(known_commit(Some("3f9c2e1")), Some("3f9c2e1"));
    }

    #[test]
    fn the_steps_follow_the_lines_the_embedded_script_prints() {
        let prefix = "update-daemon: ";
        assert!(SCRIPT.contains(&format!("printf '{prefix}%s\\n'")));
        for line in [BUILDING_LINE, RESTARTING_LINE, DAEMON_RUNS_LINE] {
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
            update_arguments(&without_a_folder),
            ["--commit", "3f9c2e1", "--binary", "/opt/agents-cockpit"]
        );
        assert_eq!(
            update_arguments(&with_a_folder),
            [
                "--commit",
                "3f9c2e1",
                "--binary",
                "/opt/agents-cockpit",
                "--source-dir",
                "/srv/cockpit"
            ]
        );
    }

    #[cfg(unix)]
    fn script_runner(script: &str) -> Command {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!("cat >/dev/null; {script}"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_output_of_both_streams_comes_back_with_the_exit_code() {
        let command = script_runner("echo out; echo err >&2; printf 'no newline'; exit 3");
        let mut lines = Vec::new();

        let status = run(command, &mut |batch| lines.extend(batch))
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
}

//! The command line against a real daemon. A shell script that echoes its stdin stands in for
//! `claude`: every prompt comes back as a line, and no turn ever ends.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure is a failed test"
)]

use std::fs::{File, Permissions};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);
const BINARY: &str = env!("CARGO_BIN_EXE_agents-cockpit");

struct Daemon {
    temp: TempDir,
    serve: Child,
}

impl Daemon {
    fn start() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join("echo-claude");
        std::fs::write(&claude, "#!/bin/sh\nexec cat\n").unwrap();
        std::fs::set_permissions(&claude, Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(temp.path().join("work")).unwrap();
        let log = File::create(temp.path().join("serve.log")).unwrap();
        let serve = Command::new(BINARY)
            .args(["serve", "--state-dir"])
            .arg(temp.path().join("state"))
            .env(cockpit_daemon::CLAUDE_PROGRAM_ENV, &claude)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let daemon = Self { temp, serve };
        let socket = cockpit_daemon::socket_path(&daemon.state());
        let deadline = Instant::now() + WAIT;
        while UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "the daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }

    fn work(&self) -> PathBuf {
        self.temp.path().join("work")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BINARY);
        command.arg("--state-dir").arg(self.state()).args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// Runs a command that must succeed, and returns its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn wait_for_list(&self, expected: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let list = self.ok(&["list"]);
            if list.contains(expected) {
                return list;
            }
            assert!(
                Instant::now() < deadline,
                "the list never showed {expected}: {list}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.serve.kill();
        let _ = self.serve.wait();
    }
}

/// The lines `child` writes on stdout, as they come.
fn lines_of(child: &mut Child) -> Receiver<String> {
    use std::io::BufRead;

    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    receiver
}

/// The first line that contains `expected`, if one comes within `wait`.
fn line_with(lines: &Receiver<String>, expected: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let line = lines.recv_timeout(left).ok()?;
        if line.contains(expected) {
            return Some(line);
        }
    }
}

fn folder_arg(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn starts_lists_prompts_tails_renames_and_stops_an_agent() {
    use std::io::Write;

    let daemon = Daemon::start();
    let empty = daemon.ok(&["list"]);
    assert_eq!(empty.lines().count(), 1, "only the header: {empty}");

    let work = daemon.work();
    let id = daemon.ok(&["start", folder_arg(&work), "Fix the parser"]);
    assert_eq!(id.trim(), "1");
    let list = daemon.wait_for_list("Fix the parser");
    assert!(list.lines().nth(1).unwrap().starts_with("1 "), "{list}");

    let mut prompt = daemon
        .command(&["prompt", "1", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    prompt
        .stdin
        .take()
        .unwrap()
        .write_all(b"second prompt from stdin")
        .unwrap();
    assert!(prompt.wait().unwrap().success());
    let mut tail = daemon
        .command(&["tail", "1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lines = lines_of(&mut tail);
    // The tail shows live events only: prompt until one shows, which proves it connected.
    let deadline = Instant::now() + WAIT;
    let line = loop {
        daemon.ok(&["prompt", "1", "third prompt"]);
        if let Some(line) = line_with(&lines, "third prompt", Duration::from_millis(200)) {
            break line;
        }
        assert!(Instant::now() < deadline, "the tail showed nothing");
    };
    let event: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(event["event"]["agent"], 1);
    let _ = tail.kill();
    let _ = tail.wait();

    daemon.ok(&["rename", "1", "Parser fix"]);
    daemon.wait_for_list("Parser fix");

    daemon.ok(&["stop", "1"]);
    daemon.wait_for_list("exited");
}

/// As in `agents-cockpit tail | head -n 1`.
#[test]
fn tail_ends_when_its_output_closes() {
    let daemon = Daemon::start();
    let work = daemon.work();
    daemon.ok(&["start", folder_arg(&work), "first prompt"]);
    let mut tail = daemon
        .command(&["tail"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lines = lines_of(&mut tail);
    let deadline = Instant::now() + WAIT;
    while line_with(&lines, "prompt", Duration::from_millis(200)).is_none() {
        daemon.ok(&["prompt", "1", "a prompt"]);
        assert!(Instant::now() < deadline, "the tail showed nothing");
    }

    // The reading thread closes the pipe once it fails to pass on its next line.
    drop(lines);
    let deadline = Instant::now() + WAIT;
    while tail.try_wait().unwrap().is_none() {
        daemon.ok(&["prompt", "1", "another prompt"]);
        if Instant::now() > deadline {
            let _ = tail.kill();
            panic!("the tail still runs with its output closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A title can come from a session file any process of the user could write.
#[test]
fn list_prints_no_control_character() {
    let daemon = Daemon::start();
    let work = daemon.work();
    daemon.ok(&[
        "start",
        folder_arg(&work),
        "--title",
        "Fix\u{1b}]0;owned\u{7} it",
    ]);

    let list = daemon.wait_for_list("Fix");

    assert!(
        !list.chars().any(|c| c.is_control() && c != '\n'),
        "{list:?}"
    );
    assert!(list.contains("Fix"), "{list}");
}

#[test]
fn errors_go_to_stderr_with_a_failure_status() {
    let daemon = Daemon::start();

    let output = daemon.run(&["prompt", "99", "hello"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no agent 99"), "{stderr}");
}

#[test]
fn the_bridge_carries_the_protocol() {
    use std::io::Write;

    let daemon = Daemon::start();
    let mut bridge = daemon
        .command(&[cockpit_client::BRIDGE_SUBCOMMAND])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    bridge
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":null}\n")
        .unwrap();
    let lines = lines_of(&mut bridge);
    let welcome = line_with(&lines, "welcome", WAIT).expect("a welcome");

    assert!(welcome.contains("\"catchup\""), "{welcome}");
    drop(bridge.stdin.take());
    assert!(bridge.wait().unwrap().success());
}

#[test]
fn the_bridge_says_when_no_daemon_runs() {
    let temp = tempfile::tempdir().unwrap();

    let output = Command::new(BINARY)
        .arg("--state-dir")
        .arg(temp.path())
        .arg(cockpit_client::BRIDGE_SUBCOMMAND)
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is `agents-cockpit serve` running?"),
        "{stderr}"
    );
}

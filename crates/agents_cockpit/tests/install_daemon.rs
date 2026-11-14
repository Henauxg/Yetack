//! `contrib/install-daemon.sh` against a real daemon in a home folder of its own. Fake
//! `systemctl` and `loginctl` stand in for systemd.

#![cfg(unix)]
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure is a failed test"
)]

use std::fs::{self, File, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_agents-cockpit");
const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contrib/install-daemon.sh"
);

/// A home folder of its own, where the fake `systemctl` starts the installed daemon. Every command
/// it runs sees only this folder.
struct Server {
    temp: TempDir,
}

impl Server {
    fn new() -> Self {
        let server = Self {
            temp: tempfile::Builder::new().prefix(".tmp").tempdir().unwrap(),
        };
        fs::create_dir_all(server.path("bin")).unwrap();
        fs::create_dir_all(server.path("home")).unwrap();
        server.write_program("echo-claude", "#!/bin/sh\nexec cat\n");
        server.write_program(
            "bin/systemctl",
            &format!(
                r#"#!/bin/sh
echo "$*" >>'{log}'
case "$*" in
    "--user show-environment") [ ! -f '{no_session}' ] ;;
    "--user enable --now agents-cockpit")
        "$HOME/.local/share/agents-cockpit/bin/agents-cockpit" serve >>'{serve_log}' 2>&1 </dev/null &
        echo $! >'{pid}'
        ;;
esac
"#,
                log = server.path("systemctl.log").display(),
                no_session = server.path("no-session").display(),
                serve_log = server.path("serve.log").display(),
                pid = server.path("daemon.pid").display(),
            ),
        );
        server.write_program("bin/loginctl", "#!/bin/sh\necho no\n");
        server
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    fn write_program(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
    }

    fn command(&self, program: &str) -> Command {
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = Command::new(program);
        command
            .current_dir(self.temp.path())
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("home/state"))
            .env("XDG_CONFIG_HOME", self.path("home/config"))
            .env("XDG_CACHE_HOME", self.path("home/cache"))
            .env(cockpit_daemon::CLAUDE_PROGRAM_ENV, self.path("echo-claude"));
        command
    }

    /// A copy of the test's binary, as the app sends it, with what its `--version` prints after
    /// the program's name.
    fn sent_binary(&self) -> (PathBuf, String) {
        let sent = self.path("home/cache/agents-cockpit/sent/agents-cockpit.a1b2c3");
        fs::create_dir_all(sent.parent().unwrap()).unwrap();
        fs::copy(BINARY, &sent).unwrap();
        let said = Command::new(BINARY).arg("--version").output().unwrap();
        let version = String::from_utf8(said.stdout).unwrap();
        let version = version
            .trim()
            .strip_prefix("agents-cockpit ")
            .unwrap()
            .to_owned();
        (sent, version)
    }

    fn install(&self, arguments: &[&str]) -> Output {
        self.command("sh")
            .args(["-s", "--"])
            .args(arguments)
            .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
            .output()
            .unwrap()
    }

    fn installed(&self) -> PathBuf {
        self.path("home/.local/share/agents-cockpit/bin/agents-cockpit")
    }

    fn unit(&self) -> PathBuf {
        self.path("home/config/systemd/user/agents-cockpit.service")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(self.path("daemon.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
    }
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum").arg(path).output().unwrap();
    let line = String::from_utf8(output.stdout).unwrap();
    line.split_whitespace().next().unwrap().to_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_sent_binary_becomes_a_running_service_and_the_report_says_what_it_needs() {
    let server = Server::new();
    let (sent, version) = server.sent_binary();
    let claude = server.path("home/.local/bin/claude");
    server.write_program("home/.local/bin/claude", "#!/bin/sh\n");

    let output = server.install(&[
        "--built",
        sent.to_str().unwrap(),
        "--sha256",
        &sha256(&sent),
        "--version",
        &version,
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    let said = stdout(&output);
    assert!(said.contains(&format!(
        "install-daemon: installed {}",
        server.installed().display()
    )));
    assert!(said.contains("install-daemon: the daemon runs"));
    assert!(said.contains("install-daemon: linger no"));
    assert!(said.contains(&format!("install-daemon: claude {}", claude.display())));
    assert_eq!(
        fs::read(server.installed()).unwrap(),
        fs::read(BINARY).unwrap()
    );
    let unit = fs::read_to_string(server.unit()).unwrap();
    assert!(unit.contains("ExecStart=%h/.local/share/agents-cockpit/bin/agents-cockpit serve"));
    let folder = server.path("home/.local/share/agents-cockpit");
    assert_eq!(
        fs::metadata(folder).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!sent.exists(), "the sent file is deleted");
    let listed = server
        .command(server.installed().to_str().unwrap())
        .arg("list")
        .output()
        .unwrap();
    assert!(listed.status.success(), "{}", stderr(&listed));
}

#[test]
fn a_sent_binary_that_is_not_the_one_the_app_read_installs_nothing() {
    let server = Server::new();
    let (sent, version) = server.sent_binary();

    let output = server.install(&[
        "--built",
        sent.to_str().unwrap(),
        "--sha256",
        &"0".repeat(64),
        "--version",
        &version,
    ]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("its checksum differs"));
    assert!(!server.installed().exists());
    assert!(!server.unit().exists());
    assert!(!sent.exists());
}

#[test]
fn without_a_systemd_user_session_it_installs_nothing_and_says_what_to_do() {
    let server = Server::new();
    fs::write(server.path("no-session"), "").unwrap();
    let (sent, version) = server.sent_binary();

    let output = server.install(&[
        "--built",
        sent.to_str().unwrap(),
        "--sha256",
        &sha256(&sent),
        "--version",
        &version,
    ]);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("contrib/start-daemon"));
    assert!(!server.installed().exists());
}

#[test]
fn a_check_only_reports_what_the_daemon_needs() {
    let server = Server::new();

    let output = server.install(&["--check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "install-daemon: linger no\ninstall-daemon: claude missing\n"
    );
}

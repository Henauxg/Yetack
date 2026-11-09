//! `contrib/update-daemon.sh` against a real daemon in a folder of its own. Fake `cargo` and
//! `systemctl` stand in for the build and the service manager.

#![cfg(unix)]
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure is a failed test"
)]

use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);
const BINARY: &str = env!("CARGO_BIN_EXE_agents-cockpit");
const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contrib/update-daemon.sh"
);
/// What the fake `cargo` appends to the binary it builds, which still runs.
const BUILT_MARK: &[u8] = b"built by the fake cargo";

/// A home folder of its own, with a repository and an installed binary. Every command it runs
/// sees only this folder, so it cannot reach another daemon or service.
struct Machine {
    temp: TempDir,
}

impl Machine {
    fn new() -> Self {
        let machine = Self {
            temp: tempfile::tempdir().unwrap(),
        };
        for folder in ["bin", "install", "work", "home/config/agents-cockpit"] {
            fs::create_dir_all(machine.path(folder)).unwrap();
        }
        machine.write_program("echo-claude", "#!/bin/sh\nexec cat\n");
        machine.write_program(
            "bin/cargo",
            &format!(
                r#"#!/bin/sh
[ "$#" -gt 0 ] || exit 0
echo "$*" >>'{log}'
[ ! -f '{fails}' ] || exit 1
while [ "$#" -gt 0 ] && [ "$1" != --target-dir ]; do shift; done
[ "$#" -ge 2 ] || exit 1
mkdir -p "$2/release"
cp '{BINARY}' "$2/release/agents-cockpit"
printf '%s' '{mark}' >>"$2/release/agents-cockpit"
"#,
                log = machine.path("cargo.log").display(),
                fails = machine.path("cargo-fails").display(),
                mark = String::from_utf8_lossy(BUILT_MARK),
            ),
        );
        machine.write_program(
            "bin/systemctl",
            &format!(
                "#!/bin/sh\n[ \"$#\" -gt 0 ] || exit 0\necho \"$*\" >>'{log}'\n[ -f '{active}' ]\n",
                log = machine.path("systemctl.log").display(),
                active = machine.path("service-active").display(),
            ),
        );
        fs::copy(BINARY, machine.installed()).unwrap();
        wait_until_it_runs(&machine.installed());
        fs::write(
            machine.start_file(),
            format!(
                "'{installed}' serve >>'{log}' 2>&1 </dev/null &\necho $! >'{pid}'\n",
                installed = machine.installed().display(),
                log = machine.path("serve.log").display(),
                pid = machine.path("daemon.pid").display(),
            ),
        )
        .unwrap();
        machine.git(
            Path::new("."),
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        machine.git(
            Path::new("."),
            &["clone", "--quiet", "origin.git", "author"],
        );
        machine.commit_on_origin("First");
        machine.git(
            Path::new("."),
            &["clone", "--quiet", "origin.git", "source"],
        );
        machine
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join(relative)
    }

    fn installed(&self) -> PathBuf {
        self.path("install/agents-cockpit")
    }

    fn start_file(&self) -> PathBuf {
        self.path("home/config/agents-cockpit/start-daemon")
    }

    fn state(&self) -> PathBuf {
        self.path("home/state/agents-cockpit")
    }

    fn write_program(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        wait_until_it_runs(&path);
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
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
            .env(cockpit_daemon::CLAUDE_PROGRAM_ENV, self.path("echo-claude"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Someone")
            .env("GIT_AUTHOR_EMAIL", "someone@example.test")
            .env("GIT_COMMITTER_NAME", "Someone")
            .env("GIT_COMMITTER_EMAIL", "someone@example.test");
        command
    }

    fn git(&self, folder: &Path, args: &[&str]) -> String {
        let output = self
            .command("git")
            .current_dir(self.temp.path().join(folder))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// Commits on the repository's `origin` and returns the commit's hash.
    fn commit_on_origin(&self, message: &str) -> String {
        let author = Path::new("author");
        self.git(
            author,
            &["commit", "--quiet", "--allow-empty", "-m", message],
        );
        self.git(author, &["push", "--quiet", "origin", "HEAD:main"]);
        self.git(author, &["rev-parse", "HEAD"])
    }

    /// Starts the daemon the way the script does, so that it is nobody's child to reap.
    fn start_daemon(&self) {
        let started = self.command("sh").arg(self.start_file()).status().unwrap();
        assert!(started.success());
        let socket = cockpit_daemon::socket_path(&self.state());
        let deadline = Instant::now() + WAIT;
        while UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "the daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn daemon_pid(&self) -> String {
        fs::read_to_string(self.path("daemon.pid"))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn cli(&self, args: &[&str]) -> Output {
        self.command(self.installed()).args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Starts an agent with no first prompt, and waits for it to be idle.
    fn idle_agent(&self) -> String {
        let work = self.path("work");
        let id = self
            .ok(&["start", work.to_str().unwrap()])
            .trim()
            .to_owned();
        self.wait_for_status(&id, "idle");
        id
    }

    fn wait_for_status(&self, id: &str, status: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let list = self.ok(&["list"]);
            let found = list.lines().skip(1).any(|row| {
                let mut columns = row.split_whitespace();
                columns.next() == Some(id) && columns.next() == Some(status)
            });
            if found {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "agent {id} never was {status}: {list}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Adds a tool call to an agent's raw log, in the form the daemon writes it.
    fn log_tool_call(&self, agent: &str, tool: &str, at: u128) {
        let claude_line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_made_up","name":"{tool}","input":{{}}}}]}}}}"#
        );
        let line = format!(
            "{{\"at\":{at},\"direction\":\"from_claude\",\"line\":{}}}\n",
            serde_json::to_string(&claude_line).unwrap()
        );
        OpenOptions::new()
            .append(true)
            .open(self.state().join("raw").join(format!("{agent}.jsonl")))
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }

    /// Adds usage figures that name a tool to an agent's raw log, in the form the daemon writes.
    fn log_usage_figures(&self, agent: &str, tool: &str) {
        let claude_line = format!(
            r#"{{"type":"control_response","response":{{"toolCallsByType":[{{"name":"{tool}","callTokens":1}}]}}}}"#
        );
        let line = format!(
            "{{\"at\":{},\"direction\":\"from_claude\",\"line\":{}}}\n",
            now_ms(),
            serde_json::to_string(&claude_line).unwrap()
        );
        OpenOptions::new()
            .append(true)
            .open(self.state().join("raw").join(format!("{agent}.jsonl")))
            .unwrap()
            .write_all(line.as_bytes())
            .unwrap();
    }

    /// Runs the script as the app does: read from stdin by the remote shell.
    fn update(&self, commit: &str) -> Output {
        self.command("sh")
            .args(["-s", "--", "--commit", commit, "--binary"])
            .arg(self.installed())
            .arg("--source-dir")
            .arg(self.path("source"))
            .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
            .output()
            .unwrap()
    }

    fn installed_is_the_new_build(&self) -> bool {
        fs::read(self.installed()).unwrap().ends_with(BUILT_MARK)
    }

    fn built(&self) -> bool {
        self.path("cargo.log").exists()
    }

    fn update_lock_is_free(&self) -> bool {
        !self.path("home/cache/agents-cockpit/update/lock").exists()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let Ok(pid) = fs::read_to_string(self.path("daemon.pid")) else {
            return;
        };
        let pid = pid.trim();
        let _ = Command::new("kill").arg(pid).status();
        let deadline = Instant::now() + WAIT;
        while Path::new("/proc").join(pid).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A process another test forks while a program is written keeps it open for writing until it
/// runs its own program, and running the program fails with "Text file busy" until then.
fn wait_until_it_runs(program: &Path) {
    for _ in 0..200 {
        let run = Command::new(program)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match run {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) | Err(_) => return,
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_succeeded(output: &Output) {
    assert!(
        output.status.success(),
        "the update failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(output)
    );
}

/// Asserts that a refused update changed nothing and said `expected`.
fn assert_refused(machine: &Machine, output: &Output, expected: &str, daemon_before: &str) {
    assert!(!output.status.success(), "the update was not refused");
    assert!(stderr(output).contains(expected), "{}", stderr(output));
    assert!(!machine.installed_is_the_new_build());
    assert_eq!(machine.daemon_pid(), daemon_before);
    assert!(machine.update_lock_is_free());
    machine.ok(&["list"]);
}

#[test]
fn a_commit_replaces_the_binary_restarts_the_daemon_and_leaves_the_checkout() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    let before = machine.daemon_pid();
    let checkout_before = machine.git(Path::new("source"), &["rev-parse", "HEAD"]);
    let commit = machine.commit_on_origin("Second");

    let output = machine.update(&commit);

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert_ne!(machine.daemon_pid(), before);
    assert!(
        machine
            .ok(&["list"])
            .lines()
            .any(|row| row.starts_with(&id))
    );
    assert_eq!(
        machine.git(Path::new("source"), &["rev-parse", "HEAD"]),
        checkout_before
    );
    assert_eq!(
        machine.git(
            Path::new("home/cache/agents-cockpit/update/source"),
            &["rev-parse", "HEAD"]
        ),
        commit
    );
    assert!(
        !fs::read(format!("{}.previous", machine.installed().display()))
            .unwrap()
            .ends_with(BUILT_MARK)
    );
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_history_rewritten_on_origin_does_not_stop_the_update() {
    let machine = Machine::new();
    machine.start_daemon();
    let author = Path::new("author");
    machine.git(
        author,
        &[
            "commit",
            "--quiet",
            "--amend",
            "--allow-empty",
            "-m",
            "Rewritten",
        ],
    );
    machine.git(
        author,
        &["push", "--quiet", "--force", "origin", "HEAD:main"],
    );
    let commit = machine.git(author, &["rev-parse", "HEAD"]);

    let output = machine.update(&commit);

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
}

#[test]
fn the_daemon_of_another_state_folder_keeps_running() {
    let machine = Machine::new();
    let neighbour = Machine::new();
    machine.start_daemon();
    neighbour.start_daemon();
    let neighbour_pid = neighbour.daemon_pid();

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert_eq!(neighbour.daemon_pid(), neighbour_pid);
    assert!(Path::new("/proc").join(&neighbour_pid).exists());
    neighbour.ok(&["list"]);
}

#[test]
fn with_no_daemon_running_it_installs_and_starts_one() {
    let machine = Machine::new();
    fs::remove_file(machine.installed()).unwrap();

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    machine.ok(&["list"]);
}

#[test]
fn refuses_while_an_agent_works() {
    let machine = Machine::new();
    machine.start_daemon();
    let work = machine.path("work");
    let id = machine
        .ok(&["start", work.to_str().unwrap(), "Fix the parser"])
        .trim()
        .to_owned();
    machine.wait_for_status(&id, "working");
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, "Fix the parser", &before);
    assert!(!machine.built(), "it refuses before the build");
}

#[test]
fn refuses_a_scheduled_job_until_it_is_deleted() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    machine.log_tool_call(&id, "CronCreate", now_ms());
    let before = machine.daemon_pid();

    let refused = machine.update("origin/main");

    assert_refused(&machine, &refused, "scheduled jobs", &before);
    machine.log_tool_call(&id, "CronDelete", now_ms());
    assert_succeeded(&machine.update("origin/main"));
    assert!(machine.installed_is_the_new_build());
}

#[test]
fn usage_figures_that_name_the_tool_are_not_a_job() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    machine.log_usage_figures(&id, "CronCreate");

    assert_succeeded(&machine.update("origin/main"));
}

#[test]
fn a_job_from_before_the_daemon_started_is_gone() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    machine.log_tool_call(&id, "CronCreate", 1_000);

    assert_succeeded(&machine.update("origin/main"));
}

#[test]
fn refuses_a_commit_origin_does_not_have() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();

    let output = machine.update("0123456789abcdef0123456789abcdef01234567");

    assert_refused(&machine, &output, "push it first", &before);
}

#[test]
fn a_failed_build_leaves_the_daemon_running() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    File::create(machine.path("cargo-fails")).unwrap();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, "the daemon was not touched", &before);
}

#[test]
fn refuses_when_nothing_can_restart_the_daemon() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    fs::remove_file(machine.start_file()).unwrap();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, "cannot restart the daemon", &before);
    assert!(!machine.built(), "it refuses before the build");
}

#[test]
fn without_a_start_file_it_restarts_the_systemd_service() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    fs::remove_file(machine.start_file()).unwrap();
    File::create(machine.path("service-active")).unwrap();

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    let calls = fs::read_to_string(machine.path("systemctl.log")).unwrap();
    assert!(calls.contains("--user restart agents-cockpit"), "{calls}");
    assert_eq!(
        machine.daemon_pid(),
        before,
        "systemd stops the daemon, not the script"
    );
}

#[test]
fn a_connection_lost_during_the_restart_does_not_stop_it() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::write(
        machine.start_file(),
        format!(
            "touch '{stopped}'\nsleep 0.5\n{start}",
            stopped = machine.path("stopped").display()
        ),
    )
    .unwrap();
    let mut update = machine
        .command("sh")
        .args(["-s", "--", "--commit", "origin/main", "--binary"])
        .arg(machine.installed())
        .arg("--source-dir")
        .arg(machine.path("source"))
        .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while !machine.path("stopped").exists() {
        assert!(
            Instant::now() < deadline,
            "the script never restarted the daemon"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let hung_up = Command::new("kill")
        .args(["-HUP", &update.id().to_string()])
        .status()
        .unwrap();

    assert!(hung_up.success());
    assert!(update.wait().unwrap().success());
    assert_ne!(machine.daemon_pid(), before);
    machine.ok(&["list"]);
}

#[test]
fn refuses_a_second_update_while_one_runs() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    fs::create_dir_all(machine.path("home/cache/agents-cockpit/update/lock")).unwrap();

    let output = machine.update("origin/main");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("another update is running"));
    assert!(!machine.built());
    assert_eq!(machine.daemon_pid(), before);
}

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
use std::process::{Child, Command, Output, Stdio};
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
        Self::in_a_folder_named(".tmp")
    }

    fn in_a_folder_named(prefix: &str) -> Self {
        let machine = Self {
            temp: tempfile::Builder::new().prefix(prefix).tempdir().unwrap(),
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
if [ -f '{waits}' ]; then
    : >'{started}'
    until [ -f '{go}' ]; do sleep 0.02; done
fi
echo '   Compiling the daemon' || exit 101
echo 'warning: on stderr' >&2 || exit 101
[ ! -f '{fails}' ] || exit 1
while [ "$#" -gt 0 ] && [ "$1" != --target-dir ]; do shift; done
[ "$#" -ge 2 ] || exit 1
mkdir -p "$2/release"
cp '{BINARY}' "$2/release/agents-cockpit"
printf '%s' '{mark}' >>"$2/release/agents-cockpit"
"#,
                log = machine.path("cargo.log").display(),
                fails = machine.path("cargo-fails").display(),
                waits = machine.path("cargo-waits").display(),
                started = machine.path("cargo-started").display(),
                go = machine.path("cargo-go").display(),
                mark = String::from_utf8_lossy(BUILT_MARK),
            ),
        );
        machine.write_program(
            "bin/systemctl",
            &format!(
                "#!/bin/sh\n[ \"$#\" -gt 0 ] || exit 0\necho \"$*\" >>'{log}'\nif [ \"$2\" = restart ] && [ -f '{restart}' ]; then sh '{restart}'; fi\n[ -f '{active}' ]\n",
                log = machine.path("systemctl.log").display(),
                active = machine.path("service-active").display(),
                restart = machine.path("service-restart").display(),
            ),
        );
        fs::copy(BINARY, machine.installed()).unwrap();
        wait_until_it_runs(&machine.installed());
        fs::write(
            machine.start_file(),
            machine.start_text(&machine.installed()),
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

    /// A start file that runs `binary` as the daemon, in the background.
    fn start_text(&self, binary: &Path) -> String {
        format!(
            "'{binary}' serve >>'{log}' 2>&1 </dev/null &\necho $! >'{pid}'\n",
            binary = binary.display(),
            log = self.path("serve.log").display(),
            pid = self.path("daemon.pid").display(),
        )
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

    /// Starts an agent with a first prompt, and waits for it to work.
    fn working_agent(&self) -> String {
        let work = self.path("work");
        let id = self
            .ok(&["start", work.to_str().unwrap(), "Fix the parser"])
            .trim()
            .to_owned();
        self.wait_for_status(&id, "working");
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

    /// The script as the app runs it: read from stdin by the remote shell.
    fn update_command(&self, commit: &str) -> Command {
        let mut command = self.command("sh");
        command
            .args(["-s", "--", "--commit", commit, "--binary"])
            .arg(self.installed())
            .arg("--source-dir")
            .arg(self.path("source"))
            .stdin(Stdio::from(File::open(SCRIPT).unwrap()));
        command
    }

    fn update(&self, commit: &str) -> Output {
        self.update_command(commit).output().unwrap()
    }

    /// Asks the update that runs to stop, as the app does over a second connection.
    fn cancel(&self) -> Output {
        self.command("sh")
            .args(["-s", "--", "--cancel"])
            .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
            .output()
            .unwrap()
    }

    /// Starts an update whose fake `cargo` waits for `release_the_build`, and returns once the
    /// build has started.
    fn update_stopped_in_its_build(&self) -> Child {
        File::create(self.path("cargo-waits")).unwrap();
        let update = self
            .update_command("origin/main")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        while !self.path("cargo-started").exists() {
            assert!(Instant::now() < deadline, "the build never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        update
    }

    fn release_the_build(&self) {
        File::create(self.path("cargo-go")).unwrap();
    }

    fn installed_is_the_new_build(&self) -> bool {
        fs::read(self.installed()).unwrap().ends_with(BUILT_MARK)
    }

    fn built(&self) -> bool {
        self.path("cargo.log").exists()
    }

    fn update_folder(&self) -> PathBuf {
        self.path("home/cache/agents-cockpit/update")
    }

    fn update_lock_is_free(&self) -> bool {
        !self.update_folder().join("lock").exists()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let Ok(pid) = fs::read_to_string(self.path("daemon.pid")) else {
            return;
        };
        let pid = pid.trim();
        let _ = Command::new("kill").arg(pid).stderr(Stdio::null()).status();
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

/// Asserts that a refused update exited with `status`, changed nothing and said `expected`.
fn assert_refused(
    machine: &Machine,
    output: &Output,
    status: i32,
    expected: &str,
    daemon_before: &str,
) {
    assert_eq!(output.status.code(), Some(status), "{}", stderr(output));
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
    machine.working_agent();
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "Fix the parser", &before);
    assert!(!machine.built(), "it refuses before the build");
}

#[test]
fn refuses_while_an_agent_works_under_a_daemon_of_another_name() {
    let machine = Machine::new();
    let renamed = machine.path("install/cockpit-renamed");
    fs::copy(BINARY, &renamed).unwrap();
    wait_until_it_runs(&renamed);
    fs::write(machine.start_file(), machine.start_text(&renamed)).unwrap();
    machine.start_daemon();
    machine.working_agent();
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "Fix the parser", &before);
}

#[test]
fn refuses_while_an_agent_works_in_a_state_folder_with_glob_characters() {
    let machine = Machine::in_a_folder_named("sta[t]e");
    machine.start_daemon();
    machine.working_agent();
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "Fix the parser", &before);
}

#[test]
fn refuses_when_it_cannot_look_for_the_daemon() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    machine.write_program("bin/find", "#!/bin/sh\nexit 1\n");

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 1, "cannot tell", &before);
    assert!(!machine.built(), "it refuses before the build");
}

#[test]
fn refuses_when_it_cannot_tell_when_the_daemon_started() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    machine.write_program("bin/ps", "#!/bin/sh\nexit 0\n");

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 1, "cannot tell", &before);
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

    assert_refused(&machine, &refused, 3, "scheduled jobs", &before);
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

    assert_refused(&machine, &output, 1, "push it first", &before);
}

#[test]
fn a_failed_build_leaves_the_daemon_running() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    File::create(machine.path("cargo-fails")).unwrap();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 1, "the daemon was not touched", &before);
}

#[test]
fn refuses_when_nothing_can_restart_the_daemon() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    fs::remove_file(machine.start_file()).unwrap();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 1, "cannot restart the daemon", &before);
    assert!(!machine.built(), "it refuses before the build");
}

#[test]
fn without_a_start_file_it_restarts_the_systemd_service() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::remove_file(machine.start_file()).unwrap();
    File::create(machine.path("service-active")).unwrap();
    fs::write(
        machine.path("service-restart"),
        format!(
            "old=$(cat '{pid}')\nkill \"$old\"\nwhile [ -d \"/proc/$old\" ]; do sleep 0.02; done\n{start}",
            pid = machine.path("daemon.pid").display(),
        ),
    )
    .unwrap();

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    let calls = fs::read_to_string(machine.path("systemctl.log")).unwrap();
    assert!(calls.contains("--user restart agents-cockpit"), "{calls}");
    assert_ne!(machine.daemon_pid(), before);
}

#[test]
fn a_service_restart_that_leaves_the_old_daemon_exits_with_four() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    fs::remove_file(machine.start_file()).unwrap();
    File::create(machine.path("service-active")).unwrap();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert_eq!(machine.daemon_pid(), before);
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_start_file_that_starts_another_binary_exits_with_four() {
    let machine = Machine::new();
    let other = machine.path("install/other-copy");
    fs::copy(BINARY, &other).unwrap();
    wait_until_it_runs(&other);
    machine.start_daemon();
    fs::write(machine.start_file(), machine.start_text(&other)).unwrap();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
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

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("another update is running"));
    assert!(!machine.built());
    assert_eq!(machine.daemon_pid(), before);
}

#[test]
fn a_failed_restart_exits_with_four() {
    let machine = Machine::new();
    machine.start_daemon();
    fs::write(machine.start_file(), "exit 7\n").unwrap();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(machine.installed_is_the_new_build());
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_binary_it_cannot_replace_exits_with_four() {
    let machine = Machine::new();
    fs::write(machine.path("install/blocker"), "a file, not a folder").unwrap();
    let binary = machine.path("install/blocker/agents-cockpit");

    let output = machine
        .command("sh")
        .args(["-s", "--", "--commit", "origin/main", "--binary"])
        .arg(&binary)
        .arg("--source-dir")
        .arg(machine.path("source"))
        .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
}

#[test]
fn a_connection_lost_during_the_build_does_not_stop_it() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let mut update = machine.update_stopped_in_its_build();

    drop(update.stdout.take());
    drop(update.stderr.take());
    machine.release_the_build();

    assert!(update.wait().unwrap().success());
    assert!(machine.installed_is_the_new_build());
    assert_ne!(machine.daemon_pid(), before);
    machine.ok(&["list"]);
    let log = fs::read_to_string(machine.update_folder().join("build.log")).unwrap();
    assert!(log.contains("Compiling the daemon"), "{log}");
    assert!(log.contains("warning: on stderr"), "{log}");
}

#[test]
fn a_cancel_stops_the_update_before_it_installs() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let update = machine.update_stopped_in_its_build();

    let cancel = machine.cancel();
    machine.release_the_build();
    let output = update.wait_with_output().unwrap();

    assert_succeeded(&cancel);
    assert_refused(&machine, &output, 1, "the update was cancelled", &before);
    assert!(stderr(&output).contains("the daemon was not touched"));
}

#[test]
fn a_cancel_during_the_install_says_it_is_too_late() {
    let machine = Machine::new();
    machine.start_daemon();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::write(
        machine.start_file(),
        format!(
            "sh -s -- --cancel <'{SCRIPT}' >'{said}' 2>&1\necho $? >'{status}'\n{start}",
            said = machine.path("cancel-said").display(),
            status = machine.path("cancel-status").display(),
        ),
    )
    .unwrap();

    assert_succeeded(&machine.update("origin/main"));

    let said = fs::read_to_string(machine.path("cancel-said")).unwrap();
    assert!(said.contains("too late"), "{said}");
    let status = fs::read_to_string(machine.path("cancel-status")).unwrap();
    assert_eq!(status.trim(), "1", "{said}");
}

#[test]
fn a_second_cancel_is_no_failure() {
    let machine = Machine::new();
    machine.start_daemon();
    let update = machine.update_stopped_in_its_build();

    let first = machine.cancel();
    let second = machine.cancel();
    machine.release_the_build();
    let output = update.wait_with_output().unwrap();

    assert_succeeded(&first);
    assert_succeeded(&second);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn a_cancel_with_no_update_running_says_so() {
    let machine = Machine::new();

    let output = machine.cancel();

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("no update is running"));
    assert!(!machine.update_folder().join("cancel").exists());
}

#[test]
fn a_stale_cancel_does_not_stop_the_next_update() {
    let machine = Machine::new();
    machine.start_daemon();
    fs::create_dir_all(machine.update_folder()).unwrap();
    File::create(machine.update_folder().join("cancel")).unwrap();

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
}

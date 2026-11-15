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

use std::ffi::OsStr;
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
const BINARY: &str = env!("CARGO_BIN_EXE_yetack");
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
        for folder in ["bin", "install", "work", "home/config/yetack"] {
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
if [ -f '{broken}' ]; then
    printf '#!/bin/sh\nexit 1\n' >"$2/release/yetack"
    chmod 755 "$2/release/yetack"
else
    cp '{BINARY}' "$2/release/yetack"
fi
printf '%s' '{mark}' >>"$2/release/yetack"
"#,
                log = machine.path("cargo.log").display(),
                fails = machine.path("cargo-fails").display(),
                waits = machine.path("cargo-waits").display(),
                started = machine.path("cargo-started").display(),
                go = machine.path("cargo-go").display(),
                broken = machine.path("cargo-builds-broken").display(),
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
        self.path("install/yetack")
    }

    fn start_file(&self) -> PathBuf {
        self.path("home/config/yetack/start-daemon")
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
        self.path("home/state/yetack")
    }

    fn write_program(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        wait_until_it_runs(&path);
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
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

    /// Starts an agent whose first turn fails for a missing login, and waits for it to wait for
    /// its sign-in. `before` runs in its claude before the turn.
    fn agent_waiting_for_its_sign_in(&self, before: &str) -> String {
        self.write_program(
            "echo-claude",
            &format!(
                "#!/bin/sh\n{before}\nread -r _\necho '{{\"type\":\"assistant\",\"error\":\"authentication_failed\",\"message\":{{\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"Not logged in\"}}]}}}}'\necho '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"num_turns\":1}}'\nexec cat\n"
            ),
        );
        let work = self.path("work");
        let id = self
            .ok(&["start", work.to_str().unwrap(), "Fix the parser"])
            .trim()
            .to_owned();
        self.wait_for_status(&id, "needs_you");
        self.wait_for_column(&id, "WAIT", "0");
        id
    }

    /// Installs a binary whose `list` prints `listing`, as an older binary would, and that runs
    /// the real one otherwise. The daemon that runs keeps its own binary.
    fn list_as(&self, listing: &str) {
        fs::remove_file(self.installed()).unwrap();
        self.write_program(
            "install/yetack",
            &format!(
                "#!/bin/sh\nif [ \"$1\" = list ]; then printf '%s' '{listing}'; exit 0; fi\nexec '{BINARY}' \"$@\"\n"
            ),
        );
    }

    /// Waits until the `list` row of agent `id` shows `value` in the column headed `column`.
    fn wait_for_column(&self, id: &str, column: &str, value: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let list = self.ok(&["list"]);
            let mut lines = list.lines();
            let index = lines
                .next()
                .and_then(|header| header.split_whitespace().position(|name| name == column));
            let found = index.is_some_and(|index| {
                lines.any(|row| {
                    let columns: Vec<&str> = row.split_whitespace().collect();
                    columns.first() == Some(&id) && columns.get(index) == Some(&value)
                })
            });
            if found {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "agent {id} never had {value} in {column}: {list}"
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

    /// A copy of the test's binary, as the app sends it, with what its `--version` prints after
    /// the program's name.
    fn sent_binary(&self) -> (PathBuf, String) {
        let sent = self.path("home/cache/yetack/sent/yetack.a1b2c3");
        fs::create_dir_all(sent.parent().unwrap()).unwrap();
        fs::copy(BINARY, &sent).unwrap();
        let said = Command::new(BINARY).arg("--version").output().unwrap();
        let version = String::from_utf8(said.stdout).unwrap();
        let version = version.trim().strip_prefix("yetack ").unwrap().to_owned();
        (sent, version)
    }

    /// The update the app runs once it sent `sent`, which must have `sha256` and `version`.
    fn update_with_sent(&self, sent: &Path, sha256: &str, version: &str) -> Output {
        self.command("sh")
            .args(["-s", "--", "--commit", "3f9c2e1", "--binary"])
            .arg(self.installed())
            .arg("--built")
            .arg(sent)
            .args(["--sha256", sha256, "--version", version])
            .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
            .output()
            .unwrap()
    }

    /// The update the app runs for "Update now", which ends the busy agents' turns.
    fn update_even_if_busy(&self, commit: &str) -> Output {
        self.update_command(commit)
            .arg("--even-if-busy")
            .output()
            .unwrap()
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
        self.path("home/cache/yetack/update")
    }

    fn lock_file(&self) -> PathBuf {
        self.update_folder().join("lock.pid")
    }

    fn update_lock_is_free(&self) -> bool {
        !self.lock_file().exists()
    }

    /// Leaves the lock an update of this pid and start time would hold.
    fn write_lock(&self, pid: u32, start_time: &str) {
        fs::create_dir_all(self.update_folder()).unwrap();
        fs::write(self.lock_file(), format!("{pid} {start_time}\n")).unwrap();
    }

    fn previous(&self) -> PathBuf {
        PathBuf::from(format!("{}.previous", self.installed().display()))
    }

    fn previous_is_the_new_build(&self) -> bool {
        fs::read(self.previous()).unwrap().ends_with(BUILT_MARK)
    }

    /// The new build's daemon exits at once. Waiting for such a daemon takes no time here, as
    /// long as the build is what is installed.
    fn build_a_daemon_that_never_starts(&self) {
        File::create(self.path("cargo-builds-broken")).unwrap();
        self.write_program(
            "bin/sleep",
            &format!(
                "#!/bin/sh\n[ \"$(head -c 2 '{installed}')\" != '#!' ] || exit 0\nPATH=${{PATH#*:}} exec sleep \"$@\"\n",
                installed = self.installed().display(),
            ),
        );
    }

    /// A start file line that is true while the daemon that never starts is installed.
    fn the_broken_build_is_installed(&self) -> String {
        format!(
            "[ \"$(head -c 2 '{installed}')\" = '#!' ]",
            installed = self.installed().display()
        )
    }

    /// What a failed restart leaves: the old daemon runs on, and only the previous binary can
    /// talk to it.
    fn leave_the_old_daemon_behind_a_new_binary(&self) {
        fs::rename(self.installed(), self.previous()).unwrap();
        self.write_program("install/yetack", "#!/bin/sh\nexit 1\n");
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

/// The signals a process ignores, as `/proc` shows them.
fn ignored_signals(pid: &str) -> String {
    let status = fs::read_to_string(Path::new("/proc").join(pid).join("status")).unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .unwrap()
        .trim()
        .to_owned()
}

/// The start time of a process that runs, as `/proc/<pid>/stat` gives it.
fn start_time(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after_the_name = stat.rsplit_once(") ").unwrap().1;
    after_the_name
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned()
}

/// Waits until process `pid` runs `sleep`, once the commands before it ran.
fn wait_until_it_sleeps(pid: u32) {
    let comm = format!("/proc/{pid}/comm");
    let deadline = Instant::now() + WAIT;
    while fs::read_to_string(&comm).unwrap() != "sleep\n" {
        assert!(Instant::now() < deadline, "process {pid} never ran sleep");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A pid that no process has any more.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
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
            Path::new("home/cache/yetack/update/source"),
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

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum").arg(path).output().unwrap();
    let line = String::from_utf8(output.stdout).unwrap();
    line.split_whitespace().next().unwrap().to_owned()
}

#[test]
fn a_sent_binary_replaces_the_daemon_without_a_build_and_goes_away() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let (sent, version) = machine.sent_binary();
    let sent_bytes = fs::read(&sent).unwrap();

    let output = machine.update_with_sent(&sent, &sha256(&sent), &version);

    assert_succeeded(&output);
    assert!(!machine.built(), "nothing was built");
    assert_eq!(fs::read(machine.installed()).unwrap(), sent_bytes);
    assert_ne!(machine.daemon_pid(), before);
    machine.ok(&["list"]);
    assert!(!sent.exists(), "the sent file is deleted");
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_sent_binary_that_is_not_the_one_the_app_read_changes_nothing() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let installed = fs::read(machine.installed()).unwrap();
    let (sent, version) = machine.sent_binary();
    let other_sum = "0".repeat(64);

    let output = machine.update_with_sent(&sent, &other_sum, &version);

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("its checksum differs"));
    assert_eq!(fs::read(machine.installed()).unwrap(), installed);
    assert_eq!(machine.daemon_pid(), before);
    assert!(!sent.exists());
}

#[test]
fn a_sent_binary_of_another_version_changes_nothing() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    let installed = fs::read(machine.installed()).unwrap();
    let (sent, _) = machine.sent_binary();

    let output = machine.update_with_sent(&sent, &sha256(&sent), "9.9.9 (3f9c2e1)");

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("not \"yetack 9.9.9 (3f9c2e1)\""));
    assert_eq!(fs::read(machine.installed()).unwrap(), installed);
    assert_eq!(machine.daemon_pid(), before);
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
fn refuses_while_an_idle_agent_runs_background_tasks() {
    let machine = Machine::new();
    // A claude that reports a background command as it starts, then echoes what it reads.
    machine.write_program(
        "echo-claude",
        "#!/bin/sh\necho '{\"type\":\"system\",\"subtype\":\"task_started\",\"task_id\":\"bg-1\",\"description\":\"Watch the build\",\"task_type\":\"local_bash\"}'\nexec cat\n",
    );
    machine.start_daemon();
    let id = machine.idle_agent();
    machine.wait_for_column(&id, "TASKS", "1");
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "these agents are busy", &before);
    assert!(
        stderr(&output).contains(&format!("\n{id} ")),
        "{}",
        stderr(&output)
    );
}

/// A background command that Claude reports as it starts.
const BACKGROUND_TASK: &str = "echo '{\"type\":\"system\",\"subtype\":\"task_started\",\"task_id\":\"bg-1\",\"description\":\"Watch the build\",\"task_type\":\"local_bash\"}'";

#[test]
fn an_agent_that_waits_only_for_its_sign_in_runs_no_turn_to_interrupt() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.agent_waiting_for_its_sign_in("");

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
}

#[test]
fn refuses_while_an_agent_that_waits_for_its_sign_in_runs_background_tasks() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.agent_waiting_for_its_sign_in(BACKGROUND_TASK);
    machine.wait_for_column(&id, "TASKS", "1");
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "these agents are busy", &before);
    assert!(
        stderr(&output).contains(&format!("\n{id} ")),
        "{}",
        stderr(&output)
    );
}

#[test]
fn refuses_a_scheduled_job_of_an_agent_that_waits_for_its_sign_in() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.agent_waiting_for_its_sign_in("");
    machine.log_tool_call(&id, "CronCreate", now_ms());
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "scheduled jobs", &before);
}

#[test]
fn an_older_listing_without_tasks_lets_a_sign_in_wait_through() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.list_as(
        "ID   STATUS    WAIT TURNS TITLE     FOLDER\n7    needs_you 0    1     Sign in   /made/up/work\n8    idle      0    2     Done      /made/up/work\n",
    );

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
}

#[test]
fn a_listing_without_wait_counts_every_agent_that_needs_you_as_busy() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.list_as(
        "ID   STATUS    TURNS TITLE     FOLDER\n7    needs_you 1     Sign in   /made/up/work\n",
    );
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "\n7    needs_you", &before);
}

#[test]
fn refuses_an_agent_that_needs_you_with_a_question_to_answer() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.list_as(
        "ID   STATUS    WAIT TURNS TASKS TITLE     FOLDER\n7    needs_you 1    1     0     Approve   /made/up/work\n",
    );
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 3, "\n7    needs_you", &before);
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
fn refuses_when_it_cannot_read_the_lock_files_inode() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    machine.write_program("bin/stat", "#!/bin/sh\nexit 1\n");

    let output = machine.update("origin/main");

    assert_refused(&machine, &output, 1, "cannot tell", &before);
    assert!(!machine.built(), "it refuses before the build");
}

/// Some sandboxed kernels give a `/proc/locks` that lists no lock: the daemon would not show, and
/// its busy agents would restart. A fake `awk` finds no lock in it.
#[test]
fn refuses_when_proc_locks_lists_no_lock() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    machine.write_program(
        "bin/awk",
        r#"#!/bin/sh
for last in "$@"; do :; done
[ "$last" = /proc/locks ] && exit 1
PATH=${PATH#*:} exec awk "$@"
"#,
    );

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
fn even_if_busy_restarts_while_an_agent_works() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.working_agent();
    let before = machine.daemon_pid();

    let output = machine.update_even_if_busy("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert_ne!(machine.daemon_pid(), before);
    assert!(
        machine
            .ok(&["list"])
            .lines()
            .any(|row| row.starts_with(&id)),
        "the new daemon has the agent"
    );
}

#[test]
fn even_if_busy_restarts_with_a_scheduled_job() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    machine.log_tool_call(&id, "CronCreate", now_ms());
    let before = machine.daemon_pid();

    let output = machine.update_even_if_busy("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert_ne!(machine.daemon_pid(), before);
}

#[test]
fn even_if_busy_may_come_before_the_other_options() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.working_agent();

    let output = machine
        .command("sh")
        .args(["-s", "--", "--even-if-busy", "--commit", "origin/main"])
        .arg("--binary")
        .arg(machine.installed())
        .arg("--source-dir")
        .arg(machine.path("source"))
        .stdin(Stdio::from(File::open(SCRIPT).unwrap()))
        .output()
        .unwrap();

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
}

#[test]
fn even_if_busy_still_refuses_when_it_cannot_look_for_the_daemon() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.working_agent();
    let before = machine.daemon_pid();
    machine.write_program("bin/find", "#!/bin/sh\nexit 1\n");

    let output = machine.update_even_if_busy("origin/main");

    assert_refused(&machine, &output, 1, "cannot tell", &before);
    assert!(!machine.built(), "it refuses before the build");
}

#[test]
fn even_if_busy_still_reads_the_agents() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    machine.leave_the_old_daemon_behind_a_new_binary();
    fs::remove_file(machine.previous()).unwrap();
    machine.write_program(
        "install/yetack.previous",
        "#!/bin/sh\necho 'made-up failure' >&2\nexit 1\n",
    );

    let output = machine.update_even_if_busy("origin/main");

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot read the agents from the daemon"),
        "{}",
        stderr(&output)
    );
    assert!(!machine.built(), "it refuses before the build");
    assert_eq!(machine.daemon_pid(), before);
}

#[test]
fn even_if_busy_still_refuses_a_second_update() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.working_agent();
    let before = machine.daemon_pid();
    let pid = std::process::id();
    machine.write_lock(pid, &start_time(pid));

    let output = machine.update_even_if_busy("origin/main");

    assert_eq!(output.status.code(), Some(6), "{}", stderr(&output));
    assert!(!machine.built());
    assert_eq!(machine.daemon_pid(), before);
}

#[test]
fn after_a_failed_restart_the_previous_binary_reads_the_busy_agents() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.working_agent();
    let before = machine.daemon_pid();
    machine.leave_the_old_daemon_behind_a_new_binary();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Fix the parser"),
        "{}",
        stderr(&output)
    );
    assert!(!machine.built(), "it refuses before the build");
    assert_eq!(machine.daemon_pid(), before);
}

#[test]
fn after_a_failed_restart_an_update_with_idle_agents_goes_on() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    let before = machine.daemon_pid();
    machine.leave_the_old_daemon_behind_a_new_binary();

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert_ne!(machine.daemon_pid(), before);
    assert!(
        machine
            .ok(&["list"])
            .lines()
            .any(|row| row.starts_with(&id))
    );
}

#[test]
fn refuses_when_neither_binary_reads_the_agents() {
    let machine = Machine::new();
    machine.start_daemon();
    let before = machine.daemon_pid();
    machine.leave_the_old_daemon_behind_a_new_binary();
    fs::remove_file(machine.previous()).unwrap();
    machine.write_program(
        "install/yetack.previous",
        "#!/bin/sh\necho 'made-up failure' >&2\nexit 1\n",
    );

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot read the agents from the daemon"),
        "{}",
        stderr(&output)
    );
    assert!(!machine.built(), "it refuses before the build");
    assert_eq!(machine.daemon_pid(), before);
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
    assert!(calls.contains("--user restart yetack"), "{calls}");
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

/// The stop finds a pid that holds the lock, then the process of that pid changes before the
/// signal: another process got the pid meanwhile. Fakes play it with a process of the test. `awk`
/// says it holds the lock and `find` that it has the file open. Once the stop has looked at it
/// twice, `sed` gives it another start time.
#[test]
fn the_stop_signals_no_process_that_changed_since_it_found_its_pid() {
    let machine = Machine::new();
    machine.start_daemon();
    let mut other = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let other_pid = other.id();
    let checked = machine.path("other-checked");
    machine.write_program(
        "bin/awk",
        &format!(
            r#"#!/bin/sh
for last in "$@"; do :; done
if [ "$last" = /proc/locks ] && [ ! -f '{checked}' ] && grep -qF '{mark}' '{installed}' 2>/dev/null; then
    echo {other_pid}
    exit 0
fi
PATH=${{PATH#*:}} exec awk "$@"
"#,
            checked = checked.display(),
            mark = String::from_utf8_lossy(BUILT_MARK),
            installed = machine.installed().display(),
        ),
    );
    machine.write_program(
        "bin/find",
        &format!(
            r#"#!/bin/sh
if [ "$1" = /proc/{other_pid}/fd ] && [ ! -f '{checked}' ]; then
    if [ -f '{found}' ]; then
        : >'{checked}'
    fi
    : >'{found}'
    echo /proc/{other_pid}/fd/0
    exit 0
fi
PATH=${{PATH#*:}} exec find "$@"
"#,
            checked = checked.display(),
            found = machine.path("other-found").display(),
        ),
    );
    machine.write_program(
        "bin/sed",
        &format!(
            r#"#!/bin/sh
for last in "$@"; do :; done
if [ "$last" = /proc/{other_pid}/stat ] && [ -f '{checked}' ]; then
    PATH=${{PATH#*:}} sed "$@" | PATH=${{PATH#*:}} awk '{{ $20 = $20 + 1; print }}'
    exit 0
fi
PATH=${{PATH#*:}} exec sed "$@"
"#,
            checked = checked.display(),
        ),
    );
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    let other_runs = other.try_wait().unwrap().is_none();
    let _ = other.kill();
    let _ = other.wait();
    assert!(
        checked.exists(),
        "the stop never looked at the other process"
    );
    assert!(other_runs, "the process that changed got no signal");
    assert_succeeded(&output);
    assert_ne!(
        machine.daemon_pid(),
        before,
        "the daemon was stopped all the same"
    );
}

/// While the daemon starts an agent, its child has the lock's file open between fork and exec. Any
/// program may open the file too. Only the daemon holds the lock. This process opens the file
/// before the daemon starts, so its pid comes first in `/proc`.
#[test]
fn the_stop_leaves_a_process_that_only_opened_the_lock_file_alone() {
    let machine = Machine::new();
    let lock = machine.state().join("serve.lock");
    fs::create_dir_all(machine.state()).unwrap();
    File::create(&lock).unwrap();
    let mut opener = Command::new("sh")
        .args(["-c", "exec 9<\"$1\"; exec sleep 600", "sh"])
        .arg(&lock)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    wait_until_it_sleeps(opener.id());
    machine.start_daemon();
    let before = machine.daemon_pid();

    let output = machine.update("origin/main");

    let opener_runs = opener.try_wait().unwrap().is_none();
    let _ = opener.kill();
    let _ = opener.wait();
    assert_succeeded(&output);
    assert!(opener_runs, "the process that only opened the file runs on");
    assert_ne!(machine.daemon_pid(), before);
}

#[test]
fn the_restarted_daemon_ignores_no_signal_the_script_ignores() {
    let machine = Machine::new();
    machine.start_daemon();
    let ignored_before = ignored_signals(&machine.daemon_pid());

    assert_succeeded(&machine.update("origin/main"));

    assert_eq!(ignored_signals(&machine.daemon_pid()), ignored_before);
}

#[test]
fn the_start_file_does_not_run_in_the_update_checkout() {
    let machine = Machine::new();
    machine.start_daemon();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::write(
        machine.start_file(),
        format!(
            "pwd >'{folder}'\n{start}",
            folder = machine.path("start-folder").display()
        ),
    )
    .unwrap();

    assert_succeeded(&machine.update("origin/main"));

    let folder = fs::read_to_string(machine.path("start-folder")).unwrap();
    assert!(
        !Path::new(folder.trim()).starts_with(machine.update_folder()),
        "{folder}"
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
    let pid = std::process::id();
    machine.write_lock(pid, &start_time(pid));
    let lock = fs::read_to_string(machine.lock_file()).unwrap();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(6), "{}", stderr(&output));
    assert!(stderr(&output).contains("another update is running"));
    assert!(!stderr(&output).contains("remove"), "{}", stderr(&output));
    assert!(!machine.built());
    assert_eq!(machine.daemon_pid(), before);
    assert_eq!(fs::read_to_string(machine.lock_file()).unwrap(), lock);
}

#[test]
fn a_running_update_names_its_process_in_the_lock() {
    let machine = Machine::new();
    machine.start_daemon();
    let update = machine.update_stopped_in_its_build();
    let pid = update.id();
    let expected = format!("{pid} {}\n", start_time(pid));

    let lock = fs::read_to_string(machine.lock_file());
    let names: Vec<String> = fs::read_dir(machine.update_folder())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    machine.release_the_build();
    assert_succeeded(&update.wait_with_output().unwrap());

    assert_eq!(lock.unwrap(), expected);
    let leftovers = names
        .iter()
        .filter(|name| name.starts_with("lock.pid.") && *name != "lock.pid.guard");
    assert_eq!(leftovers.count(), 0, "{names:?}");
}

#[test]
fn the_lock_of_a_dead_process_is_taken_over() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.write_lock(dead_pid(), "12345");

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_reused_pid_counts_as_dead() {
    let machine = Machine::new();
    machine.start_daemon();
    machine.write_lock(std::process::id(), "12345");

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_killed_update_does_not_block_the_next_one() {
    let machine = Machine::new();
    machine.start_daemon();
    let mut killed = machine.update_stopped_in_its_build();
    killed.kill().unwrap();
    killed.wait().unwrap();
    fs::remove_file(machine.path("cargo-waits")).unwrap();
    machine.release_the_build();
    let deadline = Instant::now() + WAIT;
    while !machine.update_folder().join("build.status").exists() {
        assert!(Instant::now() < deadline, "the killed build never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!machine.update_lock_is_free());

    let output = machine.update("origin/main");

    assert_succeeded(&output);
    assert!(machine.installed_is_the_new_build());
    assert!(machine.update_lock_is_free());
}

#[test]
fn of_updates_racing_for_a_dead_processs_lock_exactly_one_wins() {
    const UPDATES: usize = 8;
    let machine = Machine::new();
    machine.start_daemon();
    machine.write_lock(dead_pid(), "12345");
    File::create(machine.path("cargo-waits")).unwrap();
    // `ps` runs right before the script takes the lock: every update waits there.
    machine.write_program(
        "bin/ps",
        &format!(
            "#!/bin/sh\n[ \"$#\" -gt 0 ] || exit 0\nuntil [ -f '{go}' ]; do sleep 0.01; done\nPATH=${{PATH#*:}} exec ps \"$@\"\n",
            go = machine.path("race-go").display(),
        ),
    );
    let mut updates: Vec<Child> = (0..UPDATES)
        .map(|_| {
            machine
                .update_command("origin/main")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();

    File::create(machine.path("race-go")).unwrap();
    let deadline = Instant::now() + WAIT;
    let mut refused = Vec::new();
    while refused.len() < UPDATES - 1 && Instant::now() < deadline {
        let mut still_running = Vec::new();
        for mut update in updates {
            match update.try_wait().unwrap() {
                Some(status) => refused.push(status.code()),
                None => still_running.push(update),
            }
        }
        updates = still_running;
        std::thread::sleep(Duration::from_millis(10));
    }
    while !machine.path("cargo-started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let builds = fs::read_to_string(machine.path("cargo.log")).unwrap_or_default();
    machine.release_the_build();
    let winners: Vec<Output> = updates
        .into_iter()
        .map(|update| update.wait_with_output().unwrap())
        .collect();

    assert_eq!(refused, [Some(6); UPDATES - 1]);
    assert_eq!(builds.lines().count(), 1, "{builds}");
    assert_eq!(winners.len(), 1);
    assert!(winners[0].status.success());
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_failed_restart_exits_with_four() {
    let machine = Machine::new();
    machine.start_daemon();
    fs::write(machine.start_file(), "exit 7\n").unwrap();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(!machine.installed_is_the_new_build(), "the rollback stays");
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_new_daemon_that_never_starts_is_rolled_back() {
    let machine = Machine::new();
    machine.start_daemon();
    let id = machine.idle_agent();
    let before = machine.daemon_pid();
    machine.build_a_daemon_that_never_starts();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(5), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("did not answer within 30 seconds"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("the previous version runs again"),
        "{}",
        stderr(&output)
    );
    assert!(!machine.installed_is_the_new_build());
    assert!(!machine.previous_is_the_new_build());
    assert_ne!(machine.daemon_pid(), before);
    assert!(
        machine
            .ok(&["list"])
            .lines()
            .any(|row| row.starts_with(&id))
    );
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_service_whose_new_daemon_never_starts_is_rolled_back() {
    let machine = Machine::new();
    machine.start_daemon();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::remove_file(machine.start_file()).unwrap();
    File::create(machine.path("service-active")).unwrap();
    fs::write(
        machine.path("service-restart"),
        format!(
            "old=$(cat '{pid}')\nkill \"$old\" 2>/dev/null\nwhile [ -d \"/proc/$old\" ]; do sleep 0.02; done\n{start}",
            pid = machine.path("daemon.pid").display(),
        ),
    )
    .unwrap();
    machine.build_a_daemon_that_never_starts();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(5), "{}", stderr(&output));
    assert!(!machine.installed_is_the_new_build());
    machine.ok(&["list"]);
    let calls = fs::read_to_string(machine.path("systemctl.log")).unwrap();
    assert_eq!(calls.matches("--user restart yetack").count(), 2, "{calls}");
}

#[test]
fn a_start_file_that_fails_with_the_new_binary_is_rolled_back() {
    let machine = Machine::new();
    machine.start_daemon();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::write(
        machine.start_file(),
        format!(
            "if {broken}; then exit 7; fi\n{start}",
            broken = machine.the_broken_build_is_installed()
        ),
    )
    .unwrap();
    machine.build_a_daemon_that_never_starts();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(5), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("start-daemon failed"),
        "{}",
        stderr(&output)
    );
    assert!(!machine.installed_is_the_new_build());
    machine.ok(&["list"]);
}

#[test]
fn a_rollback_that_fails_too_exits_with_four() {
    let machine = Machine::new();
    machine.start_daemon();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::write(
        machine.start_file(),
        format!(
            "if ! {broken}; then exit 7; fi\n{start}",
            broken = machine.the_broken_build_is_installed()
        ),
    )
    .unwrap();
    machine.build_a_daemon_that_never_starts();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("did not come back"),
        "{}",
        stderr(&output)
    );
    assert!(!machine.installed_is_the_new_build());
    assert!(machine.previous().exists());
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_new_daemon_with_no_previous_binary_exits_with_four() {
    let machine = Machine::new();
    fs::remove_file(machine.installed()).unwrap();
    machine.build_a_daemon_that_never_starts();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("did not answer within 30 seconds"),
        "{}",
        stderr(&output)
    );
    assert!(machine.installed_is_the_new_build());
    assert!(machine.update_lock_is_free());
}

#[test]
fn a_previous_binary_that_is_the_new_build_is_no_rollback() {
    let machine = Machine::new();
    fs::remove_file(machine.installed()).unwrap();
    machine.build_a_daemon_that_never_starts();
    assert_eq!(machine.update("origin/main").status.code(), Some(4));
    fs::copy(machine.installed(), machine.previous()).unwrap();

    let output = machine.update("origin/main");

    assert_eq!(output.status.code(), Some(4), "{}", stderr(&output));
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("going back"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_connection_lost_during_the_rollback_does_not_stop_it() {
    let machine = Machine::new();
    machine.start_daemon();
    let start = fs::read_to_string(machine.start_file()).unwrap();
    fs::write(
        machine.start_file(),
        format!(
            "if ! {broken}; then touch '{rolling}'; sleep 0.5; fi\n{start}",
            broken = machine.the_broken_build_is_installed(),
            rolling = machine.path("rolling-back").display(),
        ),
    )
    .unwrap();
    machine.build_a_daemon_that_never_starts();
    let mut update = machine
        .update_command("origin/main")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while !machine.path("rolling-back").exists() {
        assert!(Instant::now() < deadline, "the script never rolled back");
        std::thread::sleep(Duration::from_millis(10));
    }

    let hung_up = Command::new("kill")
        .args(["-HUP", &update.id().to_string()])
        .status()
        .unwrap();

    assert!(hung_up.success());
    assert_eq!(update.wait().unwrap().code(), Some(5));
    machine.ok(&["list"]);
}

#[test]
fn a_retry_keeps_the_last_binary_that_worked() {
    let machine = Machine::new();
    machine.start_daemon();
    fs::write(machine.start_file(), "exit 7\n").unwrap();

    assert_eq!(machine.update("origin/main").status.code(), Some(4));
    assert_eq!(machine.update("origin/main").status.code(), Some(4));

    assert!(!machine.previous_is_the_new_build());
}

#[test]
fn a_checkout_cloned_again_does_not_stop_the_next_update() {
    let machine = Machine::new();
    machine.start_daemon();
    assert_succeeded(&machine.update("origin/main"));
    fs::remove_dir_all(machine.path("source")).unwrap();
    machine.git(
        Path::new("."),
        &["clone", "--quiet", "origin.git", "source"],
    );
    let commit = machine.commit_on_origin("Second");

    let output = machine.update(&commit);

    assert_succeeded(&output);
    assert_eq!(
        machine.git(
            Path::new("home/cache/yetack/update/source"),
            &["rev-parse", "HEAD"]
        ),
        commit
    );
}

#[test]
fn a_binary_it_cannot_replace_exits_with_four() {
    let machine = Machine::new();
    fs::write(machine.path("install/blocker"), "a file, not a folder").unwrap();
    let binary = machine.path("install/blocker/yetack");

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
fn a_cancel_with_only_a_dead_processs_lock_says_no_update_runs() {
    let machine = Machine::new();
    machine.write_lock(dead_pid(), "12345");

    let output = machine.cancel();

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).contains("no update is running"));
    assert!(!machine.update_folder().join("cancel").exists());
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

//! `contrib/install-daemon.sh` against a real daemon in a home folder of its own. Fake
//! `systemctl` and `loginctl` stand in for systemd, and a fake `crontab` for cron.

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
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_yetack");
const SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contrib/install-daemon.sh"
);
const UPDATE_SCRIPT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contrib/update-daemon.sh"
);
/// The file an update runs to start the daemon where no systemd service does, in the server's
/// folder.
const START_FILE: &str = "home/config/yetack/start-daemon";

/// A home folder of its own, where the fake `systemctl` starts the installed daemon. Every command
/// it runs sees only this folder.
struct Server {
    temp: TempDir,
}

impl Server {
    fn new() -> Self {
        Self::in_folder(".tmp")
    }

    /// A server whose every path starts with `prefix`, such as one with a space.
    fn in_folder(prefix: &str) -> Self {
        let server = Self {
            temp: tempfile::Builder::new().prefix(prefix).tempdir().unwrap(),
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
    "--user cat yetack") [ -f '{unit}' ] ;;
    "--user is-active --quiet yetack" | "--user is-enabled --quiet yetack")
        [ ! -f '{no_session}' ] && [ -f '{unit}' ] ;;
    "--user enable --now yetack")
        "$HOME/.local/share/yetack/bin/yetack" serve >>'{serve_log}' 2>&1 </dev/null &
        echo $! >'{pid}'
        ;;
    "--user start yetack")
        "$HOME/.local/share/yetack/bin/yetack" serve >>'{serve_log}' 2>&1 </dev/null &
        echo $! >'{pid}'
        ;;
esac
"#,
                log = server.path("systemctl.log").display(),
                no_session = server.path("no-session").display(),
                unit = server.unit().display(),
                serve_log = server.path("serve.log").display(),
                pid = server.path("daemon.pid").display(),
            ),
        );
        server.write_program("bin/loginctl", "#!/bin/sh\necho no\n");
        server.write_program(
            "bin/crontab",
            &format!(
                r#"#!/bin/sh
case "$1" in
    -l)
        [ ! -f '{unreadable}' ] || {{ echo "crontab: cannot read the crontab" >&2; exit 1; }}
        cat '{tab}' 2>/dev/null || {{ echo "no crontab for someone" >&2; exit 1; }} ;;
    -) cat >'{tab}' ;;
esac
"#,
                tab = server.path("crontab").display(),
                unreadable = server.path("crontab-unreadable").display(),
            ),
        );
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
        let sent = self.path("home/cache/yetack/sent/yetack.a1b2c3");
        fs::create_dir_all(sent.parent().unwrap()).unwrap();
        fs::copy(BINARY, &sent).unwrap();
        let said = Command::new(BINARY).arg("--version").output().unwrap();
        let version = String::from_utf8(said.stdout).unwrap();
        let version = version.trim().strip_prefix("yetack ").unwrap().to_owned();
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
        self.path("home/.local/share/yetack/bin/yetack")
    }

    fn unit(&self) -> PathBuf {
        self.path("home/config/systemd/user/yetack.service")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(self.path("daemon.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).status();
        }
        // A daemon started without systemd: its path is this test's own.
        let installed = format!("{} serve", self.installed().display());
        let _ = Command::new("pkill").args(["-f", &installed]).status();
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
    assert!(unit.contains("ExecStart=%h/.local/share/yetack/bin/yetack serve"));
    let folder = server.path("home/.local/share/yetack");
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

/// Installs the binary the app sent, as the setup runs the script.
fn install_sent(server: &Server) -> Output {
    let (sent, version) = server.sent_binary();
    server.install(&[
        "--built",
        sent.to_str().unwrap(),
        "--sha256",
        &sha256(&sent),
        "--version",
        &version,
    ])
}

/// Without a systemd user session, the install starts the daemon apart from its own session.
fn install_without_systemd(server: &Server) -> Output {
    fs::write(server.path("no-session"), "").unwrap();
    install_sent(server)
}

#[test]
fn without_a_systemd_user_session_the_daemon_starts_on_its_own_and_nothing_starts_it_at_boot() {
    let server = Server::new();

    let output = install_without_systemd(&server);

    assert!(output.status.success(), "{}", stderr(&output));
    let said = stdout(&output);
    assert!(said.contains("install-daemon: the daemon runs"), "{said}");
    assert!(said.contains("install-daemon: service none"), "{said}");
    assert!(said.contains("install-daemon: boot no"), "{said}");
    assert!(said.contains("install-daemon: crond "), "{said}");
    assert!(
        said.contains(&format!(
            "install-daemon: start PATH={}/.local/bin:",
            server.path("home").display()
        )),
        "{said}"
    );
    assert!(!said.contains("linger"), "{said}");
    assert!(!server.unit().exists());
    let listed = server
        .command(server.installed().to_str().unwrap())
        .arg("list")
        .output()
        .unwrap();
    assert!(listed.status.success(), "{}", stderr(&listed));
}

#[test]
fn the_start_at_boot_goes_into_the_crontab_once_beside_the_users_lines() {
    let server = Server::new();
    fs::write(server.path("crontab"), "0 3 * * * backup\n").unwrap();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));

    let added = server.install(&["--add-boot-start"]);
    let again = server.install(&["--add-boot-start"]);

    assert!(added.status.success(), "{}", stderr(&added));
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(stdout(&again).contains("install-daemon: boot yes"));
    let crontab = fs::read_to_string(server.path("crontab")).unwrap();
    let lines: Vec<&str> = crontab.lines().collect();
    assert_eq!(lines.len(), 2, "{crontab}");
    assert_eq!(lines[0], "0 3 * * * backup");
    assert!(
        lines[1].starts_with(&format!(
            "@reboot PATH={}/.local/bin:",
            server.path("home").display()
        )),
        "{crontab}"
    );
    assert!(
        lines[1].contains(&format!("{} serve >>", server.installed().display())),
        "{crontab}"
    );
    assert!(
        stdout(&installed).contains(&format!("install-daemon: cron {}\n", lines[1])),
        "the report shows the line before it goes in: {}",
        stdout(&installed)
    );
}

/// The line the crontab holds, which the user's crontab has only once.
fn cron_line(server: &Server) -> String {
    let crontab = fs::read_to_string(server.path("crontab")).unwrap();
    let mut lines = crontab.lines().filter(|line| line.starts_with("@reboot "));
    let line = lines.next().unwrap().to_owned();
    assert_eq!(lines.next(), None, "{crontab}");
    line
}

#[test]
fn the_crontab_line_starts_the_daemon_from_a_folder_with_a_space() {
    let server = Server::in_folder(".tmp with space");
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));
    let added = server.install(&["--add-boot-start"]);
    assert!(added.status.success(), "{}", stderr(&added));
    stop_daemon(&server);

    // cron hands the line, past its time, to `sh -c`.
    let line = cron_line(&server);
    let command = line.strip_prefix("@reboot ").unwrap();
    let mut cron = server
        .command("sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = (0..50).any(|_| {
        thread::sleep(Duration::from_millis(100));
        answers(&server)
    });
    stop_daemon(&server);
    let _ = cron.wait();

    assert!(started, "{line}");
}

#[test]
fn a_crontab_line_cron_would_cut_at_a_percent_sign_is_not_added() {
    let server = Server::in_folder(".tmp 50%");
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));

    let added = server.install(&["--add-boot-start"]);

    assert_eq!(added.status.code(), Some(1), "{}", stderr(&added));
    assert!(
        stderr(&added).contains("add it by hand"),
        "{}",
        stderr(&added)
    );
    assert!(!server.path("crontab").exists());
}

#[test]
fn a_crontab_that_cannot_be_read_stays_as_it_is() {
    let server = Server::new();
    fs::write(server.path("crontab"), "0 3 * * * backup\n").unwrap();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));
    fs::write(server.path("crontab-unreadable"), "").unwrap();

    let added = server.install(&["--add-boot-start"]);

    assert_eq!(added.status.code(), Some(1), "{}", stderr(&added));
    assert!(
        stderr(&added).contains("crontab: cannot read the crontab"),
        "{}",
        stderr(&added)
    );
    assert_eq!(
        fs::read_to_string(server.path("crontab")).unwrap(),
        "0 3 * * * backup\n"
    );
}

#[test]
fn a_daemon_started_on_its_own_does_not_ignore_hang_ups() {
    let server = Server::new();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));

    let pattern = format!("{} serve", server.installed().display());
    let found = Command::new("pgrep")
        .args(["-f", &pattern])
        .output()
        .unwrap();
    let pids = String::from_utf8(found.stdout).unwrap();
    let pid = pids.lines().next().unwrap();
    if !Path::new("/proc/self/status").exists() {
        // No /proc: this system cannot tell.
        return;
    }
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let ignored = status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .unwrap();
    let mask = u64::from_str_radix(ignored.trim(), 16).unwrap();

    assert_eq!(mask & 1, 0, "SIGHUP is ignored: {ignored}");
}

#[test]
fn a_daemon_that_stops_as_it_starts_fails_the_start_at_once_with_its_last_word() {
    let server = Server::new();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));
    stop_daemon(&server);
    // Another process holds the state folder, as a daemon that no longer answers does.
    let lock = File::options()
        .write(true)
        .open(server.path("home/state/yetack/serve.lock"))
        .unwrap();
    lock.try_lock().unwrap();

    let begun = Instant::now();
    let started = server.install(&["--start", server.installed().to_str().unwrap()]);

    assert_eq!(started.status.code(), Some(4), "{}", stderr(&started));
    assert!(
        stderr(&started).contains("the daemon stopped as it started: "),
        "{}",
        stderr(&started)
    );
    assert!(begun.elapsed() < Duration::from_secs(20));
}

#[test]
fn a_service_the_user_set_up_is_left_alone() {
    let server = Server::new();
    fs::create_dir_all(server.unit().parent().unwrap()).unwrap();
    fs::write(
        server.unit(),
        "[Service]
ExecStart=%h/.cargo/bin/yetack serve
",
    )
    .unwrap();
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
    assert!(stderr(&output).contains("exists already"));
    assert!(!server.installed().exists());
    assert!(
        fs::read_to_string(server.unit())
            .unwrap()
            .contains(".cargo/bin")
    );
}

#[test]
fn a_unit_an_earlier_install_wrote_is_written_again() {
    let server = Server::new();
    fs::create_dir_all(server.unit().parent().unwrap()).unwrap();
    fs::write(
        server.unit(),
        "# The yetack daemon as a systemd user service, which the desktop app installed.\n",
    )
    .unwrap();
    let (sent, version) = server.sent_binary();

    let output = server.install(&[
        "--built",
        sent.to_str().unwrap(),
        "--sha256",
        &sha256(&sent),
        "--version",
        &version,
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(server.installed().exists());
}

/// The service's PATH holds the system's folders too, where this machine may have its own
/// `claude`: the one in the home folder comes first, so the test does not depend on the host.
#[test]
fn a_check_only_reports_what_the_daemon_needs() {
    let server = Server::new();
    server.write_program("home/.local/bin/claude", "#!/bin/sh\n");

    let output = server.install(&["--check"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "install-daemon: service systemd\ninstall-daemon: linger no\ninstall-daemon: claude {}\n",
            server.path("home/.local/bin/claude").display()
        )
    );
}

/// Stops the daemon the test started, and waits until it no longer answers.
fn stop_daemon(server: &Server) {
    let installed = format!("{} serve", server.installed().display());
    let _ = Command::new("pkill").args(["-f", &installed]).status();
    if let Ok(pid) = fs::read_to_string(server.path("daemon.pid")) {
        let _ = Command::new("kill").arg(pid.trim()).status();
    }
    for _ in 0..50 {
        let listed = server
            .command(server.installed().to_str().unwrap())
            .arg("list")
            .output()
            .unwrap();
        if !listed.status.success() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("the daemon still answers");
}

fn answers(server: &Server) -> bool {
    server
        .command(server.installed().to_str().unwrap())
        .arg("list")
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn a_start_brings_back_a_daemon_started_without_systemd() {
    let server = Server::new();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));
    stop_daemon(&server);

    let started = server.install(&["--start", server.installed().to_str().unwrap()]);

    assert!(started.status.success(), "{}", stderr(&started));
    assert!(stdout(&started).contains("install-daemon: starting the daemon"));
    assert!(answers(&server));
}

#[test]
fn an_update_restarts_a_daemon_the_install_started_without_systemd() {
    let server = Server::new();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));
    let (sent, version) = server.sent_binary();

    let updated = server
        .command("sh")
        .args(["-s", "--", "--commit", "3f9c2e1", "--binary"])
        .arg(server.installed())
        .arg("--built")
        .arg(&sent)
        .args(["--sha256", &sha256(&sent), "--version", &version])
        .stdin(Stdio::from(File::open(UPDATE_SCRIPT).unwrap()))
        .output()
        .unwrap();

    assert!(updated.status.success(), "{}", stderr(&updated));
    assert!(answers(&server));
}

#[test]
fn a_start_file_the_user_wrote_is_left_alone() {
    let server = Server::new();
    let own = "#!/bin/sh\ntmux new-session -d -s yetack yetack serve\n";
    server.write_program(START_FILE, own);

    let installed = install_without_systemd(&server);

    assert!(installed.status.success(), "{}", stderr(&installed));
    assert_eq!(fs::read_to_string(server.path(START_FILE)).unwrap(), own);
}

/// The start file an install writes: the mark that tells it from one the user wrote, then a line
/// an earlier version of the install may have written differently.
const OLD_START_FILE: &str =
    "#!/bin/sh\n# Written by the desktop app, which installed the yetack daemon.\nold start\n";

#[test]
fn a_start_file_an_earlier_install_wrote_is_written_again() {
    let server = Server::new();
    server.write_program(START_FILE, OLD_START_FILE);

    let installed = install_without_systemd(&server);

    assert!(installed.status.success(), "{}", stderr(&installed));
    let file = fs::read_to_string(server.path(START_FILE)).unwrap();
    assert!(!file.contains("old start"), "{file}");
    assert!(
        file.contains(&format!("{} serve", server.installed().display())),
        "{file}"
    );
}

#[test]
fn an_install_with_systemd_removes_the_start_file_an_earlier_install_wrote() {
    let server = Server::new();
    server.write_program(START_FILE, OLD_START_FILE);

    let installed = install_sent(&server);

    assert!(installed.status.success(), "{}", stderr(&installed));
    assert!(server.unit().exists());
    assert!(!server.path(START_FILE).exists());
}

#[test]
fn a_start_uses_the_systemd_service_when_there_is_one() {
    let server = Server::new();
    let (sent, version) = server.sent_binary();
    let installed = server.install(&[
        "--built",
        sent.to_str().unwrap(),
        "--sha256",
        &sha256(&sent),
        "--version",
        &version,
    ]);
    assert!(installed.status.success(), "{}", stderr(&installed));
    stop_daemon(&server);

    let started = server.install(&["--start", "yetack"]);

    assert!(started.status.success(), "{}", stderr(&started));
    assert!(stdout(&started).contains("install-daemon: starting the service"));
    let log = fs::read_to_string(server.path("systemctl.log")).unwrap();
    assert!(log.contains("--user start yetack"), "{log}");
    assert!(answers(&server));
}

#[test]
fn a_check_and_a_crontab_line_for_a_daemon_elsewhere_name_that_daemon() {
    let server = Server::new();
    let installed = install_without_systemd(&server);
    assert!(installed.status.success(), "{}", stderr(&installed));
    let elsewhere = server.path("home/opt/yetack");
    fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    fs::copy(server.installed(), &elsewhere).unwrap();
    let binary = elsewhere.to_str().unwrap();

    let checked = server.install(&["--check", "--binary", binary]);
    let added = server.install(&["--add-boot-start", "--binary", binary]);

    assert!(checked.status.success(), "{}", stderr(&checked));
    assert!(
        stdout(&checked).contains(&format!(" {binary} serve >>")),
        "{}",
        stdout(&checked)
    );
    assert!(added.status.success(), "{}", stderr(&added));
    let line = cron_line(&server);
    assert!(line.contains(&format!(" {binary} serve >>")), "{line}");
}

#[test]
fn a_server_where_no_cron_daemon_runs_says_so() {
    let server = Server::new();
    server.write_program("bin/pgrep", "#!/bin/sh\nexit 1\n");

    let installed = install_without_systemd(&server);

    assert!(installed.status.success(), "{}", stderr(&installed));
    // Where /proc hides the system's processes, the script cannot tell.
    let seen = if fs::read("/proc/1/stat").is_ok() {
        "no"
    } else {
        "unknown"
    };
    let said = stdout(&installed);
    assert!(
        said.contains(&format!("install-daemon: crond {seen}\n")),
        "{said}"
    );
}

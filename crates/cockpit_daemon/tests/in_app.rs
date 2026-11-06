//! The daemon in the app end to end: `InAppDaemon` in the test's process, the fake `claude`, and
//! clients over in-memory streams.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure is a failed test"
)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cockpit_client::{Connection, InAppLink, Transport};
use cockpit_daemon::{DaemonConfig, InAppDaemon, ServeError, StoreError};
use cockpit_protocol::{
    AgentStatus, AgentSummary, Catchup, Event, Prompt, Request, ResponseBody, StartParams,
};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);
/// The fake `claude` records this first on its command line when it should carry on with a turn
/// that was cut off.
const RESUME_INTERRUPTED_TURN: &str = "CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1";

/// A state folder, an agent folder and a Claude folder, and a `claude` that runs the fake.
struct Setup {
    temp: TempDir,
}

impl Setup {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        for folder in ["state", "work", "claude"] {
            std::fs::create_dir(temp.path().join(folder)).unwrap();
        }
        let setup = Self { temp };
        // The daemon runs in the test's process, whose environment the test leaves alone: the
        // fake's settings go through a script.
        let script = format!(
            "#!/bin/sh\necho $$ >> '{pids}'\nFAKE_CLAUDE_ARGS_FILE='{args}' exec '{fake}' \"$@\"\n",
            pids = setup.pids_file().display(),
            args = setup.args_file().display(),
            fake = env!("CARGO_BIN_EXE_fake-claude"),
        );
        std::fs::write(setup.claude(), script).unwrap();
        std::fs::set_permissions(setup.claude(), std::fs::Permissions::from_mode(0o700)).unwrap();
        setup
    }

    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }

    fn claude(&self) -> PathBuf {
        self.temp.path().join("claude.sh")
    }

    fn args_file(&self) -> PathBuf {
        self.temp.path().join("claude-args.jsonl")
    }

    fn pids_file(&self) -> PathBuf {
        self.temp.path().join("claude-pids")
    }

    fn config(&self) -> DaemonConfig {
        DaemonConfig {
            state_dir: self.state(),
            claude_program: self.claude(),
            claude_config_dir: self.temp.path().join("claude"),
        }
    }

    async fn start_in_app(&self) -> Result<(InAppDaemon, Transport), ServeError> {
        let (link, connections) = InAppLink::new();
        let daemon = InAppDaemon::start(self.config(), connections).await?;
        Ok((daemon, Transport::InApp(link)))
    }

    /// `agents-cockpit serve` in its own process, as a background service.
    fn start_service(&self) -> Child {
        let child = Command::new(env!("CARGO_BIN_EXE_test-serve"))
            .arg(self.state())
            .env(cockpit_daemon::CLAUDE_PROGRAM_ENV, self.claude())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let socket = cockpit_daemon::socket_path(&self.state());
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(Instant::now() < deadline, "the service did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        child
    }

    /// The command lines of every `claude` started so far, once `count` of them have started.
    fn wait_for_launches(&self, count: usize) -> Vec<Vec<String>> {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = std::fs::read_to_string(self.args_file()).unwrap_or_default();
            let launches: Vec<Vec<String>> = text
                .split_inclusive('\n')
                .filter_map(|line| line.strip_suffix('\n'))
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if launches.len() >= count {
                return launches;
            }
            assert!(
                Instant::now() < deadline,
                "claude did not start: {launches:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn claude_pids(&self) -> Vec<i32> {
        let text = std::fs::read_to_string(self.pids_file()).unwrap_or_default();
        text.lines().map(|line| line.parse().unwrap()).collect()
    }
}

/// Stops the daemon as the app does as it closes, from a thread outside any runtime.
fn shut_down(daemon: InAppDaemon) {
    std::thread::spawn(move || daemon.shutdown(WAIT))
        .join()
        .unwrap();
}

/// Whether the process runs, a zombie counting as ended.
fn is_running(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let state = stat.rsplit(") ").next().unwrap().chars().next().unwrap();
            state != 'Z'
        }
        Err(_) => false,
    }
}

async fn connect(transport: &Transport) -> (Connection, Catchup) {
    tokio::time::timeout(WAIT, Connection::connect(transport, None))
        .await
        .expect("a welcome in time")
        .unwrap()
}

async fn summary_with(
    connection: &mut Connection,
    mut matches: impl FnMut(&AgentSummary) -> bool,
) -> AgentSummary {
    loop {
        let record = tokio::time::timeout(WAIT, connection.next_event())
            .await
            .expect("an event in time")
            .expect("the connection is open");
        if let Event::AgentAdded(summary) | Event::AgentUpdated(summary) = record.event
            && matches(&summary)
        {
            return summary;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_that_closes_mid_turn_resumes_the_turn_when_it_runs_agents_again() {
    let setup = Setup::new();
    let (daemon, transport) = setup.start_in_app().await.unwrap();
    let (mut connection, catchup) = connect(&transport).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("a first connection gets a snapshot");
    };
    assert!(snapshot.agents.is_empty());
    let start = StartParams {
        folder: setup.temp.path().join("work"),
        title: None,
        model: None,
        effort: None,
        mode: None,
        prompt: Some(Prompt {
            text: "slow".to_owned(),
            images: Vec::new(),
        }),
    };
    let ResponseBody::AgentStarted { agent } = connection
        .request(Request::AgentStart(start))
        .await
        .unwrap()
    else {
        panic!("expected AgentStarted");
    };
    let working = summary_with(&mut connection, |summary| {
        summary.id == agent && summary.status == AgentStatus::Working && summary.session.is_some()
    })
    .await;
    let session = working.session.unwrap();

    shut_down(daemon);

    // The events sent before the close come first.
    tokio::time::timeout(WAIT, async {
        while connection.next_event().await.is_ok() {}
    })
    .await
    .expect("the connection ends in time");
    let pids = setup.claude_pids();
    assert_eq!(pids.len(), 1);
    assert!(!is_running(pids[0]), "the app's close stops claude");

    let (daemon, transport) = setup.start_in_app().await.unwrap();
    let (_connection, catchup) = connect(&transport).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("a first connection gets a snapshot");
    };
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].id, agent);
    assert_ne!(snapshot.agents[0].status, AgentStatus::Exited);
    let launches = setup.wait_for_launches(2);
    assert!(
        launches[1].contains(&format!("--resume={}", session.as_str())),
        "{launches:?}"
    );
    assert!(
        launches[1].contains(&RESUME_INTERRUPTED_TURN.to_owned()),
        "the app's close cut the turn off: {launches:?}"
    );
    shut_down(daemon);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_daemon_at_a_time_serves_a_state_folder() {
    let setup = Setup::new();
    let (first, _) = setup.start_in_app().await.unwrap();

    let second = setup.start_in_app().await;
    assert!(
        matches!(
            second,
            Err(ServeError::Store(StoreError::AlreadyRunning { .. }))
        ),
        "a second app is refused"
    );
    let service = Command::new(env!("CARGO_BIN_EXE_test-serve"))
        .arg(setup.state())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!service.success(), "a background service is refused");

    shut_down(first);
    let mut service = setup.start_service();
    let refused = setup.start_in_app().await;
    assert!(
        matches!(
            refused,
            Err(ServeError::Store(StoreError::AlreadyRunning { .. }))
        ),
        "the app finds the service's lock"
    );
    let socket = Transport::Local {
        socket: cockpit_daemon::socket_path(&setup.state()),
    };
    connect(&socket).await;
    service.kill().unwrap();
    service.wait().unwrap();
}

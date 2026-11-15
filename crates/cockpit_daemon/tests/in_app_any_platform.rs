//! The daemon in the app with an agent, on Windows as on Unix: the fake `claude` runs as it is,
//! without the shell script `in_app.rs` wraps it in.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure is a failed test"
)]

use std::time::Duration;

use cockpit_client::{Connection, InAppLink, Transport};
use cockpit_daemon::{DaemonConfig, InAppDaemon};
use cockpit_protocol::{
    AccountId, AgentStatus, Catchup, ClaudeCodeVersion, Event, Prompt, Request, ResponseBody,
    StartParams,
};

const WAIT: Duration = Duration::from_secs(15);

async fn start(config: &DaemonConfig) -> (InAppDaemon, Transport) {
    let (link, connections) = InAppLink::new();
    let daemon = InAppDaemon::start(config.clone(), connections)
        .await
        .unwrap();
    (daemon, Transport::InApp(link))
}

async fn connect(transport: &Transport) -> (Connection, Catchup) {
    let (connection, welcome) = tokio::time::timeout(WAIT, Connection::connect(transport, None))
        .await
        .expect("a welcome in time")
        .unwrap();
    (connection, welcome.catchup)
}

/// Stops the daemon as the app does as it closes, from a thread outside any runtime.
fn shut_down(daemon: InAppDaemon) {
    std::thread::spawn(move || daemon.shutdown(WAIT))
        .join()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_runs_and_is_still_there_after_the_app_runs_agents_again() {
    let temp = tempfile::tempdir().unwrap();
    for folder in ["state", "work", "claude"] {
        std::fs::create_dir(temp.path().join(folder)).unwrap();
    }
    let config = DaemonConfig {
        state_dir: temp.path().join("state"),
        claude_program: env!("CARGO_BIN_EXE_fake-claude").into(),
        claude_config_dir: temp.path().join("claude"),
    };
    let (daemon, transport) = start(&config).await;
    let (mut connection, _) = connect(&transport).await;
    let start_params = StartParams {
        folder: temp.path().join("work"),
        title: None,
        model: None,
        effort: None,
        mode: None,
        prompt: Some(Prompt {
            text: "slow".to_owned(),
            images: Vec::new(),
        }),
        account: AccountId::DEFAULT,
    };
    let ResponseBody::AgentStarted { agent } = connection
        .request(Request::AgentStart(start_params))
        .await
        .unwrap()
    else {
        panic!("expected AgentStarted");
    };
    loop {
        let record = tokio::time::timeout(WAIT, connection.next_event())
            .await
            .expect("an event in time")
            .expect("the connection is open");
        if let Event::AgentAdded(summary) | Event::AgentUpdated(summary) = record.event {
            assert_ne!(summary.status, AgentStatus::Exited, "claude ran");
            if summary.id == agent
                && summary.status == AgentStatus::Working
                && summary.session.is_some()
            {
                let version = summary.claude_code_version.as_ref();
                assert_eq!(version.map(ClaudeCodeVersion::as_str), Some("9.9.9"));
                break;
            }
        }
    }

    shut_down(daemon);
    tokio::time::timeout(WAIT, async {
        while connection.next_event().await.is_ok() {}
    })
    .await
    .expect("the connection ends in time");

    let (daemon, transport) = start(&config).await;
    let (_connection, catchup) = connect(&transport).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("a first connection gets a snapshot");
    };
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].id, agent);
    assert_ne!(snapshot.agents[0].status, AgentStatus::Exited);
    shut_down(daemon);
}

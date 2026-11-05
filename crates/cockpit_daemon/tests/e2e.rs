//! The daemon end to end: `test-serve` in its own process, the fake `claude`, and clients over the
//! socket.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code: a failure is a failed test"
)]

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cockpit_client::{Connection, RequestError, Transport};
use cockpit_protocol::{
    AgentId, AgentParams, AgentStatus, AgentSummary, Answer, AnswerParams, AnswerState, Approval,
    ApprovalAnswer, ApprovalDecision, BackgroundTaskId, CancelQueuedParams, Catchup,
    ClaudeSessionId, CommandKind, CostUsd, Effort, ErrorCode, Event, EventRecord, HistoryParams,
    ItemBody, ItemId, McpRemoveParams, McpScope, McpServerParams, McpServerStatus, McpToggleParams,
    PendingId, PermissionMode, Plan, PlanAnswer, PlanDecision, Prompt, PromptParams, Question,
    QuestionAnswer, ReadFileParams, RenameParams, Request, ResponseBody, Seq, SessionResumeParams,
    SetEffortParams, SetModeParams, SetModelParams, Settings, StartParams, StopTaskParams,
    TranscriptItem,
};
use rustix::process::{Pid, Signal};
use serde_json::Value;
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);
const SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";
/// The fake `claude` records this first on its command line when it should carry on with a turn
/// that was cut off.
const RESUME_INTERRUPTED_TURN: &str = "CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1";

/// A state folder, an agent folder and a Claude folder, and the daemon serving them.
struct Setup {
    temp: TempDir,
    daemon: Option<Child>,
    starts: u32,
    /// The umask the daemon starts with, as `umask` takes it; `None` keeps the test's.
    umask: Option<&'static str>,
}

impl Setup {
    fn new() -> Self {
        Self::new_with_umask(None)
    }

    fn new_with_umask(umask: Option<&'static str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        for folder in ["state", "work", "claude"] {
            std::fs::create_dir(temp.path().join(folder)).unwrap();
        }
        let mut setup = Self {
            temp,
            daemon: None,
            starts: 0,
            umask,
        };
        setup.start();
        setup
    }

    fn state(&self) -> PathBuf {
        self.temp.path().join("state")
    }

    fn work(&self) -> PathBuf {
        self.temp.path().join("work").canonicalize().unwrap()
    }

    fn socket(&self) -> PathBuf {
        cockpit_daemon::socket_path(&self.state())
    }

    fn args_file(&self) -> PathBuf {
        self.temp.path().join("claude-args.jsonl")
    }

    fn serve_command(&mut self) -> Command {
        self.starts += 1;
        let log =
            File::create(self.temp.path().join(format!("daemon-{}.log", self.starts))).unwrap();
        let serve = env!("CARGO_BIN_EXE_test-serve");
        let mut command = match self.umask {
            // `exec` keeps the daemon's pid, which `stop` signals.
            Some(umask) => {
                let mut shell = Command::new("/bin/sh");
                shell
                    .arg("-c")
                    .arg(format!("umask {umask} && exec \"$0\" \"$@\""))
                    .arg(serve);
                shell
            }
            None => Command::new(serve),
        };
        command
            .arg(self.state())
            .env(
                cockpit_daemon::CLAUDE_PROGRAM_ENV,
                env!("CARGO_BIN_EXE_fake-claude"),
            )
            .env("CLAUDE_CONFIG_DIR", self.temp.path().join("claude"))
            .env("FAKE_CLAUDE_ARGS_FILE", self.args_file())
            .env(
                "FAKE_CLAUDE_SESSION_NAMES",
                self.temp.path().join("session-names"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        command
    }

    fn start(&mut self) {
        let child = self.serve_command().spawn().unwrap();
        self.daemon = Some(child);
        let deadline = Instant::now() + WAIT;
        while std::os::unix::net::UnixStream::connect(self.socket()).is_err() {
            assert!(Instant::now() < deadline, "the daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Stops the daemon as systemd does, with SIGTERM.
    fn stop(&mut self) {
        let Some(mut child) = self.daemon.take() else {
            return;
        };
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, Signal::TERM).unwrap();
        let deadline = Instant::now() + WAIT;
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the daemon did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn restart(&mut self) {
        self.stop();
        self.start();
    }

    /// The command lines of every `claude` started so far, once `count` of them have started.
    fn wait_for_launches(&self, count: usize) -> Vec<Vec<String>> {
        let deadline = Instant::now() + WAIT;
        loop {
            let launches = self.claude_args();
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

    /// The command lines of every `claude` started so far.
    fn claude_args(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.args_file()).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn connect(&self) -> Client {
        self.connect_after(None).await.0
    }

    async fn connect_after(&self, last_seq: Option<Seq>) -> (Client, Catchup) {
        let transport = Transport::Local {
            socket: self.socket(),
        };
        let (connection, catchup) = Connection::connect(&transport, last_seq).await.unwrap();
        let last_seq = match &catchup {
            Catchup::Snapshot(snapshot) => snapshot.last_seq,
            Catchup::Replay => last_seq,
        };
        (
            Client {
                connection,
                last_seq,
            },
            catchup,
        )
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Client {
    connection: Connection,
    last_seq: Option<Seq>,
}

impl Client {
    async fn request(&self, request: Request) -> Result<ResponseBody, RequestError> {
        tokio::time::timeout(WAIT, self.connection.request(request))
            .await
            .expect("an answer in time")
    }

    async fn ok(&self, request: Request) -> ResponseBody {
        let method = request.method();
        match self.request(request).await {
            Ok(body) => body,
            Err(error) => panic!("{method} failed: {error:?}"),
        }
    }

    async fn error_code(&self, request: Request) -> ErrorCode {
        match self.request(request).await {
            Err(RequestError::Failed(error)) => error.code,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    async fn next_event(&mut self) -> EventRecord {
        let record = tokio::time::timeout(WAIT, self.connection.next_event())
            .await
            .expect("an event in time")
            .expect("the connection is open");
        self.last_seq = Some(record.seq);
        record
    }

    /// Skips events until one matches, and returns it.
    async fn wait_for<T>(&mut self, mut found: impl FnMut(&Event) -> Option<T>) -> T {
        loop {
            let record = self.next_event().await;
            if let Some(value) = found(&record.event) {
                return value;
            }
        }
    }

    async fn start(&mut self, folder: &Path, prompt: &str) -> AgentId {
        let params = StartParams {
            folder: folder.to_owned(),
            title: None,
            model: None,
            effort: None,
            mode: None,
            prompt: Some(text(prompt)),
        };
        match self.ok(Request::AgentStart(params)).await {
            ResponseBody::AgentStarted { agent } => agent,
            other => panic!("expected AgentStarted, got {other:?}"),
        }
    }

    async fn prompt(&self, agent: AgentId, prompt: &str) {
        let params = PromptParams {
            agent,
            prompt: text(prompt),
        };
        assert_eq!(
            self.ok(Request::AgentPrompt(params)).await,
            ResponseBody::Done
        );
    }

    async fn answer(&self, agent: AgentId, pending: PendingId, answer: Answer) {
        let params = AnswerParams {
            agent,
            pending,
            answer,
        };
        assert_eq!(
            self.ok(Request::AgentAnswer(params)).await,
            ResponseBody::Done
        );
    }

    async fn summary_with(
        &mut self,
        agent: AgentId,
        mut matches: impl FnMut(&AgentSummary) -> bool,
    ) -> AgentSummary {
        self.wait_for(|event| match event {
            Event::AgentAdded(summary) | Event::AgentUpdated(summary)
                if summary.id == agent && matches(summary) =>
            {
                Some(summary.clone())
            }
            _ => None,
        })
        .await
    }

    /// Waits for an item of the agent, appended or changed, that matches.
    async fn item_with(
        &mut self,
        agent: AgentId,
        mut matches: impl FnMut(&ItemBody) -> bool,
    ) -> TranscriptItem {
        self.wait_for(|event| match event {
            Event::TranscriptAppended { agent: from, item }
            | Event::TranscriptChanged { agent: from, item }
                if *from == agent && matches(&item.body) =>
            {
                Some(item.clone())
            }
            _ => None,
        })
        .await
    }

    async fn says(&mut self, agent: AgentId, expected: &str) {
        self.item_with(
            agent,
            |body| matches!(body, ItemBody::AssistantText { markdown } if markdown == expected),
        )
        .await;
    }

    async fn idle(&mut self, agent: AgentId) -> AgentSummary {
        self.summary_with(agent, |summary| summary.status == AgentStatus::Idle)
            .await
    }

    async fn history(&self, agent: AgentId) -> Vec<TranscriptItem> {
        let params = HistoryParams {
            agent,
            before: None,
            limit: 200,
        };
        match self.ok(Request::AgentHistory(params)).await {
            ResponseBody::History(page) => page.items,
            other => panic!("expected a history page, got {other:?}"),
        }
    }
}

fn text(prompt: &str) -> Prompt {
    Prompt {
        text: prompt.to_owned(),
        images: Vec::new(),
    }
}

fn pending_of(body: &ItemBody) -> Option<PendingId> {
    match body {
        ItemBody::Approval(Approval {
            pending,
            answer: AnswerState::Waiting,
            ..
        })
        | ItemBody::Question(Question {
            pending,
            answer: AnswerState::Waiting,
            ..
        })
        | ItemBody::Plan(Plan {
            pending,
            answer: AnswerState::Waiting,
            ..
        }) => Some(pending.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn an_agent_answers_a_prompt_and_reports_its_state() {
    let setup = Setup::new();
    let mut client = setup.connect().await;

    let agent = client.start(&setup.work(), "hello there").await;

    let added = client
        .summary_with(agent, |summary| summary.title == "hello there")
        .await;
    assert_eq!(added.folder, setup.work());
    client.says(agent, "Echo: hello there").await;
    let idle = client.idle(agent).await;
    assert_eq!(idle.turns, 1);
    assert!(idle.session.is_some());
    assert_eq!(idle.model.as_deref(), Some("fake-model"));
    let with_context = client
        .summary_with(agent, |summary| summary.context_percent.is_some())
        .await;
    assert_eq!(with_context.context_percent, Some(2));

    let items = client.history(agent).await;
    let kinds: Vec<&str> = items
        .iter()
        .map(|item| match &item.body {
            ItemBody::UserMessage(_) => "user",
            ItemBody::AssistantText { .. } => "text",
            ItemBody::TurnEnd(_) => "end",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "text", "end"], "{items:?}");
    let ResponseBody::Details(details) = client
        .ok(Request::AgentDetails(AgentParams { agent }))
        .await
    else {
        panic!("expected details");
    };
    let commands: Vec<(&str, CommandKind)> = details
        .commands
        .iter()
        .map(|command| (command.name.as_str(), command.kind))
        .collect();
    assert_eq!(
        commands,
        [
            ("review", CommandKind::Command),
            ("tidy", CommandKind::Skill)
        ]
    );
    assert_eq!(details.context.map(|context| context.tokens), Some(4000));

    let (_, catchup) = setup.connect_after(None).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot");
    };
    assert_eq!(snapshot.agents.len(), 1);
    assert!(
        snapshot
            .plan_usage
            .windows
            .values()
            .any(|window| window.utilization == Some(0.25))
    );
}

#[tokio::test]
async fn a_start_reads_its_title_as_a_rename_does() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let start = |title: Option<&str>| {
        Request::AgentStart(StartParams {
            folder: setup.work(),
            title: title.map(str::to_owned),
            model: None,
            effort: None,
            mode: None,
            prompt: Some(text("Fix the cart\nand its test")),
        })
    };

    let blank = client.error_code(start(Some(" \n "))).await;
    let ResponseBody::AgentStarted { agent: titled } =
        client.ok(start(Some("  Cart \n total "))).await
    else {
        panic!("expected AgentStarted");
    };
    let named = client.summary_with(titled, |_| true).await;
    let ResponseBody::AgentStarted { agent: untitled } = client.ok(start(None)).await else {
        panic!("expected AgentStarted");
    };
    let unnamed = client.summary_with(untitled, |_| true).await;

    assert_eq!(blank, ErrorCode::InvalidRequest);
    assert_eq!(named.title, "Cart total");
    assert_eq!(unnamed.title, "Fix the cart", "the prompt's first line");
}

#[tokio::test]
async fn a_new_agent_shows_its_mcp_servers_and_context_before_any_prompt() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let params = StartParams {
        folder: setup.work(),
        title: Some("Waits".to_owned()),
        model: None,
        effort: None,
        mode: None,
        prompt: None,
    };
    let ResponseBody::AgentStarted { agent } = client.ok(Request::AgentStart(params)).await else {
        panic!("expected AgentStarted");
    };

    // The fake answers in order: the MCP status before the context usage.
    let with_context = client
        .summary_with(agent, |summary| summary.context_percent.is_some())
        .await;
    let ResponseBody::Details(details) = client
        .ok(Request::AgentDetails(AgentParams { agent }))
        .await
    else {
        panic!("expected details");
    };

    assert_eq!(with_context.context_percent, Some(2));
    assert_eq!(with_context.turns, 0);
    let servers: Vec<(&str, McpServerStatus)> = details
        .mcp_servers
        .iter()
        .map(|server| (server.name.as_str(), server.status))
        .collect();
    assert_eq!(servers, [("notes", McpServerStatus::Connected)]);
}

#[tokio::test]
async fn an_agent_is_renamed_in_its_record_and_in_its_claude_session() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "hello there").await;
    client.idle(agent).await;
    let rename = |title: &str| {
        Request::AgentRename(RenameParams {
            agent,
            title: title.to_owned(),
        })
    };

    assert_eq!(
        client.ok(rename("  Cart   total ")).await,
        ResponseBody::Done
    );

    client
        .summary_with(agent, |summary| summary.title == "Cart total")
        .await;
    client
        .item_with(agent, |body| {
            matches!(body, ItemBody::Notice { text } if text == "Session renamed to: Cart total")
        })
        .await;
    assert_eq!(
        client.error_code(rename(" \n ")).await,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        client.error_code(rename(&"x".repeat(201))).await,
        ErrorCode::InvalidRequest
    );
    let saved = std::fs::read_to_string(setup.state().join("agents.json")).unwrap();
    assert!(saved.contains("\"Cart total\""), "{saved}");
    let users = client
        .history(agent)
        .await
        .into_iter()
        .filter(|item| matches!(item.body, ItemBody::UserMessage(_)))
        .count();
    assert_eq!(users, 1, "the rename prompt shows no item");
}

#[tokio::test]
async fn a_rename_made_while_the_agent_is_stopped_outlives_its_resume() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "hello there").await;
    client.idle(agent).await;
    let rename = |title: &str| {
        Request::AgentRename(RenameParams {
            agent,
            title: title.to_owned(),
        })
    };
    client.ok(rename("Cart total")).await;
    client
        .item_with(agent, |body| {
            matches!(body, ItemBody::Notice { text } if text == "Session renamed to: Cart total")
        })
        .await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;

    client.ok(rename("Checkout")).await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    // The fake restates the session's old name before it answers `initialize`: the daemon then
    // gives the session the agent's name.
    let renamed_session = |body: &ItemBody| matches!(body, ItemBody::Notice { text } if text == "Session renamed to: Checkout");
    client.item_with(agent, renamed_session).await;
    client.prompt(agent, "after the resume").await;
    client.says(agent, "Echo: after the resume").await;
    let idle = client.idle(agent).await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    client.prompt(agent, "after a second resume").await;
    client.says(agent, "Echo: after a second resume").await;

    assert_eq!(idle.title, "Checkout");
    let saved = std::fs::read_to_string(setup.state().join("agents.json")).unwrap();
    assert!(saved.contains("\"Checkout\""), "{saved}");
    let renames = client
        .history(agent)
        .await
        .into_iter()
        .filter(|item| renamed_session(&item.body))
        .count();
    assert_eq!(renames, 1, "a session with the agent's name gets no rename");
}

#[tokio::test]
async fn approvals_allow_and_deny() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "approve this").await;

    let waiting = client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;
    let ItemBody::Approval(approval) = &waiting.body else {
        panic!("expected an approval, got {waiting:?}");
    };
    assert_eq!(approval.tool, "Bash");
    assert_eq!(approval.summary, "touch made-by-fake");
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::NeedsYou)
        .await;
    let allow = Answer::Approval(ApprovalAnswer {
        decision: ApprovalDecision::Allow,
        message: None,
    });
    client
        .answer(agent, approval.pending.clone(), allow.clone())
        .await;
    client.says(agent, "Allowed.").await;
    client.idle(agent).await;

    client.prompt(agent, "approve that").await;
    let waiting = client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;
    let pending = pending_of(&waiting.body).unwrap();
    let deny = Answer::Approval(ApprovalAnswer {
        decision: ApprovalDecision::Deny,
        message: Some("Not now".to_owned()),
    });
    client.answer(agent, pending.clone(), deny).await;
    client.says(agent, "Denied: Not now").await;

    let code = client
        .error_code(Request::AgentAnswer(AnswerParams {
            agent,
            pending,
            answer: allow,
        }))
        .await;
    assert_eq!(code, ErrorCode::NotFound);
}

#[tokio::test]
async fn questions_and_plans_get_their_answers() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "question for you").await;

    let item = client
        .item_with(agent, |body| matches!(body, ItemBody::Question(_)))
        .await;
    let ItemBody::Question(question) = &item.body else {
        panic!("expected a question");
    };
    assert_eq!(question.questions[0].text, "Which color?");
    let wrong_kind = client
        .error_code(Request::AgentAnswer(AnswerParams {
            agent,
            pending: question.pending.clone(),
            answer: Answer::Plan(PlanAnswer {
                decision: PlanDecision::AcceptEdits,
                message: None,
            }),
        }))
        .await;
    assert_eq!(wrong_kind, ErrorCode::InvalidRequest);
    let answers = vec![QuestionAnswer {
        selected: vec!["Blue".to_owned()],
        other: None,
    }];
    client
        .answer(
            agent,
            question.pending.clone(),
            Answer::Question { answers },
        )
        .await;
    client
        .says(agent, r#"Answers: {"Which color?":"Blue"}"#)
        .await;

    client.prompt(agent, "plan the work").await;
    let plan = client
        .item_with(agent, |body| matches!(body, ItemBody::Plan(_)))
        .await;
    let ItemBody::Plan(plan) = plan.body else {
        panic!("expected a plan");
    };
    assert_eq!(plan.markdown, "1. Look\n2. Change");
    let accept = Answer::Plan(PlanAnswer {
        decision: PlanDecision::AcceptEdits,
        message: None,
    });
    client.answer(agent, plan.pending, accept).await;
    client.says(agent, "Plan approved.").await;
    client
        .summary_with(agent, |summary| summary.mode == PermissionMode::AcceptEdits)
        .await;

    client.prompt(agent, "plan more").await;
    let item = client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;
    let keep_planning = Answer::Plan(PlanAnswer {
        decision: PlanDecision::KeepPlanning,
        message: Some("Add tests".to_owned()),
    });
    client
        .answer(agent, pending_of(&item.body).unwrap(), keep_planning)
        .await;
    client.says(agent, "Keep planning: Add tests").await;
}

#[tokio::test]
async fn an_interrupt_cancels_what_waits_and_queued_prompts_follow() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "approve this").await;
    let waiting = client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;

    client.prompt(agent, "hello queued").await;
    let queued = client
        .item_with(agent, |body| {
            matches!(
                body,
                ItemBody::UserMessage(message) if message.queued && message.text == "hello queued"
            )
        })
        .await;
    client.prompt(agent, "drop me").await;
    let dropped = client
        .item_with(
            agent,
            |body| matches!(body, ItemBody::UserMessage(message) if message.text == "drop me"),
        )
        .await;
    let cancel = CancelQueuedParams {
        agent,
        item: dropped.id,
    };
    assert_eq!(
        client.ok(Request::AgentCancelQueued(cancel)).await,
        ResponseBody::Done
    );
    let notice = client
        .item_with(agent, |body| matches!(body, ItemBody::Notice { .. }))
        .await;
    assert_eq!(notice.id, dropped.id);

    assert_eq!(
        client
            .ok(Request::AgentInterrupt(AgentParams { agent }))
            .await,
        ResponseBody::Done
    );

    let cancelled = client
        .item_with(agent, |body| {
            matches!(
                body,
                ItemBody::Approval(approval) if approval.answer == AnswerState::Cancelled
            )
        })
        .await;
    assert_eq!(cancelled.id, waiting.id);
    let end = client
        .item_with(agent, |body| matches!(body, ItemBody::TurnEnd(_)))
        .await;
    let ItemBody::TurnEnd(end) = end.body else {
        panic!("expected a turn end");
    };
    assert!(end.stopped);
    assert_eq!(end.error, None);
    let sent = client
        .item_with(agent, |body| {
            matches!(
                body,
                ItemBody::UserMessage(message) if !message.queued && message.text == "hello queued"
            )
        })
        .await;
    assert_eq!(sent.id, queued.id);
    client.says(agent, "Echo: hello queued").await;
    let history = client.history(agent).await;
    let users = history
        .iter()
        .filter(|item| matches!(item.body, ItemBody::UserMessage(_)))
        .count();
    assert_eq!(users, 2, "the echo of a sent prompt adds no item");
    let notices: Vec<ItemId> = history
        .iter()
        .filter(|item| matches!(item.body, ItemBody::Notice { .. }))
        .map(|item| item.id)
        .collect();
    assert_eq!(
        notices,
        [dropped.id],
        "the turn end says the user stopped the turn"
    );
}

#[tokio::test]
async fn a_restarted_daemon_resumes_its_agents_and_replays_or_snapshots() {
    let mut setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "hello").await;
    client.says(agent, "Echo: hello").await;
    let idle = client.idle(agent).await;
    let session = idle.session.clone().unwrap();
    let seen = client.last_seq.unwrap();
    drop(client);

    setup.restart();

    let (mut replayed, catchup) = setup.connect_after(Some(seen)).await;
    assert_eq!(catchup, Catchup::Replay);
    let first = replayed.next_event().await;
    assert_eq!(first.seq, seen.next().unwrap(), "the numbering continues");
    let (snapshot_client, catchup) = setup.connect_after(None).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot");
    };
    let summary = &snapshot.agents[0];
    assert_eq!(summary.session.as_ref(), Some(&session));
    assert_eq!(summary.turns, 1);
    assert_eq!(summary.status, AgentStatus::Idle);
    let launches = setup.claude_args();
    assert_eq!(launches.len(), 2);
    assert!(
        launches[1].contains(&format!("--resume={}", session.as_str())),
        "{launches:?}"
    );
    assert!(
        !launches[0].contains(&RESUME_INTERRUPTED_TURN.to_owned()),
        "a new agent has no turn to carry on: {launches:?}"
    );
    assert!(
        launches[1].contains(&RESUME_INTERRUPTED_TURN.to_owned()),
        "the daemon's restart cut the turn off: {launches:?}"
    );
    drop(snapshot_client);

    replayed.prompt(agent, "after the restart").await;
    replayed.says(agent, "Echo: after the restart").await;
    assert_eq!(
        replayed.ok(Request::AgentStop(AgentParams { agent })).await,
        ResponseBody::Done
    );
    replayed
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    let before_removal = replayed.last_seq;
    drop(replayed);

    setup.restart();

    let mut client = setup.connect().await;
    assert_eq!(
        setup.claude_args().len(),
        2,
        "a stopped agent stays stopped"
    );
    let code = client
        .error_code(Request::AgentPrompt(PromptParams {
            agent,
            prompt: text("hello"),
        }))
        .await;
    assert_eq!(code, ErrorCode::AgentExited);
    assert_eq!(
        client.ok(Request::AgentResume(AgentParams { agent })).await,
        ResponseBody::Done
    );
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Idle)
        .await;
    let launches = setup.wait_for_launches(3);
    assert!(
        launches[2].contains(&format!("--resume={}", session.as_str())),
        "{launches:?}"
    );
    assert!(
        !launches[2].contains(&RESUME_INTERRUPTED_TURN.to_owned()),
        "a user's stop cut the turn off: {launches:?}"
    );
    let code = client
        .error_code(Request::AgentRemove(AgentParams { agent }))
        .await;
    assert_eq!(code, ErrorCode::AgentRunning);
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    assert_eq!(
        client.ok(Request::AgentRemove(AgentParams { agent })).await,
        ResponseBody::Done
    );
    client
        .wait_for(|event| {
            matches!(event, Event::AgentRemoved { agent: removed } if *removed == agent)
                .then_some(())
        })
        .await;

    // The removed agent's log is gone, so a client that saw less gets a snapshot.
    let (_, catchup) = setup.connect_after(before_removal).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot, as the logs no longer hold every event");
    };
    assert!(snapshot.agents.is_empty());
}

#[tokio::test]
async fn a_second_daemon_is_refused_by_the_lock() {
    let mut setup = Setup::new();

    let output = setup.serve_command().output().unwrap();

    assert!(!output.status.success());
    let log = std::fs::read_to_string(setup.temp.path().join("daemon-2.log")).unwrap();
    assert!(log.contains("another daemon already runs"), "{log}");
    // The first daemon still serves.
    setup.connect().await;
}

/// A `claude` that does not exit once its stdin closes is killed after the stop timeout, and every
/// process it started goes with it.
#[tokio::test]
async fn a_stopped_agent_that_hangs_is_killed_with_its_children() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "linger").await;
    let started = client
        .item_with(agent, |body| {
            matches!(body, ItemBody::AssistantText { markdown } if markdown.starts_with("Started child "))
        })
        .await;
    let ItemBody::AssistantText { markdown } = started.body else {
        panic!("expected the child's pid");
    };
    let child: i32 = markdown["Started child ".len()..].parse().unwrap();
    client.idle(agent).await;

    client.ok(Request::AgentStop(AgentParams { agent })).await;

    // The daemon kills `claude` 10 seconds after closing its stdin.
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    let deadline = Instant::now() + WAIT;
    while is_running(child) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let survived = is_running(child);
    if survived {
        let pid = Pid::from_raw(child).unwrap();
        rustix::process::kill_process(pid, Signal::KILL).unwrap();
    }
    assert!(!survived, "the child of the killed claude still runs");
}

/// Whether a process runs: it exists and is not a zombie waiting for its parent.
fn is_running(pid: i32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // `pid (name) state ...`: the name may hold spaces and parentheses.
    let state = stat
        .rfind(')')
        .and_then(|end| stat[end + 1..].split_whitespace().next());
    state != Some("Z")
}

/// `claude`, and so the commands the agents run, keep the user's umask, here the most open one.
/// The state folder stays private through the modes the daemon gives its files.
#[tokio::test]
async fn claude_keeps_the_users_umask_and_the_state_stays_private() {
    use std::os::unix::fs::MetadataExt;

    let setup = Setup::new_with_umask(Some("000"));
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "hello").await;
    client.says(agent, "Echo: hello").await;
    let idle = client.idle(agent).await;

    // The fake `claude` names its session after its pid.
    let session = idle.session.unwrap();
    let pid = session.as_str().strip_prefix("fake-session-").unwrap();
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let umask = status
        .lines()
        .find_map(|line| line.strip_prefix("Umask:"))
        .map(str::trim);
    assert_eq!(umask, Some("0000"));
    let mut folders = vec![setup.state()];
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(folder) = folders.pop() {
        let mode = std::fs::metadata(&folder).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", folder.display());
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                folders.push(path);
                continue;
            }
            assert_eq!(metadata.mode() & 0o777, 0o600, "{}", path.display());
            files.push(path.strip_prefix(setup.state()).unwrap().to_owned());
        }
    }
    files.sort();
    let expected = [
        "agents-cockpit.sock",
        "agents.json",
        "events/1.jsonl",
        "events/daemon.jsonl",
        "raw/1.jsonl",
        "serve.lock",
    ];
    assert_eq!(files, expected.map(PathBuf::from));
}

#[tokio::test]
async fn malformed_requests_get_an_error_and_the_connection_stays() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let setup = Setup::new();
    let stream = tokio::net::UnixStream::connect(setup.socket())
        .await
        .unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut next_line = async || -> Value {
        let line = tokio::time::timeout(WAIT, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    };

    write
        .write_all(b"{\"type\":\"hello\",\"protocol_version\":1,\"last_seq\":null}\n")
        .await
        .unwrap();
    assert_eq!(next_line().await["type"], "welcome");
    write
        .write_all(b"{\"type\":\"request\",\"id\":5,\"request\":{\"method\":\"agent.fly\"}}\nnot json at all\n")
        .await
        .unwrap();
    let error = next_line().await;
    write
        .write_all(b"{\"type\":\"request\",\"id\":6,\"request\":{\"method\":\"agent.details\",\"params\":{\"agent\":99}}}\n")
        .await
        .unwrap();
    let not_found = next_line().await;

    assert_eq!(error["id"], 5);
    assert_eq!(error["result"]["error"]["code"], "invalid_request");
    assert_eq!(not_found["id"], 6);
    assert_eq!(not_found["result"]["error"]["code"], "not_found");

    let stream = tokio::net::UnixStream::connect(setup.socket())
        .await
        .unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"{\"type\":\"hello\",\"protocol_version\":999,\"last_seq\":null}\n")
        .await
        .unwrap();
    let refused = lines.next_line().await.unwrap().unwrap();
    assert!(refused.contains("\"type\":\"refused\""), "{refused}");
}

#[tokio::test]
async fn files_settings_sessions_and_agent_settings() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    std::fs::write(setup.work().join("notes.txt"), "remember the milk\n").unwrap();
    let agent = client.start(&setup.work(), "garbage first").await;
    client.says(agent, "Still here.").await;
    client.idle(agent).await;

    let read = |path: &str| {
        Request::AgentReadFile(ReadFileParams {
            agent,
            path: PathBuf::from(path),
        })
    };
    assert_eq!(
        client.ok(read("notes.txt")).await,
        ResponseBody::File {
            text: "remember the milk\n".to_owned()
        }
    );
    assert_eq!(
        client.error_code(read("../state/agents.json")).await,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        client.error_code(read("missing.txt")).await,
        ErrorCode::NotFound
    );

    let settings = Settings {
        hidden_mcp_servers: BTreeSet::from(["notes".to_owned()]),
    };
    client.ok(Request::SettingsSet(settings.clone())).await;
    let changed = client
        .wait_for(|event| match event {
            Event::SettingsChanged(changed) => Some(changed.clone()),
            _ => None,
        })
        .await;
    assert_eq!(changed, settings);

    let ResponseBody::McpServers { servers } = client
        .ok(Request::AgentMcpStatus(AgentParams { agent }))
        .await
    else {
        panic!("expected MCP servers");
    };
    assert_eq!(servers[0].name, "notes");
    assert!(servers[0].tools[0].read_only);
    let mcp_server = |enabled| {
        Request::AgentMcpToggle(McpToggleParams {
            agent,
            server: "notes".to_owned(),
            enabled,
        })
    };
    assert_eq!(client.ok(mcp_server(false)).await, ResponseBody::Done);
    let reconnect = Request::AgentMcpReconnect(McpServerParams {
        agent,
        server: "notes".to_owned(),
    });
    assert_eq!(client.ok(reconnect).await, ResponseBody::Done);
    let stop_task = Request::AgentStopTask(StopTaskParams {
        agent,
        task: BackgroundTaskId::new("task-1".to_owned()),
    });
    assert_eq!(client.ok(stop_task).await, ResponseBody::Done);
    let remove = |scope| {
        Request::McpRemove(McpRemoveParams {
            agent,
            server: "notes".to_owned(),
            scope,
        })
    };
    assert_eq!(
        client.ok(remove(McpScope::Project)).await,
        ResponseBody::Done
    );
    assert_eq!(
        client.error_code(remove(McpScope::Managed)).await,
        ErrorCode::InvalidRequest
    );
    let ResponseBody::Commands { commands } = client
        .ok(Request::AgentCommands(AgentParams { agent }))
        .await
    else {
        panic!("expected the commands");
    };
    assert_eq!(commands.len(), 2);
    let ResponseBody::ContextUsage(usage) = client
        .ok(Request::AgentContextUsage(AgentParams { agent }))
        .await
    else {
        panic!("expected the context usage");
    };
    assert_eq!(usage.percent, 2);

    client
        .ok(Request::AgentSetModel(SetModelParams {
            agent,
            model: "-p".to_owned(),
        }))
        .await;
    client
        .ok(Request::AgentSetMode(SetModeParams {
            agent,
            mode: PermissionMode::Plan,
        }))
        .await;
    client
        .ok(Request::AgentSetEffort(SetEffortParams {
            agent,
            effort: Effort::High,
        }))
        .await;
    let summary = client
        .summary_with(agent, |summary| summary.effort == Some(Effort::High))
        .await;
    assert_eq!(summary.model.as_deref(), Some("-p"));
    assert_eq!(summary.mode, PermissionMode::Plan);
    client.idle(agent).await;
    let notices = client
        .history(agent)
        .await
        .into_iter()
        .filter(|item| {
            matches!(
                &item.body,
                ItemBody::UserMessage(message) if message.text.starts_with("/effort")
            )
        })
        .count();
    assert_eq!(notices, 0, "the effort prompt shows no item");

    let project = setup.temp.path().join("claude/projects/-srv-shop");
    std::fs::create_dir_all(&project).unwrap();
    let session_line = serde_json::json!({
        "type": "user", "cwd": setup.work(),
        "message": {"role": "user", "content": "Fix the old bug"}
    });
    let cost_line = serde_json::json!({
        "type": "cost-state", "sessionId": SESSION_ID, "totalCostUSD": 0.5
    });
    std::fs::write(
        project.join(format!("{SESSION_ID}.jsonl")),
        format!("{session_line}\n{cost_line}\n"),
    )
    .unwrap();
    let ResponseBody::Sessions { sessions } = client.ok(Request::SessionsList).await else {
        panic!("expected sessions");
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "Fix the old bug");
    let resumed = client
        .ok(Request::SessionsResume(SessionResumeParams {
            session: ClaudeSessionId::new(SESSION_ID.to_owned()),
        }))
        .await;
    let ResponseBody::AgentStarted { agent: second } = resumed else {
        panic!("expected AgentStarted, got {resumed:?}");
    };
    assert_ne!(second, agent);
    let restored = client
        .summary_with(second, |summary| summary.cost == CostUsd::new(0.5))
        .await;
    assert_eq!(
        restored.turns, 0,
        "the session's past cost shows before any turn"
    );
    let past = client.history(second).await;
    let ids: Vec<u32> = past.iter().map(|item| item.id.get()).collect();
    assert_eq!(ids, [2001], "{past:?}");
    assert!(
        matches!(&past[0].body, ItemBody::UserMessage(message) if message.text == "Fix the old bug")
    );
    let again = client
        .ok(Request::SessionsResume(SessionResumeParams {
            session: ClaudeSessionId::new(SESSION_ID.to_owned()),
        }))
        .await;
    assert_eq!(again, ResponseBody::AgentStarted { agent: second });
    assert_eq!(
        client.history(second).await,
        past,
        "the agent that has the session keeps its items"
    );
    let launches = setup.wait_for_launches(2);
    let resumed_launch = launches
        .iter()
        .find(|args| args.contains(&format!("--resume={SESSION_ID}")))
        .unwrap();
    assert!(
        !resumed_launch.contains(&RESUME_INTERRUPTED_TURN.to_owned()),
        "a past session's last turn stays as it ended: {resumed_launch:?}"
    );
    let missing = client
        .error_code(Request::SessionsResume(SessionResumeParams {
            session: ClaudeSessionId::new("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned()),
        }))
        .await;
    assert_eq!(missing, ErrorCode::NotFound);
}

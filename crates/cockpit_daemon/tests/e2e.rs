//! The daemon end to end: `test-serve` in its own process, the fake `claude`, and clients over the
//! socket.

#![cfg(unix)]
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
    Account, AccountAddParams, AccountId, AccountParams, AccountPlanUsage, AccountState, AgentId,
    AgentParams, AgentStatus, AgentSummary, Answer, AnswerParams, AnswerState, Approval,
    ApprovalAnswer, ApprovalDecision, BackgroundTaskId, Catchup, ClaudeSessionId, CommandChanges,
    CommandKind, CostUsd, CredentialSource, DetailsChange, EditTexts, Effort, EffortSource,
    EnvironmentCredentials, ErrorCode, Event, EventRecord, FileEdit, HistoryParams, Image,
    ImageMediaType, ItemBody, McpRemoveParams, McpScope, McpServerParams, McpServerStatus,
    McpToggleAction, McpToggleParams, MoveParams, PROTOCOL_VERSION, PendingId, PermissionMode,
    Plan, PlanAnswer, PlanDecision, PlanUsage, ProcessId, Prompt, PromptDelivery, PromptParams,
    Question, QuestionAnswer, QueuedPromptParams, ReadFileParams, RenameParams, Request,
    ResponseBody, Seq, SessionResumeParams, SetEffortParams, SetModeParams, SetModelParams,
    Settings, SignIn, SignInWait, StartParams, StopProcessParams, StopTaskParams,
    TerminalInputParams, TerminalOpenParams, TerminalParams, TerminalSize, TranscriptItem,
    TurnOutcome, UsageWindow, WaitingPrompt, WaitingState,
};
use rustix::process::{Pid, Signal};
use serde_json::{Value, json};
use tempfile::TempDir;

const WAIT: Duration = Duration::from_secs(15);
const SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";
/// The file in an account's folder whose presence says it is signed in.
const CREDENTIALS_FILE: &str = ".credentials.json";
/// The fake `claude` records this first on its command line when it should carry on with a turn
/// that was cut off.
const RESUME_INTERRUPTED_TURN: &str = "CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1";

/// The variables that carry a login, which the daemon's environment has only when a test sets
/// them.
const LOGIN_VARIABLES: [&str; 4] = [
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

/// Where the daemon's default account keeps its Claude Code folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefaultFolder {
    /// `claude` in the test's folder, which the daemon's `CLAUDE_CONFIG_DIR` names.
    Variable,
    /// `.claude` in a home folder of the test's, without `CLAUDE_CONFIG_DIR`.
    Home,
}

/// A state folder, an agent folder and a Claude folder, and the daemon serving them.
struct Setup {
    temp: TempDir,
    daemon: Option<Child>,
    starts: u32,
    /// The umask the daemon starts with, as `umask` takes it; `None` keeps the test's.
    umask: Option<&'static str>,
    default_folder: DefaultFolder,
    /// Variables the daemon's environment has, beyond those every test sets.
    env: Vec<(&'static str, &'static str)>,
}

impl Setup {
    fn new() -> Self {
        Self::new_with_umask(None)
    }

    fn new_with_umask(umask: Option<&'static str>) -> Self {
        Self::new_with(umask, DefaultFolder::Variable, Vec::new())
    }

    fn new_with(
        umask: Option<&'static str>,
        default_folder: DefaultFolder,
        env: Vec<(&'static str, &'static str)>,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        for folder in ["state", "work", "claude", "home/.claude"] {
            std::fs::create_dir_all(temp.path().join(folder)).unwrap();
        }
        let mut setup = Self {
            temp,
            daemon: None,
            starts: 0,
            umask,
            default_folder,
            env,
        };
        setup.start();
        setup
    }

    /// The default account's Claude Code folder.
    fn default_folder(&self) -> PathBuf {
        match self.default_folder {
            DefaultFolder::Variable => self.temp.path().join("claude"),
            DefaultFolder::Home => self.temp.path().join("home/.claude"),
        }
    }

    fn env_file(&self) -> PathBuf {
        self.temp.path().join("claude-env.jsonl")
    }

    /// The login variables of every `claude` started so far, once `count` of them have started.
    fn wait_for_envs(&self, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let text = std::fs::read_to_string(self.env_file()).unwrap_or_default();
            let envs: Vec<Value> = text
                .split_inclusive('\n')
                .filter_map(|line| line.strip_suffix('\n'))
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if envs.len() >= count {
                return envs;
            }
            assert!(Instant::now() < deadline, "claude did not start: {envs:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
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

    /// The name `/rename` gave `session`, as the fake keeps it.
    fn session_name(&self, session: Option<&ClaudeSessionId>) -> Option<String> {
        let session = session?;
        std::fs::read_to_string(
            self.temp
                .path()
                .join("session-names")
                .join(session.as_str()),
        )
        .ok()
        .map(|name| name.trim().to_owned())
    }

    /// Waits until `/rename` gave `session` the name `title`.
    async fn wait_for_session_name(&self, session: Option<&ClaudeSessionId>, title: &str) {
        for _ in 0..200 {
            if self.session_name(session).as_deref() == Some(title) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the session never took the name {title}");
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
            .env("FAKE_CLAUDE_ARGS_FILE", self.args_file())
            .env("FAKE_CLAUDE_ENV_FILE", self.env_file())
            .env(
                "FAKE_CLAUDE_SESSION_NAMES",
                self.temp.path().join("session-names"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        for name in LOGIN_VARIABLES {
            command.env_remove(name);
        }
        match self.default_folder {
            DefaultFolder::Variable => {
                command.env("CLAUDE_CONFIG_DIR", self.default_folder());
            }
            DefaultFolder::Home => {
                command.env("HOME", self.temp.path().join("home"));
            }
        }
        command.envs(self.env.iter().copied());
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
        // The last line may still be being written.
        text.split_inclusive('\n')
            .filter_map(|line| line.strip_suffix('\n'))
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
        let (connection, welcome) = Connection::connect(&transport, last_seq).await.unwrap();
        let catchup = welcome.catchup;
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
        self.start_with(folder, prompt, AccountId::DEFAULT).await
    }

    async fn start_with(&mut self, folder: &Path, prompt: &str, account: AccountId) -> AgentId {
        let params = StartParams {
            folder: folder.to_owned(),
            title: None,
            model: None,
            effort: None,
            mode: None,
            prompt: Some(text(prompt)),
            account,
        };
        match self.ok(Request::AgentStart(params)).await {
            ResponseBody::AgentStarted { agent } => agent,
            other => panic!("expected AgentStarted, got {other:?}"),
        }
    }

    async fn prompt(&self, agent: AgentId, prompt: &str) {
        self.prompt_with(agent, prompt, PromptDelivery::Steer).await;
    }

    async fn prompt_with(&self, agent: AgentId, prompt: &str, delivery: PromptDelivery) {
        let params = PromptParams {
            agent,
            prompt: text(prompt),
            delivery,
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

    /// Waits for the agent's waiting prompts to match, and gives them.
    async fn waiting_with(
        &mut self,
        agent: AgentId,
        mut matches: impl FnMut(&[WaitingPrompt]) -> bool,
    ) -> Vec<WaitingPrompt> {
        self.wait_for(|event| {
            let waiting = match event {
                Event::DetailsChanged {
                    agent: from,
                    details,
                } if *from == agent => &details.waiting_prompts,
                Event::DetailsUpdated {
                    agent: from,
                    changes,
                } if *from == agent => changes.iter().find_map(|change| match change {
                    DetailsChange::WaitingPrompts { waiting_prompts } => Some(waiting_prompts),
                    _ => None,
                })?,
                _ => return None,
            };
            matches(waiting).then(|| waiting.clone())
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

    async fn add_account(
        &self,
        name: &str,
        folder: Option<&Path>,
        credentials: CredentialSource,
    ) -> AccountState {
        let params = AccountAddParams {
            name: name.to_owned(),
            folder: folder.map(Path::to_owned),
            credentials,
        };
        match self.ok(Request::AccountsAdd(params)).await {
            ResponseBody::Account(state) => state,
            other => panic!("expected an account, got {other:?}"),
        }
    }

    async fn accounts(&self) -> Vec<AccountState> {
        match self.ok(Request::AccountsList).await {
            ResponseBody::Accounts { accounts, .. } => accounts,
            other => panic!("expected the accounts, got {other:?}"),
        }
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

/// The notice the fake writes once it gives the Claude session `title`.
fn session_renamed(body: &ItemBody, title: &str) -> bool {
    matches!(body, ItemBody::Notice { text, .. } if text.strip_prefix("Session renamed to: ") == Some(title))
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
    let ResponseBody::Details { details, .. } = client
        .ok(Request::AgentDetails(AgentParams { agent }))
        .await
    else {
        panic!("expected details");
    };
    let ResponseBody::Commands { commands } = client
        .ok(Request::AgentCommands(AgentParams { agent }))
        .await
    else {
        panic!("expected commands");
    };
    let commands: Vec<(&str, CommandKind)> = commands
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

/// The names and kinds of the commands a `CommandsChanged` event of `agent` carries.
fn changed_commands(event: &Event, agent: AgentId) -> Option<Vec<(String, CommandKind)>> {
    let Event::CommandsChanged {
        agent: changed,
        commands,
    } = event
    else {
        return None;
    };
    let named = commands
        .iter()
        .map(|command| (command.name.clone(), command.kind))
        .collect();
    (*changed == agent).then_some(named)
}

fn named(commands: &[(&str, CommandKind)]) -> Vec<(String, CommandKind)> {
    commands
        .iter()
        .map(|&(name, kind)| (name.to_owned(), kind))
        .collect()
}

#[tokio::test]
async fn a_reload_updates_the_commands_every_client_sees() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "hello").await;
    // A new agent shows as idle before `initialize` lists its commands, which a reload's changes
    // count from.
    client
        .wait_for(|event| changed_commands(event, agent))
        .await;

    let skills = client
        .ok(Request::AgentReloadSkills(AgentParams { agent }))
        .await;
    let after_skills = client
        .wait_for(|event| {
            changed_commands(event, agent).filter(|commands| {
                commands.contains(&("ship-check".to_owned(), CommandKind::Skill))
            })
        })
        .await;
    let plugins = client
        .ok(Request::AgentReloadPlugins(AgentParams { agent }))
        .await;
    let after_plugins = client
        .wait_for(|event| {
            changed_commands(event, agent)
                .filter(|commands| commands.iter().any(|(name, _)| name == "notes:search"))
        })
        .await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    let exited = client
        .error_code(Request::AgentReloadSkills(AgentParams { agent }))
        .await;

    assert_eq!(
        skills,
        ResponseBody::SkillsReloaded {
            skills: 2,
            changes: CommandChanges {
                added: vec!["ship-check".to_owned()],
                removed: Vec::new(),
            },
        }
    );
    assert_eq!(
        after_skills,
        named(&[
            ("review", CommandKind::Command),
            ("tidy", CommandKind::Skill),
            ("ship-check", CommandKind::Skill),
        ])
    );
    assert_eq!(
        plugins,
        ResponseBody::PluginsReloaded {
            plugins: 1,
            errors: 1,
            changes: CommandChanges {
                added: vec!["notes:search".to_owned()],
                removed: Vec::new(),
            },
        }
    );
    assert_eq!(
        after_plugins,
        named(&[
            ("review", CommandKind::Command),
            ("tidy", CommandKind::Skill),
            ("ship-check", CommandKind::Skill),
            ("notes:search", CommandKind::Command),
        ])
    );
    assert_eq!(exited, ErrorCode::AgentExited);
}

#[tokio::test]
async fn an_agent_without_an_effort_shows_the_one_claude_code_applies() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let start = |effort: Option<Effort>| {
        Request::AgentStart(StartParams {
            folder: setup.work(),
            title: None,
            model: None,
            effort,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        })
    };

    let ResponseBody::AgentStarted { agent } = client.ok(start(None)).await else {
        panic!("expected AgentStarted");
    };
    let applied = client
        .summary_with(agent, |summary| summary.effort.is_some())
        .await;
    let set_model = SetModelParams {
        agent,
        model: "other-model".to_owned(),
    };
    client.ok(Request::AgentSetModel(set_model)).await;
    let other_model = client
        .summary_with(agent, |summary| summary.effort == Some(Effort::Low))
        .await;
    let set_effort = SetEffortParams {
        agent,
        effort: Effort::High,
    };
    client.ok(Request::AgentSetEffort(set_effort)).await;
    let given = client
        .summary_with(agent, |summary| summary.effort == Some(Effort::High))
        .await;
    let ResponseBody::AgentStarted { agent: explicit } =
        client.ok(start(Some(Effort::Xhigh))).await
    else {
        panic!("expected AgentStarted");
    };
    // The context usage comes after the settings an agent without an effort asks for.
    let started = client
        .summary_with(explicit, |summary| summary.context_percent.is_some())
        .await;

    assert_eq!(
        (applied.effort, applied.effort_source),
        (Some(Effort::Medium), EffortSource::ClaudeCodeDefault)
    );
    assert_eq!(
        other_model.effort_source,
        EffortSource::ClaudeCodeDefault,
        "the new model's default"
    );
    assert_eq!(given.effort_source, EffortSource::Chosen);
    assert_eq!(
        (started.effort, started.effort_source),
        (Some(Effort::Xhigh), EffortSource::Chosen)
    );
}

#[tokio::test]
async fn a_model_typed_in_the_composer_shows_its_own_default_effort() {
    let setup = Setup::new();
    let mut client = setup.connect().await;

    let agent = client.start(&setup.work(), "first").await;
    let applied = client
        .summary_with(agent, |summary| summary.effort.is_some())
        .await;
    client.prompt(agent, "/model other-model").await;
    client.prompt(agent, "next").await;
    let changed = client
        .summary_with(agent, |summary| summary.effort == Some(Effort::Low))
        .await;

    assert_eq!(applied.effort, Some(Effort::Medium));
    assert_eq!(changed.model.as_deref(), Some("other-model"));
    assert_eq!(changed.effort_source, EffortSource::ClaudeCodeDefault);
}

#[tokio::test]
async fn a_stopped_agent_given_another_model_shows_no_effort_until_it_starts() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "first").await;
    client
        .summary_with(agent, |summary| summary.effort.is_some())
        .await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;

    let set_model = SetModelParams {
        agent,
        model: "other-model".to_owned(),
    };
    client.ok(Request::AgentSetModel(set_model)).await;
    let stopped = client
        .summary_with(agent, |summary| {
            summary.model.as_deref() == Some("other-model")
        })
        .await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    let resumed = client
        .summary_with(agent, |summary| summary.effort.is_some())
        .await;

    assert_eq!(
        (stopped.effort, stopped.effort_source),
        (None, EffortSource::Chosen)
    );
    assert_eq!(
        (resumed.effort, resumed.effort_source),
        (Some(Effort::Low), EffortSource::ClaudeCodeDefault)
    );
}

#[tokio::test]
async fn resuming_an_exited_agent_is_activity_of_the_agent() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "first").await;
    client.idle(agent).await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    let exited = client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    client.ok(Request::AgentResume(AgentParams { agent })).await;
    let resumed = client
        .summary_with(agent, |summary| summary.status != AgentStatus::Exited)
        .await;

    assert!(exited.last_activity_at.is_some());
    assert!(
        resumed.last_activity_at > exited.last_activity_at,
        "{:?} after {:?}",
        resumed.last_activity_at,
        exited.last_activity_at
    );
}

#[tokio::test]
async fn an_effort_typed_in_the_composer_is_kept_as_if_set_from_the_menu() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "first").await;
    client
        .summary_with(agent, |summary| {
            summary.effort_source == EffortSource::ClaudeCodeDefault
        })
        .await;

    client.prompt(agent, "/effort turbo").await;
    client.prompt(agent, "/effort high").await;
    let typed = client
        .summary_with(agent, |summary| {
            summary.effort.is_some() && summary.effort_source == EffortSource::Chosen
        })
        .await;
    client.idle(agent).await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    let launches = setup.wait_for_launches(2);

    assert_eq!(typed.effort, Some(Effort::High));
    assert!(
        launches[1].contains(&"--effort=high".to_owned()),
        "{launches:?}"
    );
}

#[tokio::test]
async fn a_model_typed_in_the_composer_is_kept_by_a_resume() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "first").await;

    client.prompt(agent, "/model other-model").await;
    client
        .item_with(
            agent,
            |body| matches!(body, ItemBody::Notice { text, .. } if text == "Model set"),
        )
        .await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    let launches = setup.wait_for_launches(2);
    // The fake applies `low` to any model but its own.
    let resumed = client
        .summary_with(agent, |summary| summary.effort == Some(Effort::Low))
        .await;

    assert!(
        launches[1].contains(&"--model=other-model".to_owned()),
        "{launches:?}"
    );
    assert_eq!(resumed.effort_source, EffortSource::ClaudeCodeDefault);
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
            account: AccountId::DEFAULT,
        })
    };

    let blank = client.error_code(start(Some(" \n "))).await;
    let ResponseBody::AgentStarted { agent: titled } =
        client.ok(start(Some("  Cart \n total "))).await
    else {
        panic!("expected AgentStarted");
    };
    let named = client
        .summary_with(titled, |summary| {
            summary.turns == 1 && summary.status == AgentStatus::Idle
        })
        .await;
    let ResponseBody::AgentStarted { agent: untitled } = client.ok(start(None)).await else {
        panic!("expected AgentStarted");
    };
    let unnamed = client.summary_with(untitled, |_| true).await;
    let notices = client
        .history(titled)
        .await
        .iter()
        .filter(|item| session_renamed(&item.body, "Cart total"))
        .count();

    assert_eq!(blank, ErrorCode::InvalidRequest);
    assert_eq!(named.title, "Cart total");
    assert_eq!(unnamed.title, "Fix the cart", "the prompt's first line");
    // The Claude session takes the name too, with no notice: the user renamed nothing.
    assert_eq!(
        setup.session_name(named.session.as_ref()).as_deref(),
        Some("Cart total")
    );
    assert_eq!(notices, 0);
}

/// The statuses of an agent started with a name, each once in a row, until it answers a prompt
/// sent once the session took the name.
async fn named_start_statuses(prompt: Option<&str>) -> Vec<AgentStatus> {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let params = StartParams {
        folder: setup.work(),
        title: Some("Cart total".to_owned()),
        model: None,
        effort: None,
        mode: None,
        prompt: prompt.map(text),
        account: AccountId::DEFAULT,
    };
    let ResponseBody::AgentStarted { agent } = client.ok(Request::AgentStart(params)).await else {
        panic!("expected AgentStarted");
    };
    let turns = if prompt.is_some() { 2 } else { 1 };
    let mut statuses = Vec::new();
    let mut note = |event: &Event| {
        if let Event::AgentAdded(summary) | Event::AgentUpdated(summary) = event
            && summary.id == agent
            && statuses.last() != Some(&summary.status)
        {
            statuses.push(summary.status);
        }
    };
    let first_turns = turns - 1;
    let ready = client
        .wait_for(|event| {
            note(event);
            match event {
                Event::AgentAdded(summary) | Event::AgentUpdated(summary)
                    if summary.id == agent
                        && summary.turns == first_turns
                        && summary.status == AgentStatus::Idle
                        && summary.session.is_some() =>
                {
                    Some(summary.session.clone())
                }
                _ => None,
            }
        })
        .await;
    setup
        .wait_for_session_name(ready.as_ref(), "Cart total")
        .await;
    client.prompt(agent, "ping").await;
    client
        .wait_for(|event| {
            note(event);
            matches!(event, Event::AgentUpdated(summary)
                if summary.id == agent && summary.turns == turns && summary.status == AgentStatus::Idle)
            .then_some(())
        })
        .await;
    statuses
}

#[tokio::test]
async fn the_name_a_start_gives_the_session_shows_no_turn() {
    use AgentStatus::{Idle, Working};

    assert_eq!(
        named_start_statuses(Some("hello there")).await,
        [Idle, Working, Idle, Working, Idle],
        "the first turn, then the ping's"
    );
    assert_eq!(
        named_start_statuses(None).await,
        [Idle, Working, Idle],
        "the ping's turn alone"
    );
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
        account: AccountId::DEFAULT,
    };
    let ResponseBody::AgentStarted { agent } = client.ok(Request::AgentStart(params)).await else {
        panic!("expected AgentStarted");
    };

    // The fake answers in order: the MCP status before the context usage.
    let with_context = client
        .summary_with(agent, |summary| summary.context_percent.is_some())
        .await;
    let ResponseBody::Details { details, .. } = client
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
        .item_with(agent, |body| session_renamed(body, "Cart total"))
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
        .item_with(agent, |body| session_renamed(body, "Cart total"))
        .await;
    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;

    client.ok(rename("Checkout")).await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    // The fake restates the session's old name before it answers `initialize`: the daemon then
    // gives the session the agent's name.
    let renamed_session = |body: &ItemBody| session_renamed(body, "Checkout");
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
    // A second press of Y before the app sees the approval answered.
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
    assert_eq!(code, ErrorCode::InvalidRequest, "the card holds the denial");
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
            Answer::Question {
                answers: answers.clone(),
            },
        )
        .await;
    client
        .says(agent, r#"Answers: {"Which color?":"Blue"}"#)
        .await;
    // A second Enter before the app sees the question answered sends the same answer again.
    client
        .answer(
            agent,
            question.pending.clone(),
            Answer::Question { answers },
        )
        .await;
    let changed_mind = client
        .error_code(Request::AgentAnswer(AnswerParams {
            agent,
            pending: question.pending.clone(),
            answer: Answer::Question {
                answers: vec![QuestionAnswer {
                    selected: vec!["Red".to_owned()],
                    other: None,
                }],
            },
        }))
        .await;
    assert_eq!(changed_mind, ErrorCode::InvalidRequest);

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
    client
        .answer(agent, plan.pending.clone(), accept.clone())
        .await;
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
async fn an_interrupt_cancels_what_waits_and_waiting_prompts_follow_in_order() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "approve this").await;
    let waiting = client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;

    client.prompt(agent, "hello steered").await;
    client
        .waiting_with(
            agent,
            |prompts| matches!(prompts, [prompt] if prompt.state == WaitingState::Sent),
        )
        .await;
    client
        .prompt_with(agent, "drop me", PromptDelivery::Queue)
        .await;
    let prompts = client
        .waiting_with(agent, |prompts| prompts.len() == 2)
        .await;
    let cancel = QueuedPromptParams {
        agent,
        prompt: prompts[1].id,
    };
    assert_eq!(
        client.ok(Request::AgentCancelQueued(cancel)).await,
        ResponseBody::Done
    );
    client
        .waiting_with(agent, |prompts| prompts.len() == 1)
        .await;
    assert_eq!(
        client.error_code(Request::AgentSendQueued(cancel)).await,
        ErrorCode::NotFound,
        "a cancelled prompt is gone"
    );
    client
        .prompt_with(agent, "then queued", PromptDelivery::Queue)
        .await;
    client
        .prompt_with(agent, "/effort high", PromptDelivery::Steer)
        .await;
    let prompts = client
        .waiting_with(agent, |prompts| prompts.len() == 3)
        .await;
    let states: Vec<WaitingState> = prompts.iter().map(|prompt| prompt.state).collect();
    assert_eq!(
        states,
        [
            WaitingState::Sent,
            WaitingState::Queued,
            WaitingState::Queued
        ],
        "a slash command waits for the turn's end"
    );
    let effort = QueuedPromptParams {
        agent,
        prompt: prompts[2].id,
    };
    assert_eq!(
        client.ok(Request::AgentCancelQueued(effort)).await,
        ResponseBody::Done
    );

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
    assert_eq!(end.outcome, TurnOutcome::Stopped);
    assert_eq!(end.error, None);
    client.says(agent, "Echo: hello steered").await;
    client.says(agent, "Echo: then queued").await;
    client.idle(agent).await;
    let users: Vec<String> = client
        .history(agent)
        .await
        .into_iter()
        .filter_map(|item| match item.body {
            ItemBody::UserMessage(message) => Some(message.text),
            _ => None,
        })
        .collect();
    assert_eq!(users, ["approve this", "hello steered", "then queued"]);
}

#[tokio::test]
async fn a_queued_prompt_sent_now_waits_for_claude_to_read_it() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "approve this").await;
    client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;
    client
        .prompt_with(agent, "hello later", PromptDelivery::Queue)
        .await;
    let prompts = client
        .waiting_with(agent, |prompts| prompts.len() == 1)
        .await;

    let send = QueuedPromptParams {
        agent,
        prompt: prompts[0].id,
    };
    assert_eq!(
        client.ok(Request::AgentSendQueued(send)).await,
        ResponseBody::Done
    );
    client
        .waiting_with(
            agent,
            |prompts| matches!(prompts, [prompt] if prompt.state == WaitingState::Sent),
        )
        .await;
    assert_eq!(
        client
            .ok(Request::AgentInterrupt(AgentParams { agent }))
            .await,
        ResponseBody::Done
    );

    client
        .waiting_with(agent, <[WaitingPrompt]>::is_empty)
        .await;
    client.says(agent, "Echo: hello later").await;
    client.idle(agent).await;
}

#[tokio::test]
async fn a_queued_slash_command_cannot_be_sent_now() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "approve this").await;
    client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;
    client.prompt(agent, "/effort high").await;
    let prompts = client
        .waiting_with(agent, |prompts| prompts.len() == 1)
        .await;

    let send = QueuedPromptParams {
        agent,
        prompt: prompts[0].id,
    };
    assert_eq!(
        client.error_code(Request::AgentSendQueued(send)).await,
        ErrorCode::InvalidRequest
    );
    assert_eq!(
        client.ok(Request::AgentCancelQueued(send)).await,
        ResponseBody::Done,
        "the command still waits in the queue"
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
    assert!(idle.last_activity_at.is_some());
    assert_eq!(
        summary.last_activity_at, idle.last_activity_at,
        "a restart is no activity of the agent"
    );
    assert_eq!(summary.turn_started_at, None);
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
            delivery: PromptDelivery::Steer,
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

/// The agents in the list's order, as a new client sees them.
async fn list_order(setup: &Setup) -> Vec<AgentId> {
    let (_, catchup) = setup.connect_after(None).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot");
    };
    let mut agents = snapshot.agents;
    agents.sort_by(|left, right| cockpit_protocol::list_order(left.place(), right.place()));
    agents.iter().map(|summary| summary.id).collect()
}

#[tokio::test]
async fn the_list_keeps_the_order_a_client_sets() {
    let mut setup = Setup::new();
    let mut client = setup.connect().await;
    let first = client.start(&setup.work(), "one").await;
    client.idle(first).await;
    let second = client.start(&setup.work(), "two").await;
    client.idle(second).await;
    let started = list_order(&setup).await;

    client.prompt(first, "a turn").await;
    client.says(first, "Echo: a turn").await;
    client.idle(first).await;
    let after_a_turn = list_order(&setup).await;
    let moved = MoveParams {
        agent: first,
        before: Some(second),
    };
    assert_eq!(
        client.ok(Request::AgentMove(moved)).await,
        ResponseBody::Done
    );
    let updated = client.summary_with(first, |_| true).await;
    let unknown = MoveParams {
        agent: first,
        before: Some(AgentId::new(9)),
    };
    let refused = client.error_code(Request::AgentMove(unknown)).await;
    drop(client);
    setup.restart();

    assert_eq!(started, [second, first]);
    assert_eq!(after_a_turn, [second, first]);
    assert!(updated.position > Default::default());
    assert_eq!(refused, ErrorCode::NotFound);
    assert_eq!(list_order(&setup).await, [first, second]);
}

/// A close takes an agent off the list while it waits for an answer: no stop and no wait for
/// its process come first.
#[tokio::test]
async fn a_close_removes_an_agent_whose_claude_runs() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "approve this").await;
    client
        .item_with(agent, |body| pending_of(body).is_some())
        .await;

    let closed = client.ok(Request::AgentClose(AgentParams { agent })).await;
    client
        .wait_for(|event| {
            matches!(event, Event::AgentRemoved { agent: removed } if *removed == agent)
                .then_some(())
        })
        .await;
    let again = client
        .error_code(Request::AgentClose(AgentParams { agent }))
        .await;

    assert_eq!(closed, ResponseBody::Done);
    assert_eq!(again, ErrorCode::NotFound);
    let (_, catchup) = setup.connect_after(None).await;
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot");
    };
    assert!(snapshot.agents.is_empty());
    let log = setup
        .state()
        .join("events")
        .join(format!("{}.jsonl", agent.get()));
    assert!(!log.exists(), "the agent's log is gone");
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

/// `machine.load` shows the agent's `claude` and the command it runs, and `agent.stop_process`
/// stops that command and refuses a process outside the agent.
#[tokio::test]
async fn the_machine_load_shows_an_agents_command_which_it_can_stop() {
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
    let child: u32 = markdown["Started child ".len()..].parse().unwrap();
    client.idle(agent).await;
    // The fake `claude` lingers, and outlives the daemon the test kills: the test ends it.
    let claude = parent_of(i32::try_from(child).unwrap());

    let ResponseBody::MachineLoad(load) = client.ok(Request::MachineLoad).await else {
        panic!("expected the machine's load");
    };
    let outside = client
        .error_code(Request::AgentStopProcess(StopProcessParams {
            agent,
            pid: ProcessId::new(std::process::id()),
        }))
        .await;
    client
        .ok(Request::AgentStopProcess(StopProcessParams {
            agent,
            pid: ProcessId::new(child),
        }))
        .await;

    let child = i32::try_from(child).unwrap();
    let deadline = Instant::now() + WAIT;
    while is_running(child) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let survived = is_running(child);
    if survived {
        let pid = Pid::from_raw(child).unwrap();
        rustix::process::kill_process(pid, Signal::KILL).unwrap();
    }
    rustix::process::kill_process(Pid::from_raw(claude).unwrap(), Signal::KILL).unwrap();
    assert!(load.server.cores >= 1);
    assert!(load.server.memory_used_bytes <= load.server.memory_total_bytes);
    let ours = load
        .agents
        .iter()
        .find(|found| found.agent == agent)
        .expect("the agent's load");
    assert_eq!(ours.processes, 2, "claude and its child: {ours:?}");
    let sleep = ours.heaviest.first().expect("the child's command");
    assert_eq!(
        (sleep.pid.get(), sleep.command.as_str()),
        (child.unsigned_abs(), "sleep 600")
    );
    assert_eq!(load.history.server.len(), 1);
    assert_eq!(outside, ErrorCode::NotFound);
    assert!(!survived, "the child still runs after agent.stop_process");
}

/// The parent in `/proc/<pid>/stat`: `pid (name) state ppid ...`.
fn parent_of(pid: i32) -> i32 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let end = stat.rfind(')').unwrap();
    stat[end + 1..]
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
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
        "agents.json",
        "details/1.json",
        "events/1.jsonl",
        "events/daemon.jsonl",
        "raw/1.jsonl",
        "serve.lock",
        "yetack.sock",
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

    let hello = format!(
        "{{\"type\":\"hello\",\"protocol_version\":{PROTOCOL_VERSION},\"last_seq\":null}}\n"
    );
    write.write_all(hello.as_bytes()).await.unwrap();
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
async fn a_file_comes_with_its_edits() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let work = setup.work();
    std::fs::create_dir(work.join("old")).unwrap();
    std::fs::write(work.join("notes.txt"), "remember the bread\n").unwrap();
    std::fs::write(work.join("old/notes.txt"), "remember the tea\n").unwrap();
    std::fs::write(work.join("old/todo.txt"), "call the bank\n").unwrap();
    let agent = client.start(&work, "edit notes.txt milk bread").await;
    for prompt in [
        "edit old/notes.txt coffee tea",
        "edit old/todo.txt shop bank",
    ] {
        client.says(agent, "Edited.").await;
        client.idle(agent).await;
        client.prompt(agent, prompt).await;
    }
    client.says(agent, "Edited.").await;
    client.idle(agent).await;
    let read = |path: &str| {
        Request::AgentReadFile(ReadFileParams {
            agent,
            path: PathBuf::from(path),
        })
    };
    let edit = |path: &str, old_text: &str, new_text: &str| FileEdit {
        path: work.join(path),
        old_text: old_text.to_owned(),
        new_text: new_text.to_owned(),
        texts: EditTexts::Kept,
    };

    // The other file of the same name keeps its own edit.
    assert_eq!(
        client.ok(read("notes.txt")).await,
        ResponseBody::File {
            path: Some(work.join("notes.txt")),
            text: "remember the bread\n".to_owned(),
            edits: Some(vec![edit("notes.txt", "milk", "bread")]),
        }
    );
    // A name that is not in the folder finds the file the agent changed.
    assert_eq!(
        client.ok(read("todo.txt")).await,
        ResponseBody::File {
            path: Some(work.join("old/todo.txt")),
            text: "call the bank\n".to_owned(),
            edits: Some(vec![edit("old/todo.txt", "shop", "bank")]),
        }
    );
}

#[tokio::test]
async fn files_settings_sessions_and_agent_settings() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    std::fs::write(setup.work().join("notes.txt"), "remember the milk\n").unwrap();
    std::fs::write(setup.work().join("cart.gif"), b"GIF89a").unwrap();
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
            path: Some(setup.work().join("notes.txt")),
            text: "remember the milk\n".to_owned(),
            edits: Some(Vec::new()),
        }
    );
    assert_eq!(
        client.ok(read("cart.gif")).await,
        ResponseBody::Image {
            path: setup.work().join("cart.gif"),
            image: Image {
                media_type: ImageMediaType::Gif,
                data: "R0lGODlh".to_owned(),
            }
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
    let mcp_server = |action| {
        Request::AgentMcpToggle(McpToggleParams {
            agent,
            server: "notes".to_owned(),
            action,
        })
    };
    assert_eq!(
        client.ok(mcp_server(McpToggleAction::Disable)).await,
        ResponseBody::Done
    );
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
            account: AccountId::DEFAULT,
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
            account: AccountId::DEFAULT,
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
            account: AccountId::DEFAULT,
        }))
        .await;
    assert_eq!(missing, ErrorCode::NotFound);
}

/// A session file in a Claude Code folder, made with the work folder as its `cwd`.
fn write_session(claude_folder: &Path, work: &Path, id: &str, prompt: &str) {
    let project = claude_folder.join("projects/-srv-work");
    std::fs::create_dir_all(&project).unwrap();
    let line = json!({
        "type": "user", "cwd": work,
        "message": {"role": "user", "content": prompt}
    });
    std::fs::write(project.join(format!("{id}.jsonl")), format!("{line}\n")).unwrap();
}

#[tokio::test]
async fn each_accounts_claude_gets_its_folder_and_only_the_login_it_may_use() {
    let setup = Setup::new_with(
        None,
        DefaultFolder::Variable,
        vec![
            ("ANTHROPIC_API_KEY", "test-api-key"),
            ("ANTHROPIC_AUTH_TOKEN", "test-auth-token"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "test-oauth-token"),
        ],
    );
    let mut client = setup.connect().await;
    let keyed_folder = setup.temp.path().join("claude-keyed");
    let plain_folder = setup.temp.path().join("claude-plain");
    let keyed = client
        .add_account("Keyed", Some(&keyed_folder), CredentialSource::Environment)
        .await;
    let plain = client
        .add_account("Plain", Some(&plain_folder), CredentialSource::SignIn)
        .await;

    for account in [AccountId::DEFAULT, keyed.account.id, plain.account.id] {
        let agent = client.start_with(&setup.work(), "hello", account).await;
        let summary = client.summary_with(agent, |_| true).await;
        assert_eq!(summary.account, account);
    }
    let envs = setup.wait_for_envs(3);

    assert_eq!(
        envs,
        [
            json!({"CLAUDE_CONFIG_DIR": setup.default_folder(),
                   "CLAUDE_CODE_OAUTH_TOKEN": null, "ANTHROPIC_API_KEY": null,
                   "ANTHROPIC_AUTH_TOKEN": null}),
            json!({"CLAUDE_CONFIG_DIR": keyed_folder,
                   "CLAUDE_CODE_OAUTH_TOKEN": null, "ANTHROPIC_API_KEY": "test-api-key",
                   "ANTHROPIC_AUTH_TOKEN": "test-auth-token"}),
            json!({"CLAUDE_CONFIG_DIR": plain_folder,
                   "CLAUDE_CODE_OAUTH_TOKEN": null, "ANTHROPIC_API_KEY": null,
                   "ANTHROPIC_AUTH_TOKEN": null}),
        ]
    );
    let ResponseBody::Accounts {
        environment_credentials,
        ..
    } = client.ok(Request::AccountsList).await
    else {
        panic!("expected the accounts");
    };
    assert_eq!(environment_credentials, EnvironmentCredentials::Set);
}

#[tokio::test]
async fn an_auth_token_alone_counts_as_credentials_in_the_environment() {
    let setup = Setup::new_with(
        None,
        DefaultFolder::Variable,
        vec![("ANTHROPIC_AUTH_TOKEN", "test-auth-token")],
    );
    let client = setup.connect().await;

    let ResponseBody::Accounts {
        environment_credentials,
        ..
    } = client.ok(Request::AccountsList).await
    else {
        panic!("expected the accounts");
    };

    assert_eq!(environment_credentials, EnvironmentCredentials::Set);
}

/// `claude mcp remove` edits the user scope of the agent's account, without the daemon's logins.
#[tokio::test]
async fn removing_an_mcp_server_runs_claude_with_the_agents_account() {
    let setup = Setup::new_with(
        None,
        DefaultFolder::Variable,
        vec![
            ("ANTHROPIC_API_KEY", "test-api-key"),
            ("ANTHROPIC_AUTH_TOKEN", "test-auth-token"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "test-oauth-token"),
        ],
    );
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await;
    let agent = client
        .start_with(&setup.work(), "hello", work.account.id)
        .await;
    client.says(agent, "Echo: hello").await;

    let removed = client
        .ok(Request::McpRemove(McpRemoveParams {
            agent,
            server: "notes".to_owned(),
            scope: McpScope::User,
        }))
        .await;

    assert_eq!(removed, ResponseBody::Done);
    let launches = setup.wait_for_launches(2);
    assert!(launches[1].contains(&"mcp".to_owned()), "{launches:?}");
    let envs = setup.wait_for_envs(2);
    assert_eq!(
        envs[1],
        json!({"CLAUDE_CONFIG_DIR": folder,
               "CLAUDE_CODE_OAUTH_TOKEN": null, "ANTHROPIC_API_KEY": null,
               "ANTHROPIC_AUTH_TOKEN": null})
    );
}

/// Without `CLAUDE_CONFIG_DIR`, the default account is `.claude` in the home folder, and a new
/// account's folder sits beside it.
#[tokio::test]
async fn the_default_account_runs_without_the_variable_and_a_new_one_signs_in_beside_it() {
    let setup = Setup::new_with(None, DefaultFolder::Home, Vec::new());
    let mut client = setup.connect().await;
    let home = setup.temp.path().join("home");

    let agent = client.start(&setup.work(), "hello").await;
    client.says(agent, "Echo: hello").await;
    let before = client.accounts().await;
    std::fs::write(setup.default_folder().join(CREDENTIALS_FILE), "{}").unwrap();
    let after = client.accounts().await;
    let added = client
        .add_account("Client Work", None, CredentialSource::SignIn)
        .await;

    let envs = setup.wait_for_envs(1);
    assert_eq!(envs[0]["CLAUDE_CONFIG_DIR"], Value::Null);
    let [default] = before.as_slice() else {
        panic!("expected the default account alone: {before:?}");
    };
    assert_eq!(default.account.folder, setup.default_folder());
    assert_eq!(default.sign_in, SignIn::NotSignedIn);
    // The daemon runs the program its configuration names, so the command names it too.
    let claude = env!("CARGO_BIN_EXE_fake-claude");
    assert_eq!(default.sign_in_command, format!("'{claude}' auth login"));
    assert_eq!(after[0].sign_in, SignIn::SignedIn);
    let folder = home.join(".claude-client-work");
    assert_eq!(added.account.folder, folder);
    assert_eq!(added.sign_in, SignIn::NotSignedIn);
    assert_eq!(
        added.sign_in_command,
        format!(
            "CLAUDE_CONFIG_DIR='{}' '{claude}' auth login",
            folder.display()
        )
    );
}

#[tokio::test]
async fn an_agent_keeps_its_account_through_a_stop_a_resume_and_a_restart() {
    let mut setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await
        .account;
    let agent = client.start_with(&setup.work(), "hello", work.id).await;
    client.says(agent, "Echo: hello").await;

    client.ok(Request::AgentStop(AgentParams { agent })).await;
    client
        .summary_with(agent, |summary| summary.status == AgentStatus::Exited)
        .await;
    client.ok(Request::AgentResume(AgentParams { agent })).await;
    setup.wait_for_envs(2);
    setup.restart();
    let (_, catchup) = setup.connect_after(None).await;

    let envs = setup.wait_for_envs(3);
    for env in &envs {
        assert_eq!(env["CLAUDE_CONFIG_DIR"], json!(folder), "{envs:?}");
    }
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot");
    };
    assert_eq!(snapshot.agents[0].account, work.id);
    let names: Vec<&str> = snapshot
        .accounts
        .iter()
        .map(|account| account.name.as_str())
        .collect();
    assert_eq!(names, ["Default", "Work"]);
}

#[tokio::test]
async fn plan_usage_is_kept_per_account() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await
        .account;
    let five_hour = |usage: &PlanUsage| usage.windows.get(&UsageWindow::FiveHour)?.utilization;

    client.start(&setup.work(), "usage 0.4").await;
    let default_usage = client
        .wait_for(|event| match event {
            Event::PlanUsageChanged(usage) if five_hour(usage) == Some(0.4) => Some(usage.clone()),
            _ => None,
        })
        .await;
    client.start_with(&setup.work(), "usage 0.7", work.id).await;
    let work_usage = client
        .wait_for(|event| match event {
            Event::AccountPlanUsageChanged(changed) if five_hour(&changed.usage) == Some(0.7) => {
                Some(changed.clone())
            }
            Event::PlanUsageChanged(usage) => panic!("the default account changed: {usage:?}"),
            _ => None,
        })
        .await;
    let (_, catchup) = setup.connect_after(None).await;

    assert_eq!(work_usage.account, work.id);
    let Catchup::Snapshot(snapshot) = catchup else {
        panic!("expected a snapshot");
    };
    assert_eq!(snapshot.plan_usage, default_usage);
    assert_eq!(
        snapshot.other_plan_usage,
        [AccountPlanUsage {
            account: work.id,
            usage: work_usage.usage,
        }]
    );
}

#[tokio::test]
async fn past_sessions_list_and_resume_with_their_account() {
    const WORK_SESSION: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await
        .account;
    write_session(
        &setup.default_folder(),
        &setup.work(),
        SESSION_ID,
        "Personal task",
    );
    write_session(&folder, &setup.work(), WORK_SESSION, "Work task");

    let ResponseBody::Sessions { sessions } = client.ok(Request::SessionsList).await else {
        panic!("expected sessions");
    };
    let mut listed: Vec<(&str, AccountId)> = sessions
        .iter()
        .map(|session| (session.title.as_str(), session.account))
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        [
            ("Personal task", AccountId::DEFAULT),
            ("Work task", work.id)
        ]
    );
    let resume = |account| {
        Request::SessionsResume(SessionResumeParams {
            session: ClaudeSessionId::new(WORK_SESSION.to_owned()),
            account,
        })
    };
    let elsewhere = client.error_code(resume(AccountId::DEFAULT)).await;
    let ResponseBody::AgentStarted { agent } = client.ok(resume(work.id)).await else {
        panic!("expected AgentStarted");
    };

    assert_eq!(elsewhere, ErrorCode::NotFound);
    let summary = client.summary_with(agent, |_| true).await;
    assert_eq!(summary.account, work.id);
    let envs = setup.wait_for_envs(1);
    assert_eq!(envs[0]["CLAUDE_CONFIG_DIR"], json!(folder));
    let launches = setup.wait_for_launches(1);
    assert!(launches[0].contains(&format!("--resume={WORK_SESSION}")));
}

#[tokio::test]
async fn an_account_an_agent_uses_cannot_be_removed() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await
        .account;
    let agent = client.start_with(&setup.work(), "hello", work.id).await;
    let remove = |account| Request::AccountsRemove(AccountParams { account });

    let used = client.error_code(remove(work.id)).await;
    let default = client.error_code(remove(AccountId::DEFAULT)).await;
    client.ok(Request::AgentClose(AgentParams { agent })).await;
    let removed = client.ok(remove(work.id)).await;
    let accounts: Vec<Account> = client
        .wait_for(|event| match event {
            Event::AccountsChanged { accounts } if accounts.len() == 1 => Some(accounts.clone()),
            _ => None,
        })
        .await;

    assert_eq!(used, ErrorCode::AccountInUse);
    assert_eq!(default, ErrorCode::InvalidRequest);
    assert_eq!(removed, ResponseBody::Done);
    assert_eq!(accounts[0].id, AccountId::DEFAULT);
}

#[tokio::test]
async fn an_account_without_a_login_says_how_to_sign_in() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await;
    let agent = client
        .start_with(&setup.work(), "nologin", work.account.id)
        .await;

    let notice = client
        .item_with(agent, |body| matches!(body, ItemBody::Notice { .. }))
        .await;
    let end = client
        .item_with(agent, |body| matches!(body, ItemBody::TurnEnd(_)))
        .await;

    let ItemBody::Notice { text, .. } = notice.body else {
        panic!("expected a notice");
    };
    assert!(text.contains("The account Work is not signed in"), "{text}");
    assert!(text.ends_with(&work.sign_in_command), "{text}");
    let ItemBody::TurnEnd(end) = end.body else {
        panic!("expected a turn end");
    };
    assert_eq!(end.error.as_deref(), Some("The account is not signed in"));
    assert_eq!(end.outcome, TurnOutcome::NotSignedIn);
    let items = client.history(agent).await;
    assert!(
        !items.iter().any(|item| matches!(
            &item.body,
            ItemBody::Notice { text, .. } if text.contains("/login")
        )),
        "{items:?}"
    );
}

#[tokio::test]
async fn an_agent_without_a_login_needs_you_until_its_account_signs_in() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await;
    let agent = client
        .start_with(&setup.work(), "nologin", work.account.id)
        .await;
    let waits = |event: &Event| match event {
        Event::AgentUpdated(summary) if summary.id == agent => Some(summary.clone()),
        _ => None,
    };

    let waiting = client
        .wait_for(|event| {
            waits(event).filter(|summary| summary.sign_in_wait == SignInWait::Waiting)
        })
        .await;
    // Still waiting: the folder holds no login yet.
    client.ok(Request::AccountsList).await;
    // The login must be written after the failed turn, on a clock that counts milliseconds.
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(CREDENTIALS_FILE), "{}").unwrap();
    client.ok(Request::AccountsList).await;
    let (_listing, catchup) = setup.connect_after(None).await;
    let Catchup::Snapshot(listed) = catchup else {
        panic!("expected a snapshot");
    };
    let signed_in = client
        .wait_for(|event| {
            waits(event).filter(|summary| summary.sign_in_wait == SignInWait::NotWaiting)
        })
        .await;

    assert_eq!(waiting.status, AgentStatus::NeedsYou);
    assert_eq!(waiting.pending_count, 0);
    assert_eq!(
        listed.agents[0].sign_in_wait,
        SignInWait::NotWaiting,
        "the wait ended before accounts.list answered"
    );
    assert_eq!(signed_in.status, AgentStatus::Idle);
}

/// A credentials file Claude Code can no longer use, as one whose OAuth session expired and could
/// not be refreshed, is no sign-in: a turn failed on it after it was written.
#[tokio::test]
async fn a_login_a_turn_failed_on_after_it_was_written_is_no_sign_in() {
    let setup = Setup::new();
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-work");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(CREDENTIALS_FILE), "{}").unwrap();
    let work = client
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await;
    // The turn must fail after the login was written, on a clock that counts milliseconds.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let agent = client
        .start_with(&setup.work(), "nologin", work.account.id)
        .await;
    client
        .wait_for(|event| match event {
            Event::AgentUpdated(summary)
                if summary.id == agent && summary.sign_in_wait == SignInWait::Waiting =>
            {
                Some(())
            }
            _ => None,
        })
        .await;

    let listed = |accounts: ResponseBody| {
        let ResponseBody::Accounts { accounts, .. } = accounts else {
            panic!("expected the accounts");
        };
        accounts
            .iter()
            .find(|state| state.account.id == work.account.id)
            .map(|state| state.sign_in)
    };

    let failed_on = listed(client.ok(Request::AccountsList).await);
    // A new sign-in writes the file again.
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(folder.join(CREDENTIALS_FILE), "{}").unwrap();
    let signed_in_again = listed(client.ok(Request::AccountsList).await);

    assert_eq!(failed_on, Some(SignIn::NotSignedIn));
    assert_eq!(signed_in_again, Some(SignIn::SignedIn));
}

/// A turn of an account that uses the daemon's API key fails on the key, not on the folder's
/// login, which stays signed in.
#[tokio::test]
async fn a_refused_api_key_leaves_the_folders_login_signed_in() {
    let setup = Setup::new_with(
        None,
        DefaultFolder::Variable,
        vec![("ANTHROPIC_API_KEY", "test-api-key")],
    );
    let mut client = setup.connect().await;
    let folder = setup.temp.path().join("claude-keyed");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(CREDENTIALS_FILE), "{}").unwrap();
    let keyed = client
        .add_account("Keyed", Some(&folder), CredentialSource::Environment)
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let agent = client
        .start_with(&setup.work(), "nologin", keyed.account.id)
        .await;
    client
        .wait_for(|event| match event {
            Event::AgentUpdated(summary)
                if summary.id == agent && summary.sign_in_wait == SignInWait::Waiting =>
            {
                Some(())
            }
            _ => None,
        })
        .await;

    let ResponseBody::Accounts { accounts, .. } = client.ok(Request::AccountsList).await else {
        panic!("expected the accounts");
    };
    let sign_in = accounts
        .iter()
        .find(|state| state.account.id == keyed.account.id)
        .map(|state| state.sign_in);

    assert_eq!(sign_in, Some(SignIn::SignedIn));
}

/// The daemon checks the accounts' folders on its own: no client has to ask `accounts.list`.
#[tokio::test]
async fn a_sign_in_ends_the_wait_and_tells_the_clients_without_a_check() {
    let mut setup = Setup::new();
    let folder = setup.temp.path().join("claude-work");
    let work = setup
        .connect()
        .await
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await;
    // The new daemon's first check sees the account, so only its sign-in tells the clients.
    setup.restart();
    let mut client = setup.connect().await;
    let agent = client
        .start_with(&setup.work(), "nologin", work.account.id)
        .await;
    client
        .wait_for(|event| match event {
            Event::AgentUpdated(summary)
                if summary.id == agent && summary.sign_in_wait == SignInWait::Waiting =>
            {
                Some(())
            }
            _ => None,
        })
        .await;

    // The login must be written after the failed turn, on a clock that counts milliseconds.
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(CREDENTIALS_FILE), "{}").unwrap();
    let mut accounts_changed = false;
    let mut signed_in = None;
    while !accounts_changed || signed_in.is_none() {
        match client.next_event().await.event {
            Event::AccountsChanged { .. } => accounts_changed = true,
            Event::AgentUpdated(summary)
                if summary.id == agent && summary.sign_in_wait == SignInWait::NotWaiting =>
            {
                signed_in = Some(summary);
            }
            _ => {}
        }
    }

    assert_eq!(
        signed_in.map(|summary| summary.status),
        Some(AgentStatus::Idle)
    );
}

/// A turn that fails on the login tells the clients that the account is no longer signed in,
/// without a check from them.
#[tokio::test]
async fn a_turn_that_fails_on_the_login_tells_the_clients_without_a_check() {
    let mut setup = Setup::new();
    let folder = setup.temp.path().join("claude-work");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(CREDENTIALS_FILE), "{}").unwrap();
    let work = setup
        .connect()
        .await
        .add_account("Work", Some(&folder), CredentialSource::SignIn)
        .await;
    // The new daemon's first check sees the account signed in, so only the failure tells.
    setup.restart();
    let mut client = setup.connect().await;
    // The turn must fail after the login was written, on a clock that counts milliseconds.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let agent = client
        .start_with(&setup.work(), "nologin", work.account.id)
        .await;

    let mut waits = false;
    let mut accounts_changed = false;
    while !waits || !accounts_changed {
        match client.next_event().await.event {
            Event::AccountsChanged { .. } => accounts_changed = true,
            Event::AgentUpdated(summary)
                if summary.id == agent && summary.sign_in_wait == SignInWait::Waiting =>
            {
                waits = true;
            }
            _ => {}
        }
    }
    let ResponseBody::Accounts { accounts, .. } = client.ok(Request::AccountsList).await else {
        panic!("expected the accounts");
    };
    let sign_in = accounts
        .iter()
        .find(|state| state.account.id == work.account.id)
        .map(|state| state.sign_in);

    assert_eq!(sign_in, Some(SignIn::NotSignedIn));
}

/// The app restarts this computer's background service with its own program this way: the daemon
/// names itself in its lock, and stops when asked, so that another daemon can take the folder.
#[tokio::test]
async fn the_daemon_names_itself_in_its_lock_and_stops_when_asked() {
    let mut setup = Setup::new();
    let pid = ProcessId::new(setup.daemon.as_ref().unwrap().id());
    let transport = Transport::Local {
        socket: setup.socket(),
    };
    let (connection, welcome) = Connection::connect(&transport, None).await.unwrap();
    drop(connection);

    let holder = cockpit_daemon::lock_holder(&setup.state())
        .unwrap()
        .unwrap();
    let ran = holder.runs();
    let state = setup.state();
    let stopped = tokio::task::spawn_blocking(move || cockpit_daemon::stop_lock_holder(&state))
        .await
        .unwrap();

    let status = setup.daemon.take().unwrap().wait().unwrap();
    assert_eq!(welcome.daemon_commit, cockpit_daemon::build_commit());
    assert_eq!(holder.pid(), pid);
    assert!(ran, "the daemon is the recorded process");
    assert!(!holder.runs(), "it ended");
    assert!(stopped.is_ok(), "{stopped:?}");
    assert!(status.success(), "{status:?}");
    assert_eq!(cockpit_daemon::lock_holder(&setup.state()).unwrap(), None);
    setup.start();
}

/// A terminal runs the shell in the agent's folder, takes what is typed, keeps the output for a
/// client that attaches, and goes when closed.
#[cfg(unix)]
#[tokio::test]
async fn a_terminal_runs_a_shell_in_the_agents_folder() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let setup = Setup::new_with(
        None,
        DefaultFolder::Variable,
        vec![("YETACK_SHELL", "/bin/sh")],
    );
    let mut client = setup.connect().await;
    let agent = client.start(&setup.work(), "hello").await;
    let opened = client
        .ok(Request::AgentTerminalOpen(TerminalOpenParams {
            agent,
            size: TerminalSize { cols: 80, rows: 24 },
        }))
        .await;
    let ResponseBody::TerminalOpened { terminal } = opened else {
        panic!("expected a terminal, got {opened:?}");
    };
    let params = TerminalParams { agent, terminal };

    client
        .ok(Request::TerminalInput(TerminalInputParams {
            agent,
            terminal,
            data: "pwd; echo marker-$((40 + 2))\n".to_owned(),
        }))
        .await;
    let deadline = Instant::now() + WAIT;
    let screen = loop {
        let ResponseBody::TerminalScreen { offset, data } =
            client.ok(Request::TerminalAttach(params)).await
        else {
            panic!("expected a screen");
        };
        let text = String::from_utf8_lossy(&STANDARD.decode(data).unwrap()).into_owned();
        if text.contains("marker-42") || Instant::now() > deadline {
            assert_eq!(offset, 0);
            break text;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert!(screen.contains("marker-42"), "{screen:?}");
    assert!(
        screen.contains(&setup.work().display().to_string()),
        "{screen:?}"
    );
    let listed = client
        .ok(Request::AgentTerminals(AgentParams { agent }))
        .await;
    assert!(matches!(
        listed,
        ResponseBody::Terminals { ref terminals } if terminals.len() == 1
    ));
    client.ok(Request::TerminalClose(params)).await;
    assert_eq!(
        client.error_code(Request::TerminalAttach(params)).await,
        ErrorCode::NotFound
    );
}

//! The wire format: every message round trips, a few are pinned as exact JSON, and every word the
//! protocol writes is pinned.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failure is a failed test"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::PathBuf;
use std::time::Duration;

use cockpit_protocol::{
    AgentDetails, AgentId, AgentParams, AgentStatus, AgentSummary, Answer, AnswerParams,
    AnswerState, Approval, ApprovalAnswer, ApprovalDecision, AskedQuestion, BackgroundTask,
    BackgroundTaskId, BackgroundTaskKind, BackgroundTaskStatus, CancelQueuedParams, Catchup,
    ChangedFile, ClaudeSessionId, ClientMessage, CommandKind, ContextCategory, ContextUsage,
    CostUsd, Effort, ErrorBody, ErrorCode, Event, EventRecord, FileEdit, HistoryPage,
    HistoryParams, Image, ImageMediaType, ItemBody, ItemId, McpRemoveParams, McpScope, McpServer,
    McpServerParams, McpServerStatus, McpToggleParams, McpTool, McpTransport, Method, MoveParams,
    PROTOCOL_VERSION, PastSession, PendingId, PermissionMode, Plan, PlanAnswer, PlanDecision,
    PlanUsage, Position, Prompt, PromptParams, Question, QuestionAnswer, QuestionOption,
    RateLimitStatus, ReadFileParams, RenameParams, Request, RequestId, ResponseBody,
    ResponseResult, Seq, ServerMessage, SessionResumeParams, SetEffortParams, SetModeParams,
    SetModelParams, Settings, SlashCommand, Snapshot, StartParams, StopTaskParams, TaskListItem,
    TaskProgress, TaskStatus, Timestamp, TokenUsage, ToolCall, ToolCallStatus, TranscriptItem,
    TurnEnd, UsageWindow, UserMessage, WindowUsage,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use strum::VariantArray;

const AGENT: u32 = 3;

fn agent() -> AgentId {
    AgentId::new(AGENT)
}

fn pending() -> PendingId {
    PendingId::new("toolu_01".to_owned())
}

fn session_id() -> ClaudeSessionId {
    ClaudeSessionId::new("0b7e1c2a-3d4f-4a5b-8c6d-7e8f9a0b1c2d".to_owned())
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_unix_millis(millis)
}

fn summary() -> AgentSummary {
    AgentSummary {
        id: agent(),
        title: "Fix the login form".to_owned(),
        folder: PathBuf::from("/srv/projects/shop"),
        status: AgentStatus::NeedsYou,
        activity: Some("Waiting for approval: Bash".to_owned()),
        pending_count: 1,
        model: Some("opus".to_owned()),
        effort: Some(Effort::High),
        effort_is_default: true,
        mode: PermissionMode::AcceptEdits,
        task_progress: Some(TaskProgress { done: 3, total: 5 }),
        context_percent: Some(42),
        cache_expires_at: Some(at(1_700_000_300_000)),
        turns: 7,
        cost: CostUsd::new(1.25),
        started_at: at(1_700_000_000_000),
        session: Some(session_id()),
        position: Position::new(2048),
    }
}

fn idle_summary() -> AgentSummary {
    AgentSummary {
        id: AgentId::new(4),
        title: "Untitled".to_owned(),
        folder: PathBuf::from("/srv/projects/blog"),
        status: AgentStatus::Idle,
        activity: None,
        pending_count: 0,
        model: None,
        effort: None,
        effort_is_default: false,
        mode: PermissionMode::Default,
        task_progress: None,
        context_percent: None,
        cache_expires_at: None,
        turns: 0,
        cost: CostUsd::default(),
        started_at: at(1_700_000_000_000),
        session: None,
        position: Position::new(1024),
    }
}

fn image() -> Image {
    Image {
        media_type: ImageMediaType::Png,
        data: "iVBORw0KGgo=".to_owned(),
    }
}

fn prompt() -> Prompt {
    Prompt {
        text: "Why does the form reject valid emails?".to_owned(),
        images: vec![image()],
    }
}

fn edit() -> FileEdit {
    FileEdit {
        path: PathBuf::from("src/login.rs"),
        old_text: "let valid = false;".to_owned(),
        new_text: "let valid = check(email);".to_owned(),
    }
}

fn tool_call(status: ToolCallStatus) -> ToolCall {
    ToolCall {
        tool: "Bash".to_owned(),
        summary: "cargo test".to_owned(),
        input: json!({"command": "cargo test", "description": "Run the tests"}),
        status,
        output: Some("test result: FAILED. 3 passed; 1 failed".to_owned()),
        exit_code: Some(101),
        edit: None,
    }
}

fn asked_question() -> AskedQuestion {
    AskedQuestion {
        header: "Database".to_owned(),
        text: "Which database should the tests use?".to_owned(),
        options: vec![
            QuestionOption {
                label: "SQLite".to_owned(),
                description: "In memory, no setup".to_owned(),
            },
            QuestionOption {
                label: "Postgres".to_owned(),
                description: "Like production".to_owned(),
            },
        ],
        multi_select: false,
    }
}

fn question_answers() -> Vec<QuestionAnswer> {
    vec![QuestionAnswer {
        selected: vec!["SQLite".to_owned()],
        other: Some("and keep the fixtures small".to_owned()),
    }]
}

fn approval_answer() -> ApprovalAnswer {
    ApprovalAnswer {
        decision: ApprovalDecision::Deny,
        message: Some("Run only the login tests".to_owned()),
    }
}

fn plan_answer() -> PlanAnswer {
    PlanAnswer {
        decision: PlanDecision::KeepPlanning,
        message: Some("Split the second step".to_owned()),
    }
}

/// One item of every kind, and every answer state.
fn bodies() -> Vec<ItemBody> {
    vec![
        ItemBody::UserMessage(UserMessage {
            text: prompt().text,
            images: vec![image()],
            queued: true,
        }),
        ItemBody::AssistantText {
            markdown: "The regex rejects `+` in the local part.".to_owned(),
        },
        ItemBody::Thinking {
            text: "The form calls validate() twice.".to_owned(),
        },
        ItemBody::ToolCall(ToolCall {
            tool: "Edit".to_owned(),
            summary: "src/login.rs".to_owned(),
            input: json!({"file_path": "src/login.rs"}),
            status: ToolCallStatus::Done,
            output: None,
            exit_code: None,
            edit: Some(edit()),
        }),
        ItemBody::Subagent(tool_call(ToolCallStatus::Running)),
        ItemBody::Approval(Approval {
            pending: pending(),
            tool: "Bash".to_owned(),
            summary: "rm -rf target".to_owned(),
            input: json!({"command": "rm -rf target"}),
            edit: None,
            reason: Some("The command deletes files".to_owned()),
            answer: AnswerState::Answered(approval_answer()),
            tool_call: Some(ItemId::new(12)),
        }),
        ItemBody::Approval(Approval {
            pending: pending(),
            tool: "Write".to_owned(),
            summary: "notes.md".to_owned(),
            input: json!({"file_path": "notes.md", "content": "Done"}),
            edit: Some(edit()),
            reason: None,
            answer: AnswerState::Waiting,
            tool_call: None,
        }),
        ItemBody::Question(Question {
            pending: pending(),
            questions: vec![asked_question()],
            answer: AnswerState::Answered(question_answers()),
        }),
        ItemBody::Plan(Plan {
            pending: pending(),
            markdown: "1. Fix the regex\n2. Add a test".to_owned(),
            answer: AnswerState::Answered(plan_answer()),
        }),
        ItemBody::Plan(Plan {
            pending: pending(),
            markdown: "1. Rewrite the form".to_owned(),
            answer: AnswerState::Cancelled,
        }),
        ItemBody::TurnEnd(TurnEnd {
            duration: Duration::from_millis(83_250),
            cost: CostUsd::new(0.42),
            usage: TokenUsage {
                input: 1_200,
                output: 3_400,
                cache_creation: 15_000,
                cache_read: 210_000,
            },
            error: Some("The turn reached the maximum number of turns".to_owned()),
            stopped: false,
        }),
        ItemBody::Notice {
            text: "Conversation compacted".to_owned(),
            detail: None,
        },
        ItemBody::Notice {
            text: "Skill instructions: /greet".to_owned(),
            detail: Some("# /greet\n\nGreet the person the arguments name.".to_owned()),
        },
    ]
}

fn items() -> Vec<TranscriptItem> {
    let mut items = Vec::new();
    for (raw, body) in (10..).zip(bodies()) {
        items.push(TranscriptItem {
            id: ItemId::new(raw),
            parent: (raw == 15).then(|| ItemId::new(14)),
            body,
        });
    }
    items
}

fn context_usage() -> ContextUsage {
    ContextUsage {
        tokens: 84_000,
        max_tokens: 200_000,
        percent: 42,
        categories: vec![
            ContextCategory {
                name: "System prompt".to_owned(),
                tokens: 3_000,
            },
            ContextCategory {
                name: "Messages".to_owned(),
                tokens: 81_000,
            },
        ],
    }
}

fn mcp_servers() -> Vec<McpServer> {
    vec![
        McpServer {
            name: "issues".to_owned(),
            status: McpServerStatus::Connected,
            error: None,
            version: Some("1.4.0".to_owned()),
            transport: Some(McpTransport::Stdio {
                command: "issues-mcp --stdio".to_owned(),
            }),
            scope: Some(McpScope::Project),
            tools: vec![McpTool {
                name: "list_issues".to_owned(),
                description: Some("Lists open issues".to_owned()),
                read_only: true,
                destructive: false,
                open_world: true,
                calls: 4,
            }],
        },
        McpServer {
            name: "docs".to_owned(),
            status: McpServerStatus::NeedsAuth,
            error: Some("401 Unauthorized".to_owned()),
            version: None,
            transport: Some(McpTransport::Http {
                url: "https://docs.example.com/mcp".to_owned(),
            }),
            scope: Some(McpScope::User),
            tools: Vec::new(),
        },
        McpServer {
            name: "events".to_owned(),
            status: McpServerStatus::Failed,
            error: Some("connection refused".to_owned()),
            version: None,
            transport: Some(McpTransport::Sse {
                url: "https://events.example.com/sse".to_owned(),
            }),
            scope: None,
            tools: Vec::new(),
        },
    ]
}

fn commands() -> Vec<SlashCommand> {
    vec![
        SlashCommand {
            name: "review".to_owned(),
            description: "Review the branch".to_owned(),
            kind: CommandKind::Skill,
        },
        SlashCommand {
            name: "compact".to_owned(),
            description: "Compact the conversation".to_owned(),
            kind: CommandKind::Command,
        },
    ]
}

fn details() -> AgentDetails {
    AgentDetails {
        tasks: vec![
            TaskListItem {
                content: "Fix the regex".to_owned(),
                status: TaskStatus::Completed,
                active_form: "Fixing the regex".to_owned(),
            },
            TaskListItem {
                content: "Add a test".to_owned(),
                status: TaskStatus::InProgress,
                active_form: "Adding a test".to_owned(),
            },
        ],
        background_tasks: vec![
            BackgroundTask {
                id: BackgroundTaskId::new("task-1".to_owned()),
                kind: BackgroundTaskKind::Command,
                description: "cargo watch".to_owned(),
                status: BackgroundTaskStatus::Running,
                started_at: at(1_700_000_100_000),
                ended_at: None,
                tool_calls: 0,
            },
            BackgroundTask {
                id: BackgroundTaskId::new("task-2".to_owned()),
                kind: BackgroundTaskKind::Agent,
                description: "Search the callers".to_owned(),
                status: BackgroundTaskStatus::Done,
                started_at: at(1_700_000_100_000),
                ended_at: Some(at(1_700_000_160_000)),
                tool_calls: 12,
            },
        ],
        changed_files: vec![ChangedFile {
            path: PathBuf::from("src/login.rs"),
            lines_added: 4,
            lines_removed: 1,
        }],
        context: Some(context_usage()),
        mcp_servers: mcp_servers(),
        commands: commands(),
    }
}

fn plan_usage() -> PlanUsage {
    PlanUsage {
        windows: BTreeMap::from([
            (
                UsageWindow::FiveHour,
                WindowUsage {
                    utilization: Some(0.25),
                    resets_at: Some(at(1_700_010_000_000)),
                    status: RateLimitStatus::Allowed,
                },
            ),
            (
                UsageWindow::SevenDay,
                WindowUsage {
                    utilization: None,
                    resets_at: None,
                    status: RateLimitStatus::AllowedWarning,
                },
            ),
        ]),
    }
}

fn settings() -> Settings {
    Settings {
        hidden_mcp_servers: BTreeSet::from(["events".to_owned()]),
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        agents: vec![summary(), idle_summary()],
        plan_usage: plan_usage(),
        settings: settings(),
        last_seq: Some(Seq::new(41)),
    }
}

/// One request per method, in the order of `Method::VARIANTS`.
fn requests() -> Vec<Request> {
    let agent = agent();
    vec![
        Request::AgentStart(StartParams {
            folder: PathBuf::from("/srv/projects/shop"),
            title: Some("Fix the login form".to_owned()),
            model: Some("opus".to_owned()),
            effort: Some(Effort::Medium),
            mode: Some(PermissionMode::Plan),
            prompt: Some(prompt()),
        }),
        Request::AgentPrompt(PromptParams {
            agent,
            prompt: prompt(),
        }),
        Request::AgentCancelQueued(CancelQueuedParams {
            agent,
            item: ItemId::new(12),
        }),
        Request::AgentAnswer(AnswerParams {
            agent,
            pending: pending(),
            answer: Answer::Approval(approval_answer()),
        }),
        Request::AgentInterrupt(AgentParams { agent }),
        Request::AgentStop(AgentParams { agent }),
        Request::AgentResume(AgentParams { agent }),
        Request::AgentRemove(AgentParams { agent }),
        Request::AgentHistory(HistoryParams {
            agent,
            before: Some(ItemId::new(40)),
            limit: 50,
        }),
        Request::AgentDetails(AgentParams { agent }),
        Request::AgentSetModel(SetModelParams {
            agent,
            model: "sonnet".to_owned(),
        }),
        Request::AgentSetEffort(SetEffortParams {
            agent,
            effort: Effort::Xhigh,
        }),
        Request::AgentSetMode(SetModeParams {
            agent,
            mode: PermissionMode::BypassPermissions,
        }),
        Request::AgentRename(RenameParams {
            agent,
            title: "Login form".to_owned(),
        }),
        Request::AgentMove(MoveParams {
            agent,
            before: Some(AgentId::new(4)),
        }),
        Request::AgentContextUsage(AgentParams { agent }),
        Request::AgentStopTask(StopTaskParams {
            agent,
            task: BackgroundTaskId::new("task-1".to_owned()),
        }),
        Request::AgentMcpStatus(AgentParams { agent }),
        Request::AgentMcpReconnect(McpServerParams {
            agent,
            server: "issues".to_owned(),
        }),
        Request::AgentMcpToggle(McpToggleParams {
            agent,
            server: "issues".to_owned(),
            enabled: false,
        }),
        Request::AgentCommands(AgentParams { agent }),
        Request::AgentReadFile(ReadFileParams {
            agent,
            path: PathBuf::from("src/login.rs"),
        }),
        Request::McpRemove(McpRemoveParams {
            agent,
            server: "issues".to_owned(),
            scope: McpScope::Project,
        }),
        Request::SettingsSet(settings()),
        Request::SessionsList,
        Request::SessionsResume(SessionResumeParams {
            session: session_id(),
        }),
    ]
}

fn answers() -> Vec<Answer> {
    vec![
        Answer::Approval(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        }),
        Answer::Approval(approval_answer()),
        Answer::Question {
            answers: question_answers(),
        },
        Answer::Plan(plan_answer()),
    ]
}

/// One body of every kind.
fn response_bodies() -> Vec<ResponseBody> {
    vec![
        ResponseBody::Done,
        ResponseBody::AgentStarted { agent: agent() },
        ResponseBody::History(HistoryPage {
            items: items(),
            has_older: true,
        }),
        ResponseBody::Details(details()),
        ResponseBody::ContextUsage(context_usage()),
        ResponseBody::McpServers {
            servers: mcp_servers(),
        },
        ResponseBody::Commands {
            commands: commands(),
        },
        ResponseBody::File {
            path: PathBuf::from("/srv/projects/shop/src/main.rs"),
            text: "fn main() {}\n".to_owned(),
        },
        ResponseBody::FileMatches {
            matches: vec![
                PathBuf::from("/srv/projects/shop/src/login.rs"),
                PathBuf::from("/srv/projects/shop_feature/src/login.rs"),
            ],
        },
        ResponseBody::Sessions {
            sessions: vec![PastSession {
                id: session_id(),
                title: "Fix the login form".to_owned(),
                folder: PathBuf::from("/srv/projects/shop"),
                last_activity: at(1_700_000_500_000),
                turns: 7,
            }],
        },
    ]
}

/// One event of every kind.
fn events() -> Vec<Event> {
    let mut events = vec![
        Event::AgentAdded(idle_summary()),
        Event::AgentUpdated(summary()),
        Event::AgentRemoved { agent: agent() },
    ];
    for item in items() {
        events.push(Event::TranscriptAppended {
            agent: agent(),
            item: item.clone(),
        });
        events.push(Event::TranscriptChanged {
            agent: agent(),
            item,
        });
    }
    events.extend([
        Event::DetailsChanged {
            agent: agent(),
            details: details(),
        },
        Event::DetailsChanged {
            agent: agent(),
            details: AgentDetails::default(),
        },
        Event::PlanUsageChanged(plan_usage()),
        Event::SettingsChanged(settings()),
    ]);
    events
}

fn client_messages() -> Vec<ClientMessage> {
    let mut messages = vec![
        ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            last_seq: None,
        },
        ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            last_seq: Some(Seq::new(41)),
        },
    ];
    let mut requests = requests();
    for answer in answers() {
        requests.push(Request::AgentAnswer(AnswerParams {
            agent: agent(),
            pending: pending(),
            answer,
        }));
    }
    for (raw, request) in (1..).zip(requests) {
        messages.push(ClientMessage::Request {
            id: RequestId::new(raw),
            request,
        });
    }
    messages
}

fn server_messages() -> Vec<ServerMessage> {
    let mut messages = vec![
        ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            catchup: Catchup::Replay,
        },
        ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            catchup: Catchup::Snapshot(snapshot()),
        },
        ServerMessage::Refused {
            reason: "the daemon speaks protocol 2".to_owned(),
        },
    ];
    for (raw, body) in (1..).zip(response_bodies()) {
        messages.push(ServerMessage::Response {
            id: RequestId::new(raw),
            result: ResponseResult::Ok(body),
        });
    }
    for &code in ErrorCode::VARIANTS {
        messages.push(ServerMessage::Response {
            id: RequestId::new(99),
            result: ResponseResult::Error(ErrorBody::new(code, "it failed".to_owned())),
        });
    }
    for (raw, event) in (42..).zip(events()) {
        messages.push(ServerMessage::Event(EventRecord {
            seq: Seq::new(raw),
            event,
        }));
    }
    messages
}

/// Serializes, deserializes, and checks that both the value and its JSON come back unchanged.
fn assert_round_trips<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
    let json = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&json).unwrap_or_else(|error| panic!("{json}: {error}"));
    assert_eq!(&back, value, "{json}");
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
}

/// Checks that `value` serializes to `expected`, and that `expected` deserializes to `value`.
fn assert_wire<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T, expected: Value) {
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
    let back: T = serde_json::from_value(expected).unwrap();
    assert_eq!(&back, value);
}

/// The JSON text of each variant of a word enum.
fn words<T: Serialize + VariantArray>() -> Vec<String> {
    let mut words = Vec::new();
    for variant in T::VARIANTS {
        let Value::String(word) = serde_json::to_value(variant).unwrap() else {
            panic!("a word enum serializes to a string");
        };
        words.push(word);
    }
    words
}

/// The value of the `key` field of each value's JSON object.
fn tags<T: Serialize>(values: &[T], key: &str) -> Vec<String> {
    let mut tags = Vec::new();
    for value in values {
        let json = serde_json::to_value(value).unwrap();
        let Some(Value::String(tag)) = json.get(key) else {
            panic!("no `{key}` in {json}");
        };
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }
    tags
}

#[test]
fn every_client_message_round_trips() {
    for message in client_messages() {
        assert_round_trips(&message);
    }
}

#[test]
fn every_server_message_round_trips() {
    for message in server_messages() {
        assert_round_trips(&message);
    }
}

#[test]
fn the_samples_cover_every_method() {
    let methods: Vec<Method> = requests().iter().map(Request::method).collect();

    assert_eq!(methods, Method::VARIANTS);
}

#[test]
fn method_names_are_pinned() {
    let names: Vec<&str> = Method::VARIANTS
        .iter()
        .map(|&method| method.as_str())
        .collect();

    assert_eq!(
        names,
        [
            "agent.start",
            "agent.prompt",
            "agent.cancel_queued",
            "agent.answer",
            "agent.interrupt",
            "agent.stop",
            "agent.resume",
            "agent.remove",
            "agent.history",
            "agent.details",
            "agent.set_model",
            "agent.set_effort",
            "agent.set_mode",
            "agent.rename",
            "agent.move",
            "agent.context_usage",
            "agent.stop_task",
            "agent.mcp_status",
            "agent.mcp_reconnect",
            "agent.mcp_toggle",
            "agent.commands",
            "agent.read_file",
            "mcp.remove",
            "settings.set",
            "sessions.list",
            "sessions.resume",
        ]
    );
}

#[test]
fn each_request_writes_its_method_name() {
    let written: Vec<String> = tags(&requests(), "method");
    let names: Vec<String> = Method::VARIANTS.iter().map(ToString::to_string).collect();

    assert_eq!(written, names);
}

#[test]
fn message_and_body_tags_are_pinned() {
    assert_eq!(tags(&client_messages(), "type"), ["hello", "request"]);
    assert_eq!(
        tags(&server_messages(), "type"),
        ["welcome", "refused", "response", "event"]
    );
    assert_eq!(
        tags(&[Catchup::Replay, Catchup::Snapshot(snapshot())], "type"),
        ["replay", "snapshot"]
    );
    assert_eq!(
        tags(&response_bodies(), "type"),
        [
            "done",
            "agent_started",
            "history",
            "details",
            "context_usage",
            "mcp_servers",
            "commands",
            "file",
            "file_matches",
            "sessions",
        ]
    );
    assert_eq!(
        tags(&events(), "type"),
        [
            "agent_added",
            "agent_updated",
            "agent_removed",
            "transcript_appended",
            "transcript_changed",
            "details_changed",
            "plan_usage_changed",
            "settings_changed",
        ]
    );
    assert_eq!(
        tags(&bodies(), "type"),
        [
            "user_message",
            "assistant_text",
            "thinking",
            "tool_call",
            "subagent",
            "approval",
            "question",
            "plan",
            "turn_end",
            "notice",
        ]
    );
    assert_eq!(tags(&answers(), "type"), ["approval", "question", "plan"]);
    assert_eq!(
        tags(
            &[
                AnswerState::Waiting,
                AnswerState::Answered(plan_answer()),
                AnswerState::Cancelled,
            ],
            "state",
        ),
        ["waiting", "answered", "cancelled"]
    );
    let transports: Vec<McpTransport> = mcp_servers()
        .into_iter()
        .filter_map(|server| server.transport)
        .collect();
    assert_eq!(tags(&transports, "type"), ["stdio", "http", "sse"]);
}

#[test]
fn words_are_pinned() {
    assert_eq!(
        words::<AgentStatus>(),
        ["working", "needs_you", "idle", "exited"]
    );
    assert_eq!(
        words::<PermissionMode>(),
        [
            "default",
            "acceptEdits",
            "plan",
            "auto",
            "dontAsk",
            "bypassPermissions"
        ]
    );
    assert_eq!(words::<Effort>(), ["low", "medium", "high", "xhigh", "max"]);
    assert_eq!(
        words::<ImageMediaType>(),
        ["image/png", "image/jpeg", "image/gif", "image/webp"]
    );
    assert_eq!(words::<ToolCallStatus>(), ["running", "done", "failed"]);
    assert_eq!(words::<ApprovalDecision>(), ["allow", "deny"]);
    assert_eq!(
        words::<PlanDecision>(),
        ["accept_edits", "ask_before_edits", "keep_planning"]
    );
    assert_eq!(
        words::<TaskStatus>(),
        ["pending", "in_progress", "completed"]
    );
    assert_eq!(words::<BackgroundTaskKind>(), ["agent", "command"]);
    assert_eq!(
        words::<BackgroundTaskStatus>(),
        ["running", "stopped", "done", "failed"]
    );
    assert_eq!(
        words::<McpServerStatus>(),
        ["connected", "failed", "needs_auth", "pending", "disabled"]
    );
    assert_eq!(
        words::<McpScope>(),
        [
            "user",
            "project",
            "local",
            "dynamic",
            "enterprise",
            "managed",
            "claude_ai"
        ]
    );
    assert_eq!(words::<CommandKind>(), ["skill", "command"]);
    assert_eq!(
        words::<UsageWindow>(),
        [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "overage"
        ]
    );
    assert_eq!(
        words::<RateLimitStatus>(),
        ["allowed", "allowed_warning", "rejected"]
    );
    assert_eq!(
        words::<ErrorCode>(),
        [
            "invalid_request",
            "not_found",
            "agent_exited",
            "agent_running",
            "file_unreadable",
            "claude_failed",
            "timeout",
            "internal",
        ]
    );
}

#[test]
fn hello_is_one_exact_line() {
    let hello = ClientMessage::Hello {
        protocol_version: 1,
        last_seq: Some(Seq::new(41)),
    };

    let json = serde_json::to_string(&hello).unwrap();

    assert_eq!(
        json,
        r#"{"type":"hello","protocol_version":1,"last_seq":41}"#
    );
}

#[test]
fn a_prompt_request_on_the_wire() {
    let message = ClientMessage::Request {
        id: RequestId::new(7),
        request: Request::AgentPrompt(PromptParams {
            agent: agent(),
            prompt: prompt(),
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "request",
            "id": 7,
            "request": {
                "method": "agent.prompt",
                "params": {
                    "agent": 3,
                    "prompt": {
                        "text": "Why does the form reject valid emails?",
                        "images": [{"media_type": "image/png", "data": "iVBORw0KGgo="}],
                    },
                },
            },
        }),
    );
}

#[test]
fn a_rename_request_on_the_wire() {
    let message = ClientMessage::Request {
        id: RequestId::new(9),
        request: Request::AgentRename(RenameParams {
            agent: agent(),
            title: "Login form".to_owned(),
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "request",
            "id": 9,
            "request": {
                "method": "agent.rename",
                "params": {"agent": 3, "title": "Login form"},
            },
        }),
    );
}

#[test]
fn move_requests_on_the_wire() {
    let request = |before| ClientMessage::Request {
        id: RequestId::new(10),
        request: Request::AgentMove(MoveParams {
            agent: agent(),
            before,
        }),
    };

    assert_wire(
        &request(Some(AgentId::new(4))),
        json!({
            "type": "request",
            "id": 10,
            "request": {"method": "agent.move", "params": {"agent": 3, "before": 4}},
        }),
    );
    assert_wire(
        &request(None),
        json!({
            "type": "request",
            "id": 10,
            "request": {"method": "agent.move", "params": {"agent": 3, "before": null}},
        }),
    );
}

#[test]
fn a_question_answer_on_the_wire() {
    let message = ClientMessage::Request {
        id: RequestId::new(8),
        request: Request::AgentAnswer(AnswerParams {
            agent: agent(),
            pending: pending(),
            answer: Answer::Question {
                answers: question_answers(),
            },
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "request",
            "id": 8,
            "request": {
                "method": "agent.answer",
                "params": {
                    "agent": 3,
                    "pending": "toolu_01",
                    "answer": {
                        "type": "question",
                        "answers": [
                            {"selected": ["SQLite"], "other": "and keep the fixtures small"},
                        ],
                    },
                },
            },
        }),
    );
}

#[test]
fn a_request_without_params_has_no_params_field() {
    let message = ClientMessage::Request {
        id: RequestId::new(9),
        request: Request::SessionsList,
    };

    let json = serde_json::to_string(&message).unwrap();

    assert_eq!(
        json,
        r#"{"type":"request","id":9,"request":{"method":"sessions.list"}}"#
    );
}

#[test]
fn a_welcome_with_a_snapshot_on_the_wire() {
    let message = ServerMessage::Welcome {
        protocol_version: 1,
        catchup: Catchup::Snapshot(Snapshot {
            agents: Vec::new(),
            plan_usage: plan_usage(),
            settings: settings(),
            last_seq: None,
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "welcome",
            "protocol_version": 1,
            "catchup": {
                "type": "snapshot",
                "agents": [],
                "plan_usage": {
                    "windows": {
                        "five_hour": {
                            "utilization": 0.25,
                            "resets_at": 1_700_010_000_000_u64,
                            "status": "allowed",
                        },
                        "seven_day": {
                            "utilization": null,
                            "resets_at": null,
                            "status": "allowed_warning",
                        },
                    },
                },
                "settings": {"hidden_mcp_servers": ["events"]},
                "last_seq": null,
            },
        }),
    );
}

#[test]
fn responses_on_the_wire() {
    let done = ServerMessage::Response {
        id: RequestId::new(7),
        result: ResponseResult::Ok(ResponseBody::Done),
    };
    let failed = ServerMessage::Response {
        id: RequestId::new(8),
        result: ResponseResult::Error(ErrorBody::new(ErrorCode::NotFound, "no agent 9".to_owned())),
    };

    assert_eq!(
        serde_json::to_string(&done).unwrap(),
        r#"{"type":"response","id":7,"result":{"ok":{"type":"done"}}}"#
    );
    assert_eq!(
        serde_json::to_string(&failed).unwrap(),
        r#"{"type":"response","id":8,"result":{"error":{"code":"not_found","message":"no agent 9"}}}"#
    );
}

#[test]
fn an_agent_update_on_the_wire() {
    let message = ServerMessage::Event(EventRecord {
        seq: Seq::new(42),
        event: Event::AgentUpdated(summary()),
    });

    assert_wire(
        &message,
        json!({
            "type": "event",
            "seq": 42,
            "event": {
                "type": "agent_updated",
                "id": 3,
                "title": "Fix the login form",
                "folder": "/srv/projects/shop",
                "status": "needs_you",
                "activity": "Waiting for approval: Bash",
                "pending_count": 1,
                "model": "opus",
                "effort": "high",
                "effort_is_default": true,
                "mode": "acceptEdits",
                "task_progress": {"done": 3, "total": 5},
                "context_percent": 42,
                "cache_expires_at": 1_700_000_300_000_u64,
                "turns": 7,
                "cost": 1.25,
                "started_at": 1_700_000_000_000_u64,
                "session": "0b7e1c2a-3d4f-4a5b-8c6d-7e8f9a0b1c2d",
                "position": 2048,
            },
        }),
    );
}

#[test]
fn a_summary_logged_before_positions_reads_position_zero() {
    let mut logged = serde_json::to_value(summary()).unwrap();
    logged.as_object_mut().unwrap().remove("position");

    let read: AgentSummary = serde_json::from_value(logged).unwrap();

    assert_eq!(read.position, Position::new(0));
}

#[test]
fn a_summary_saved_without_its_effort_source_still_loads() {
    let mut saved = serde_json::to_value(summary()).unwrap();
    saved.as_object_mut().unwrap().remove("effort_is_default");

    let loaded: AgentSummary = serde_json::from_value(saved).unwrap();

    assert_eq!(loaded.effort, Some(Effort::High));
    assert!(!loaded.effort_is_default);
}

#[test]
fn a_tool_call_on_the_wire() {
    let message = ServerMessage::Event(EventRecord {
        seq: Seq::new(43),
        event: Event::TranscriptChanged {
            agent: agent(),
            item: TranscriptItem {
                id: ItemId::new(16),
                parent: Some(ItemId::new(14)),
                body: ItemBody::ToolCall(tool_call(ToolCallStatus::Failed)),
            },
        },
    });

    assert_wire(
        &message,
        json!({
            "type": "event",
            "seq": 43,
            "event": {
                "type": "transcript_changed",
                "agent": 3,
                "item": {
                    "id": 16,
                    "parent": 14,
                    "body": {
                        "type": "tool_call",
                        "tool": "Bash",
                        "summary": "cargo test",
                        "input": {"command": "cargo test", "description": "Run the tests"},
                        "status": "failed",
                        "output": "test result: FAILED. 3 passed; 1 failed",
                        "exit_code": 101,
                        "edit": null,
                    },
                },
            },
        }),
    );
}

#[test]
fn an_approval_and_a_turn_end_on_the_wire() {
    let approval = ItemBody::Approval(Approval {
        pending: pending(),
        tool: "Edit".to_owned(),
        summary: "src/login.rs".to_owned(),
        input: json!({"file_path": "src/login.rs"}),
        edit: Some(edit()),
        reason: None,
        answer: AnswerState::Answered(approval_answer()),
        tool_call: Some(ItemId::new(16)),
    });
    let turn_end = ItemBody::TurnEnd(TurnEnd {
        duration: Duration::from_millis(83_250),
        cost: CostUsd::new(0.42),
        usage: TokenUsage::default(),
        error: None,
        stopped: false,
    });

    assert_wire(
        &approval,
        json!({
            "type": "approval",
            "pending": "toolu_01",
            "tool": "Edit",
            "summary": "src/login.rs",
            "input": {"file_path": "src/login.rs"},
            "edit": {
                "path": "src/login.rs",
                "old_text": "let valid = false;",
                "new_text": "let valid = check(email);",
            },
            "reason": null,
            "answer": {
                "state": "answered",
                "answer": {"decision": "deny", "message": "Run only the login tests"},
            },
            "tool_call": 16,
        }),
    );
    assert_wire(
        &turn_end,
        json!({
            "type": "turn_end",
            "duration_ms": 83_250,
            "cost": 0.42,
            "usage": {"input": 0, "output": 0, "cache_creation": 0, "cache_read": 0},
            "error": null,
            "stopped": false,
        }),
    );
}

#[test]
fn a_notice_with_its_detail_on_the_wire_and_one_saved_without_it() {
    let notice = ItemBody::Notice {
        text: "Conversation summary".to_owned(),
        detail: Some("The user asked for a parser.".to_owned()),
    };
    let saved = r#"{"type":"notice","text":"Conversation compacted"}"#;

    assert_wire(
        &notice,
        json!({
            "type": "notice",
            "text": "Conversation summary",
            "detail": "The user asked for a parser.",
        }),
    );
    assert_eq!(
        serde_json::from_str::<ItemBody>(saved).unwrap(),
        ItemBody::Notice {
            text: "Conversation compacted".to_owned(),
            detail: None,
        }
    );
}

#[test]
fn an_approval_saved_without_its_tool_call_still_loads() {
    let saved = r#"{"type":"approval","pending":"toolu_01","tool":"Bash","summary":"ls","input":{"command":"ls"},"edit":null,"reason":null,"answer":{"state":"waiting"}}"#;

    let ItemBody::Approval(approval) = serde_json::from_str::<ItemBody>(saved).unwrap() else {
        panic!("not an approval");
    };

    assert_eq!(approval.tool_call, None);
    assert_eq!(approval.answer, AnswerState::Waiting);
}

#[test]
fn a_stopped_turn_end_on_the_wire_and_one_saved_without_the_field() {
    let stopped = ItemBody::TurnEnd(TurnEnd {
        duration: Duration::from_millis(6_100),
        cost: CostUsd::new(0.01),
        usage: TokenUsage::default(),
        error: None,
        stopped: true,
    });
    let saved = r#"{"type":"turn_end","duration_ms":6100,"cost":0.01,"usage":{"input":0,"output":0,"cache_creation":0,"cache_read":0},"error":"Interrupted"}"#;

    assert_wire(
        &stopped,
        json!({
            "type": "turn_end",
            "duration_ms": 6_100,
            "cost": 0.01,
            "usage": {"input": 0, "output": 0, "cache_creation": 0, "cache_read": 0},
            "error": null,
            "stopped": true,
        }),
    );
    let ItemBody::TurnEnd(saved) = serde_json::from_str::<ItemBody>(saved).unwrap() else {
        panic!("not a turn end");
    };
    assert!(!saved.stopped);
    assert_eq!(saved.error.as_deref(), Some("Interrupted"));
}

#[test]
fn a_missing_optional_field_reads_as_none() {
    let hello: ClientMessage =
        serde_json::from_str(r#"{"type":"hello","protocol_version":1}"#).unwrap();

    assert_eq!(
        hello,
        ClientMessage::Hello {
            protocol_version: 1,
            last_seq: None,
        }
    );
}

#[test]
fn unknown_fields_are_ignored() {
    let hello: ClientMessage =
        serde_json::from_str(r#"{"type":"hello","protocol_version":1,"last_seq":2,"extra":true}"#)
            .unwrap();

    assert_eq!(
        hello,
        ClientMessage::Hello {
            protocol_version: 1,
            last_seq: Some(Seq::new(2)),
        }
    );
}

#[test]
fn settings_missing_a_field_take_its_default() {
    let settings: Settings = serde_json::from_str("{}").unwrap();

    assert_eq!(settings, Settings::default());
}

#[test]
fn an_unknown_method_does_not_decode() {
    let result = serde_json::from_str::<Request>(r#"{"method":"agent.fly","params":{}}"#);

    let error = result.unwrap_err().to_string();
    assert!(error.contains("unknown method `agent.fly`"), "{error}");
}

#[test]
fn params_that_do_not_fit_the_method_do_not_decode() {
    let result = serde_json::from_str::<Request>(r#"{"method":"agent.stop","params":{}}"#);

    let error = result.unwrap_err().to_string();
    assert!(error.contains("missing field `agent`"), "{error}");
}

#[test]
fn a_response_result_converts_to_a_result() {
    let body = ResponseResult::Ok(ResponseBody::Done);
    let error = ResponseResult::Error(ErrorBody::new(ErrorCode::Timeout, "slow".to_owned()));

    assert_eq!(body.clone().into_result(), Ok(ResponseBody::Done));
    assert_eq!(ResponseResult::from(body.clone().into_result()), body);
    assert_eq!(
        error.into_result(),
        Err(ErrorBody::new(ErrorCode::Timeout, "slow".to_owned()))
    );
}

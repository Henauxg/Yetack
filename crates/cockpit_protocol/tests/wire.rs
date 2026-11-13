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
    Account, AccountAddParams, AccountChangeParams, AccountId, AccountParams, AccountPlanUsage,
    AccountState, AgentDetails, AgentHistory, AgentId, AgentLoad, AgentParams, AgentStatus,
    AgentSummary, Answer, AnswerParams, AnswerState, Approval, ApprovalAnswer, ApprovalDecision,
    AskedQuestion, BackgroundTask, BackgroundTaskId, BackgroundTaskKind, BackgroundTaskStatus,
    Catchup, ChangedFile, ClaudeRoute, ClaudeSessionId, ClientMessage, CommandKind, CommandLoad,
    Completeness, ContextCategory, ContextUsage, Cores, CostUsd, CredentialSource, DirEntry,
    DirListing, Effort, EffortSource, EntryKind, EntryLink, EnvironmentCredentials, ErrorBody,
    ErrorCode, Event, EventRecord, FileEdit, FindFilesParams, FolderParams, FoldersListParams,
    GitIgnore, HistoryPage, HistoryParams, Image, ImageMediaType, ItemBody, ItemId, ListDirParams,
    LoadAverage, LoadHistory, MachineLoad, Mark, MarkParams, McpRemoveParams, McpScope, McpServer,
    McpServerParams, McpServerStatus, McpToggleAction, McpToggleParams, McpTool, McpTransport,
    Method, MoveParams, OlderItems, PROTOCOL_VERSION, PastSession, PendingId, PermissionMode, Plan,
    PlanAnswer, PlanDecision, PlanUsage, Position, ProcessId, Prompt, PromptDelivery, PromptId,
    PromptParams, Question, QuestionAnswer, QuestionOption, QueuedPromptParams, RateLimitStatus,
    ReadFileParams, ReleaseVersion, RenameParams, Request, RequestId, ResourceUse, ResponseBody,
    ResponseResult, Selection, Seq, ServerLoad, ServerMessage, ServerPoint, SessionResumeParams,
    SetEffortParams, SetModeParams, SetModelParams, Settings, SignIn, SignInWait, SlashCommand,
    Snapshot, StartParams, StopProcessParams, StopTaskParams, Subagent, SubagentRun,
    SubagentStatus, SubagentToolCall, TaskListItem, TaskProgress, TaskStatus, Timestamp,
    TokenUsage, ToolCall, ToolCallStatus, TranscriptItem, TurnEnd, TurnOutcome, UsageWindow,
    UserMessage, WaitingPrompt, WaitingState, WindowUsage,
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
        effort_source: EffortSource::ClaudeCodeDefault,
        mode: PermissionMode::AcceptEdits,
        task_progress: Some(TaskProgress { done: 3, total: 5 }),
        context_percent: Some(42),
        cache_expires_at: Some(at(1_700_000_300_000)),
        turns: 7,
        cost: CostUsd::new(1.25),
        started_at: at(1_700_000_000_000),
        last_activity_at: Some(at(1_700_000_250_000)),
        last_output_at: None,
        turn_started_at: Some(at(1_700_000_200_000)),
        session: Some(session_id()),
        position: Position::new(2048),
        account: work_account().id,
        sign_in_wait: SignInWait::Waiting,
        background_running: 2,
        mark: Mark::ToHandle,
        repository: None,
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
        effort_source: EffortSource::Chosen,
        mode: PermissionMode::Default,
        task_progress: None,
        context_percent: None,
        cache_expires_at: None,
        turns: 0,
        cost: CostUsd::default(),
        started_at: at(1_700_000_000_000),
        last_activity_at: Some(at(1_700_000_000_000)),
        last_output_at: None,
        turn_started_at: None,
        session: None,
        position: Position::new(1024),
        account: AccountId::DEFAULT,
        sign_in_wait: SignInWait::NotWaiting,
        background_running: 0,
        mark: Mark::Unmarked,
        repository: None,
    }
}

fn default_account() -> Account {
    Account {
        id: AccountId::DEFAULT,
        name: "Personal".to_owned(),
        folder: PathBuf::from("/home/sam/.claude"),
        credentials: CredentialSource::SignIn,
    }
}

fn work_account() -> Account {
    Account {
        id: AccountId::new(2),
        name: "Work".to_owned(),
        folder: PathBuf::from("/home/sam/.claude-work"),
        credentials: CredentialSource::Environment,
    }
}

fn work_account_state() -> AccountState {
    AccountState {
        account: work_account(),
        sign_in: SignIn::NotSignedIn,
        sign_in_command: "CLAUDE_CONFIG_DIR='/home/sam/.claude-work' claude".to_owned(),
        route: ClaudeRoute::Bedrock,
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
        selection: Selection::Single,
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
            sent_at: Some(at(1_700_000_000_000)),
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
            outcome: TurnOutcome::Failed,
            error: Some("The turn reached the maximum number of turns".to_owned()),
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
                call: Some(ItemId::new(3)),
            },
            BackgroundTask {
                id: BackgroundTaskId::new("task-2".to_owned()),
                kind: BackgroundTaskKind::Agent,
                description: "Search the callers".to_owned(),
                status: BackgroundTaskStatus::Done,
                started_at: at(1_700_000_100_000),
                ended_at: Some(at(1_700_000_160_000)),
                tool_calls: 12,
                call: None,
            },
        ],
        changed_files: vec![ChangedFile {
            path: PathBuf::from("src/login.rs"),
            lines_added: 4,
            lines_removed: 1,
        }],
        context: Some(context_usage()),
        mcp_servers: mcp_servers(),
        subagents: subagents(),
        waiting_prompts: vec![
            WaitingPrompt {
                id: PromptId::new(4),
                text: "Also check the signup form".to_owned(),
                image_count: 1,
                state: WaitingState::Sent,
                sent_at: Some(at(1_700_000_100_000)),
            },
            WaitingPrompt {
                id: PromptId::new(5),
                text: "Then run the tests".to_owned(),
                image_count: 0,
                state: WaitingState::Queued,
                sent_at: None,
            },
        ],
    }
}

/// A background subagent that waits for a foreground one it started.
fn subagents() -> Vec<Subagent> {
    vec![
        Subagent {
            item: ItemId::new(40),
            parent: None,
            description: "Search the callers".to_owned(),
            agent_type: Some("Explore".to_owned()),
            run: SubagentRun::Background,
            status: SubagentStatus::Running,
            started_at: at(1_700_000_100_000),
            ended_at: None,
            tokens: Some(18_400),
            tool_calls: 7,
            last_tool: Some(SubagentToolCall {
                item: Some(ItemId::new(44)),
                tool: "Agent".to_owned(),
                summary: "Check the lockfile".to_owned(),
            }),
            error: None,
            turn: 3,
            task: Some(BackgroundTaskId::new("task-2".to_owned())),
        },
        Subagent {
            item: ItemId::new(44),
            parent: Some(ItemId::new(40)),
            description: "Check the lockfile".to_owned(),
            agent_type: None,
            run: SubagentRun::Foreground,
            status: SubagentStatus::Failed,
            started_at: at(1_700_000_130_000),
            ended_at: Some(at(1_700_000_142_000)),
            tokens: None,
            tool_calls: 2,
            last_tool: None,
            error: Some("API error 529: overloaded".to_owned()),
            turn: 3,
            task: None,
        },
    ]
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
        accounts: vec![default_account(), work_account()],
        other_plan_usage: vec![AccountPlanUsage {
            account: work_account().id,
            usage: plan_usage(),
        }],
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
            account: work_account().id,
        }),
        Request::AgentPrompt(PromptParams {
            agent,
            prompt: prompt(),
            delivery: PromptDelivery::Queue,
        }),
        Request::AgentCancelQueued(QueuedPromptParams {
            agent,
            prompt: PromptId::new(5),
        }),
        Request::AgentSendQueued(QueuedPromptParams {
            agent,
            prompt: PromptId::new(5),
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
        Request::AgentClose(AgentParams { agent }),
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
        Request::AgentMark(MarkParams {
            agent,
            mark: Mark::ToHandle,
        }),
        Request::AgentFork(FolderParams {
            agent,
            folder: PathBuf::from("/work/app_fork"),
        }),
        Request::AgentChangeFolder(FolderParams {
            agent,
            folder: PathBuf::from("/work/app_moved"),
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
        Request::AgentStopProcess(StopProcessParams {
            agent,
            pid: ProcessId::new(4242),
        }),
        Request::AgentMcpStatus(AgentParams { agent }),
        Request::AgentMcpReconnect(McpServerParams {
            agent,
            server: "issues".to_owned(),
        }),
        Request::AgentMcpToggle(McpToggleParams {
            agent,
            server: "issues".to_owned(),
            action: McpToggleAction::Disable,
        }),
        Request::AgentCommands(AgentParams { agent }),
        Request::AgentReadFile(ReadFileParams {
            agent,
            path: PathBuf::from("src/login.rs"),
        }),
        Request::AgentListDir(ListDirParams {
            agent,
            path: PathBuf::from("src"),
        }),
        Request::AgentFindFiles(FindFilesParams {
            agent,
            name: "login".to_owned(),
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
            account: work_account().id,
        }),
        Request::FoldersList(FoldersListParams {
            path: PathBuf::from("/srv/projects"),
        }),
        Request::MachineLoad,
        Request::AccountsList,
        Request::AccountsAdd(AccountAddParams {
            name: "Work".to_owned(),
            folder: None,
            credentials: CredentialSource::SignIn,
        }),
        Request::AccountsChange(AccountChangeParams {
            account: work_account().id,
            name: Some("Client work".to_owned()),
            credentials: None,
        }),
        Request::AccountsRemove(AccountParams {
            account: work_account().id,
        }),
    ]
}

fn dir_listing() -> DirListing {
    DirListing {
        path: PathBuf::from("/srv/projects/shop/src"),
        entries: vec![
            DirEntry {
                name: "forms".to_owned(),
                kind: EntryKind::Folder,
                size: None,
                link: EntryLink::Direct,
                ignore: GitIgnore::Ignored,
            },
            DirEntry {
                name: "login.rs".to_owned(),
                kind: EntryKind::File,
                size: Some(2_048),
                link: EntryLink::Direct,
                ignore: GitIgnore::Kept,
            },
            DirEntry {
                name: "shared".to_owned(),
                kind: EntryKind::Other,
                size: None,
                link: EntryLink::SymbolicLink,
                ignore: GitIgnore::Kept,
            },
        ],
        completeness: Completeness::Complete,
    }
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

/// A server of 64 cores where one agent builds and another waits, over two samples.
fn machine_load() -> MachineLoad {
    MachineLoad {
        taken_at: at(1_700_000_003_000),
        server: ServerLoad {
            cores: 64,
            cpu: Cores::new(46.5),
            memory_used_bytes: 312_000_000_000,
            memory_total_bytes: 540_000_000_000,
            load_average: Some(LoadAverage {
                one_minute: 43.5,
                five_minutes: 38.25,
                fifteen_minutes: 30.0,
            }),
        },
        agents: vec![
            AgentLoad {
                agent: agent(),
                cpu: Cores::new(18.5),
                memory_bytes: 22_000_000_000,
                processes: 14,
                heaviest: vec![CommandLoad {
                    pid: ProcessId::new(4242),
                    command: "rustc --crate-name shop_api --edition=2024".to_owned(),
                    cpu: Cores::new(7.75),
                    memory_bytes: 6_000_000_000,
                }],
            },
            AgentLoad {
                agent: AgentId::new(4),
                cpu: Cores::new(0.0),
                memory_bytes: 400_000_000,
                processes: 1,
                heaviest: Vec::new(),
            },
        ],
        others: ResourceUse {
            cpu: Cores::new(28.0),
            memory_bytes: 289_600_000_000,
        },
        history: LoadHistory {
            server: vec![
                ServerPoint {
                    taken_at: at(1_700_000_000_000),
                    cpu: Cores::new(44.0),
                },
                ServerPoint {
                    taken_at: at(1_700_000_003_000),
                    cpu: Cores::new(46.5),
                },
            ],
            agents: vec![
                AgentHistory {
                    agent: agent(),
                    cpu: vec![Some(Cores::new(16.0)), Some(Cores::new(18.5))],
                },
                AgentHistory {
                    agent: AgentId::new(4),
                    cpu: vec![None, Some(Cores::new(0.0))],
                },
            ],
        },
    }
}

/// One body of every kind.
fn response_bodies() -> Vec<ResponseBody> {
    vec![
        ResponseBody::Done,
        ResponseBody::AgentStarted { agent: agent() },
        ResponseBody::History(HistoryPage {
            items: items(),
            older_items: OlderItems::Exist,
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
            path: Some(PathBuf::from("/srv/projects/shop/src/main.rs")),
            text: "fn main() {}\n".to_owned(),
            edits: Some(vec![edit()]),
        },
        ResponseBody::Image {
            path: PathBuf::from("/srv/projects/shop/docs/cart.png"),
            image: image(),
        },
        ResponseBody::Directory(dir_listing()),
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
                account: work_account().id,
            }],
        },
        ResponseBody::MachineLoad(machine_load()),
        ResponseBody::Accounts {
            accounts: vec![
                AccountState {
                    account: default_account(),
                    sign_in: SignIn::SignedIn,
                    sign_in_command: "claude".to_owned(),
                    route: ClaudeRoute::Anthropic,
                },
                work_account_state(),
            ],
            environment_credentials: EnvironmentCredentials::Set,
        },
        ResponseBody::Account(work_account_state()),
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
        Event::CommandsChanged {
            agent: agent(),
            commands: commands(),
        },
        Event::PlanUsageChanged(plan_usage()),
        Event::SettingsChanged(settings()),
        Event::AccountPlanUsageChanged(AccountPlanUsage {
            account: work_account().id,
            usage: plan_usage(),
        }),
        Event::AccountsChanged {
            accounts: vec![default_account(), work_account()],
        },
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
            daemon_version: ReleaseVersion::new("0.1.0".to_owned()),
            catchup: Catchup::Replay,
        },
        ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: ReleaseVersion::new("0.1.0".to_owned()),
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
            "agent.send_queued",
            "agent.answer",
            "agent.interrupt",
            "agent.stop",
            "agent.resume",
            "agent.remove",
            "agent.close",
            "agent.history",
            "agent.details",
            "agent.set_model",
            "agent.set_effort",
            "agent.set_mode",
            "agent.rename",
            "agent.mark",
            "agent.fork",
            "agent.change_folder",
            "agent.move",
            "agent.context_usage",
            "agent.stop_task",
            "agent.stop_process",
            "agent.mcp_status",
            "agent.mcp_reconnect",
            "agent.mcp_toggle",
            "agent.commands",
            "agent.read_file",
            "agent.list_dir",
            "agent.find_files",
            "mcp.remove",
            "settings.set",
            "sessions.list",
            "sessions.resume",
            "folders.list",
            "machine.load",
            "accounts.list",
            "accounts.add",
            "accounts.change",
            "accounts.remove",
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
            "image",
            "directory",
            "file_matches",
            "sessions",
            "machine_load",
            "accounts",
            "account",
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
            "commands_changed",
            "plan_usage_changed",
            "settings_changed",
            "account_plan_usage_changed",
            "accounts_changed",
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
    assert_eq!(words::<EntryKind>(), ["folder", "file", "other"]);
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
    assert_eq!(words::<SubagentRun>(), ["foreground", "background"]);
    assert_eq!(
        words::<SubagentStatus>(),
        ["running", "needs_you", "done", "failed", "stopped"]
    );
    assert_eq!(words::<PromptDelivery>(), ["steer", "queue"]);
    assert_eq!(words::<WaitingState>(), ["queued", "sent"]);
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
            "unsupported",
            "account_in_use",
            "internal",
        ]
    );
    assert_eq!(words::<SignIn>(), ["signed_in", "not_signed_in", "unknown"]);
    assert_eq!(
        words::<ClaudeRoute>(),
        ["anthropic", "bedrock", "vertex", "foundry", "custom_server"]
    );
    assert_eq!(words::<CredentialSource>(), ["sign_in", "environment"]);
    assert_eq!(words::<EnvironmentCredentials>(), ["set", "unset"]);
    assert_eq!(words::<EffortSource>(), ["chosen", "claude_code_default"]);
    assert_eq!(words::<SignInWait>(), ["not_waiting", "waiting"]);
    assert_eq!(words::<Mark>(), ["unmarked", "to_handle", "to_handle_kept"]);
    assert_eq!(words::<Selection>(), ["single", "multiple"]);
    assert_eq!(
        words::<TurnOutcome>(),
        ["completed", "stopped", "failed", "not_signed_in"]
    );
    assert_eq!(words::<Completeness>(), ["complete", "truncated"]);
    assert_eq!(words::<EntryLink>(), ["direct", "symbolic_link"]);
    assert_eq!(words::<GitIgnore>(), ["kept", "ignored"]);
    assert_eq!(words::<OlderItems>(), ["exist", "none"]);
    assert_eq!(words::<McpToggleAction>(), ["enable", "disable"]);
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
            delivery: PromptDelivery::Steer,
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
                    "delivery": "steer",
                },
            },
        }),
    );
}

#[test]
fn an_image_type_comes_from_the_file_names_extension() {
    let of = ImageMediaType::from_file_name;

    assert_eq!(of("shot.PNG"), Some(ImageMediaType::Png));
    assert_eq!(of(r"C:\shots\sheet05.jpg"), Some(ImageMediaType::Jpeg));
    assert_eq!(of("photo.jpeg"), Some(ImageMediaType::Jpeg));
    assert_eq!(of("notes.txt"), None);
    assert_eq!(of("no-extension"), None);
}

#[test]
fn a_prompt_without_a_delivery_steers() {
    let params: PromptParams = serde_json::from_value(json!({
        "agent": 3,
        "prompt": {"text": "Why does the form reject valid emails?", "images": []},
    }))
    .unwrap();

    assert_eq!(params.delivery, PromptDelivery::Steer);
}

#[test]
fn waiting_prompts_and_their_requests_on_the_wire() {
    let [sent, _] = details().waiting_prompts.try_into().unwrap();
    let send = ClientMessage::Request {
        id: RequestId::new(12),
        request: Request::AgentSendQueued(QueuedPromptParams {
            agent: agent(),
            prompt: PromptId::new(5),
        }),
    };

    assert_wire(
        &sent,
        json!({
            "id": 4,
            "text": "Also check the signup form",
            "image_count": 1,
            "state": "sent",
            "sent_at": 1_700_000_100_000_u64,
        }),
    );
    assert_wire(
        &send,
        json!({
            "type": "request",
            "id": 12,
            "request": {"method": "agent.send_queued", "params": {"agent": 3, "prompt": 5}},
        }),
    );
}

#[test]
fn an_image_answer_on_the_wire() {
    let answer = ResponseBody::Image {
        path: PathBuf::from("/srv/projects/shop/docs/cart.png"),
        image: image(),
    };

    assert_wire(
        &answer,
        json!({
            "type": "image",
            "path": "/srv/projects/shop/docs/cart.png",
            "image": {"media_type": "image/png", "data": "iVBORw0KGgo="},
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
fn a_mark_request_on_the_wire() {
    let message = ClientMessage::Request {
        id: RequestId::new(10),
        request: Request::AgentMark(MarkParams {
            agent: agent(),
            mark: Mark::ToHandle,
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "request",
            "id": 10,
            "request": {
                "method": "agent.mark",
                "params": {"agent": 3, "mark": "to_handle"},
            },
        }),
    );
}

#[test]
fn a_change_of_working_folder_on_the_wire() {
    let message = ClientMessage::Request {
        id: RequestId::new(11),
        request: Request::AgentChangeFolder(FolderParams {
            agent: agent(),
            folder: PathBuf::from("/work/app_moved"),
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "request",
            "id": 11,
            "request": {
                "method": "agent.change_folder",
                "params": {"agent": 3, "folder": "/work/app_moved"},
            },
        }),
    );
}

#[test]
fn a_folder_listing_on_the_wire() {
    let request = ClientMessage::Request {
        id: RequestId::new(11),
        request: Request::AgentListDir(ListDirParams {
            agent: agent(),
            path: PathBuf::from("src"),
        }),
    };
    let search = ClientMessage::Request {
        id: RequestId::new(12),
        request: Request::AgentFindFiles(FindFilesParams {
            agent: agent(),
            name: "login".to_owned(),
        }),
    };
    let answer = ServerMessage::Response {
        id: RequestId::new(11),
        result: ResponseResult::Ok(ResponseBody::Directory(dir_listing())),
    };

    assert_wire(
        &request,
        json!({
            "type": "request",
            "id": 11,
            "request": {"method": "agent.list_dir", "params": {"agent": 3, "path": "src"}},
        }),
    );
    assert_wire(
        &search,
        json!({
            "type": "request",
            "id": 12,
            "request": {"method": "agent.find_files", "params": {"agent": 3, "name": "login"}},
        }),
    );
    assert_wire(
        &answer,
        json!({
            "type": "response",
            "id": 11,
            "result": {"ok": {
                "type": "directory",
                "path": "/srv/projects/shop/src",
                "entries": [
                    {"name": "forms", "kind": "folder", "size": null, "link": "direct", "ignore": "ignored"},
                    {"name": "login.rs", "kind": "file", "size": 2048, "link": "direct", "ignore": "kept"},
                    {"name": "shared", "kind": "other", "size": null, "link": "symbolic_link", "ignore": "kept"},
                ],
                "completeness": "complete",
            }},
        }),
    );
}

#[test]
fn a_folders_listing_on_the_wire() {
    let request = ClientMessage::Request {
        id: RequestId::new(13),
        request: Request::FoldersList(FoldersListParams {
            path: PathBuf::from("/srv/projects"),
        }),
    };
    let answer = ServerMessage::Response {
        id: RequestId::new(13),
        result: ResponseResult::Ok(ResponseBody::Directory(DirListing {
            path: PathBuf::from("/srv/projects"),
            entries: vec![
                DirEntry {
                    name: ".archive".to_owned(),
                    kind: EntryKind::Folder,
                    size: None,
                    link: EntryLink::Direct,
                    ignore: GitIgnore::Kept,
                },
                DirEntry {
                    name: "shop".to_owned(),
                    kind: EntryKind::Folder,
                    size: None,
                    link: EntryLink::SymbolicLink,
                    ignore: GitIgnore::Kept,
                },
            ],
            completeness: Completeness::Truncated,
        })),
    };

    assert_wire(
        &request,
        json!({
            "type": "request",
            "id": 13,
            "request": {"method": "folders.list", "params": {"path": "/srv/projects"}},
        }),
    );
    assert_wire(
        &answer,
        json!({
            "type": "response",
            "id": 13,
            "result": {"ok": {
                "type": "directory",
                "path": "/srv/projects",
                "entries": [
                    {"name": ".archive", "kind": "folder", "size": null, "link": "direct", "ignore": "kept"},
                    {"name": "shop", "kind": "folder", "size": null, "link": "symbolic_link", "ignore": "kept"},
                ],
                "completeness": "truncated",
            }},
        }),
    );
}

#[test]
fn a_close_request_on_the_wire() {
    let message = ClientMessage::Request {
        id: RequestId::new(11),
        request: Request::AgentClose(AgentParams { agent: agent() }),
    };

    assert_wire(
        &message,
        json!({
            "type": "request",
            "id": 11,
            "request": {"method": "agent.close", "params": {"agent": 3}},
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
        daemon_version: ReleaseVersion::new("0.1.0".to_owned()),
        catchup: Catchup::Snapshot(Snapshot {
            agents: Vec::new(),
            plan_usage: plan_usage(),
            settings: settings(),
            last_seq: None,
            accounts: vec![work_account()],
            other_plan_usage: vec![AccountPlanUsage {
                account: work_account().id,
                usage: PlanUsage::default(),
            }],
        }),
    };

    assert_wire(
        &message,
        json!({
            "type": "welcome",
            "protocol_version": 1,
            "daemon_version": "0.1.0",
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
                "accounts": [{
                    "id": 2,
                    "name": "Work",
                    "folder": "/home/sam/.claude-work",
                    "credentials": "environment",
                }],
                "other_plan_usage": [{"account": 2, "usage": {"windows": {}}}],
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
fn machine_load_requests_on_the_wire() {
    let load = ClientMessage::Request {
        id: RequestId::new(11),
        request: Request::MachineLoad,
    };
    let stop = ClientMessage::Request {
        id: RequestId::new(12),
        request: Request::AgentStopProcess(StopProcessParams {
            agent: agent(),
            pid: ProcessId::new(4242),
        }),
    };

    assert_wire(
        &load,
        json!({"type": "request", "id": 11, "request": {"method": "machine.load"}}),
    );
    assert_wire(
        &stop,
        json!({
            "type": "request",
            "id": 12,
            "request": {"method": "agent.stop_process", "params": {"agent": 3, "pid": 4242}},
        }),
    );
}

#[test]
fn a_machine_load_answer_on_the_wire() {
    let answer = ResponseBody::MachineLoad(machine_load());

    assert_wire(
        &answer,
        json!({
            "type": "machine_load",
            "taken_at": 1_700_000_003_000_u64,
            "server": {
                "cores": 64,
                "cpu": 46.5,
                "memory_used_bytes": 312_000_000_000_u64,
                "memory_total_bytes": 540_000_000_000_u64,
                "load_average": {"one_minute": 43.5, "five_minutes": 38.25, "fifteen_minutes": 30.0},
            },
            "agents": [
                {
                    "agent": 3,
                    "cpu": 18.5,
                    "memory_bytes": 22_000_000_000_u64,
                    "processes": 14,
                    "heaviest": [{
                        "pid": 4242,
                        "command": "rustc --crate-name shop_api --edition=2024",
                        "cpu": 7.75,
                        "memory_bytes": 6_000_000_000_u64,
                    }],
                },
                {"agent": 4, "cpu": 0.0, "memory_bytes": 400_000_000, "processes": 1, "heaviest": []},
            ],
            "others": {"cpu": 28.0, "memory_bytes": 289_600_000_000_u64},
            "history": {
                "server": [
                    {"taken_at": 1_700_000_000_000_u64, "cpu": 44.0},
                    {"taken_at": 1_700_000_003_000_u64, "cpu": 46.5},
                ],
                "agents": [
                    {"agent": 3, "cpu": [16.0, 18.5]},
                    {"agent": 4, "cpu": [null, 0.0]},
                ],
            },
        }),
    );
}

/// A Windows daemon has no load average to send.
#[test]
fn a_server_load_without_a_load_average_on_the_wire() {
    let server = ServerLoad {
        cores: 8,
        cpu: Cores::new(1.5),
        memory_used_bytes: 4_000,
        memory_total_bytes: 16_000,
        load_average: None,
    };
    let fields = json!({
        "cores": 8,
        "cpu": 1.5,
        "memory_used_bytes": 4_000,
        "memory_total_bytes": 16_000,
    });

    let missing: ServerLoad = serde_json::from_value(fields).unwrap();

    assert_eq!(missing, server);
    assert_wire(
        &server,
        json!({
            "cores": 8,
            "cpu": 1.5,
            "memory_used_bytes": 4_000,
            "memory_total_bytes": 16_000,
            "load_average": null,
        }),
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
                "effort_source": "claude_code_default",
                "mode": "acceptEdits",
                "task_progress": {"done": 3, "total": 5},
                "context_percent": 42,
                "cache_expires_at": 1_700_000_300_000_u64,
                "turns": 7,
                "cost": 1.25,
                "started_at": 1_700_000_000_000_u64,
                "last_activity_at": 1_700_000_250_000_u64,
                "turn_started_at": 1_700_000_200_000_u64,
                "session": "0b7e1c2a-3d4f-4a5b-8c6d-7e8f9a0b1c2d",
                "position": 2048,
                "account": 2,
                "sign_in_wait": "waiting",
                "background_running": 2,
                "mark": "to_handle",
            },
        }),
    );
}

/// A summary from a daemon that predates the count has nothing running in the background, and
/// one that predates the mark is not to handle.
#[test]
fn a_summary_without_its_background_count_reads_none_running() {
    let mut older = serde_json::to_value(summary()).unwrap();
    older.as_object_mut().unwrap().remove("background_running");
    older.as_object_mut().unwrap().remove("mark");

    let read: AgentSummary = serde_json::from_value(older).unwrap();

    assert_eq!(read.background_running, 0);
    assert_eq!(read.mark, Mark::Unmarked);
}

/// A task names the call that started it, and a task without one leaves the field out.
#[test]
fn a_background_task_on_the_wire() {
    let [running, done] = details().background_tasks.try_into().unwrap();

    assert_wire(
        &running,
        json!({
            "id": "task-1",
            "kind": "command",
            "description": "cargo watch",
            "status": "running",
            "started_at": 1_700_000_100_000_u64,
            "ended_at": null,
            "tool_calls": 0,
            "call": 3,
        }),
    );
    assert!(serde_json::to_value(&done).unwrap().get("call").is_none());
}

#[test]
fn a_subagent_on_the_wire() {
    let [running, _] = subagents().try_into().unwrap();

    assert_wire(
        &running,
        json!({
            "item": 40,
            "parent": null,
            "description": "Search the callers",
            "agent_type": "Explore",
            "run": "background",
            "status": "running",
            "started_at": 1_700_000_100_000_u64,
            "ended_at": null,
            "tokens": 18_400,
            "tool_calls": 7,
            "last_tool": {"item": 44, "tool": "Agent", "summary": "Check the lockfile"},
            "error": null,
            "turn": 3,
            "task": "task-2",
        }),
    );
}

#[test]
fn details_from_a_daemon_without_subagents_still_load() {
    let mut sent = serde_json::to_value(details()).unwrap();
    sent.as_object_mut().unwrap().remove("subagents");

    let read: AgentDetails = serde_json::from_value(sent).unwrap();

    assert_eq!(read.subagents, []);
    assert_eq!(read.background_tasks, details().background_tasks);
}

#[test]
fn a_summary_logged_before_positions_reads_position_zero() {
    let mut logged = serde_json::to_value(summary()).unwrap();
    logged.as_object_mut().unwrap().remove("position");

    let read: AgentSummary = serde_json::from_value(logged).unwrap();

    assert_eq!(read.position, Position::new(0));
}

#[test]
fn a_summary_logged_before_accounts_reads_the_default_account() {
    let mut logged = serde_json::to_value(summary()).unwrap();
    logged.as_object_mut().unwrap().remove("account");

    let read: AgentSummary = serde_json::from_value(logged).unwrap();

    assert_eq!(read.account, AccountId::DEFAULT);
}

/// An event log of an older daemon holds `waits_for_sign_in`, a `bool`, or nothing.
#[test]
fn a_summary_logged_with_its_sign_in_wait_as_a_bool_keeps_it() {
    let read = |waits: Option<bool>| {
        let mut logged = serde_json::to_value(summary()).unwrap();
        let fields = logged.as_object_mut().unwrap();
        fields.remove("sign_in_wait");
        if let Some(waits) = waits {
            fields.insert("waits_for_sign_in".to_owned(), Value::Bool(waits));
        }
        serde_json::from_value::<AgentSummary>(logged)
            .unwrap()
            .sign_in_wait
    };

    assert_eq!(read(Some(true)), SignInWait::Waiting);
    assert_eq!(read(Some(false)), SignInWait::NotWaiting);
    assert_eq!(read(None), SignInWait::NotWaiting);
}

/// An event log of an older daemon holds `effort_is_default`, a `bool`, or nothing.
#[test]
fn a_summary_saved_with_its_effort_source_as_a_bool_keeps_it() {
    let loaded = |is_default: Option<bool>| {
        let mut saved = serde_json::to_value(summary()).unwrap();
        let fields = saved.as_object_mut().unwrap();
        fields.remove("effort_source");
        if let Some(is_default) = is_default {
            fields.insert("effort_is_default".to_owned(), Value::Bool(is_default));
        }
        let loaded: AgentSummary = serde_json::from_value(saved).unwrap();
        (loaded.effort, loaded.effort_source)
    };

    let high = Some(Effort::High);
    assert_eq!(loaded(Some(true)), (high, EffortSource::ClaudeCodeDefault));
    assert_eq!(loaded(Some(false)), (high, EffortSource::Chosen));
    assert_eq!(loaded(None), (high, EffortSource::Chosen));
}

#[test]
fn a_summary_from_a_daemon_without_activity_times_reads_none() {
    let mut logged = serde_json::to_value(summary()).unwrap();
    let fields = logged.as_object_mut().unwrap();
    fields.remove("last_activity_at");
    fields.remove("turn_started_at");

    let read: AgentSummary = serde_json::from_value(logged).unwrap();

    assert_eq!(read.last_activity_at, None);
    assert_eq!(read.turn_started_at, None);
}

#[test]
fn an_idle_agent_sends_no_turn_start() {
    let sent = serde_json::to_value(idle_summary()).unwrap();

    assert_eq!(sent["last_activity_at"], json!(1_700_000_000_000_u64));
    assert_eq!(sent["turn_started_at"], Value::Null);
}

#[test]
fn a_file_answer_with_or_without_the_path_it_read_and_its_edits() {
    let with_path = ResponseBody::File {
        path: Some(PathBuf::from("/srv/projects/shop/src/main.rs")),
        text: "fn main() {}\n".to_owned(),
        edits: Some(vec![edit()]),
    };

    assert_wire(
        &with_path,
        json!({
            "type": "file",
            "path": "/srv/projects/shop/src/main.rs",
            "text": "fn main() {}\n",
            "edits": [{
                "path": "src/login.rs",
                "old_text": "let valid = false;",
                "new_text": "let valid = check(email);",
            }],
        }),
    );
    // A daemon that predates the path and the edits.
    let without_path: ResponseBody =
        serde_json::from_value(json!({"type": "file", "text": "fn main() {}\n"})).unwrap();
    assert_eq!(
        without_path,
        ResponseBody::File {
            path: None,
            text: "fn main() {}\n".to_owned(),
            edits: None,
        }
    );
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
        outcome: TurnOutcome::NotSignedIn,
        error: Some("The account is not signed in".to_owned()),
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
            "outcome": "not_signed_in",
            "error": "The account is not signed in",
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
fn a_stopped_turn_end_on_the_wire() {
    let stopped = ItemBody::TurnEnd(TurnEnd {
        duration: Duration::from_millis(6_100),
        cost: CostUsd::new(0.01),
        usage: TokenUsage::default(),
        outcome: TurnOutcome::Stopped,
        error: None,
    });

    assert_wire(
        &stopped,
        json!({
            "type": "turn_end",
            "duration_ms": 6_100,
            "cost": 0.01,
            "usage": {"input": 0, "output": 0, "cache_creation": 0, "cache_read": 0},
            "outcome": "stopped",
            "error": null,
        }),
    );
}

/// An event log of an older daemon holds `stopped` and `not_signed_in`, two `bool`s that may be
/// missing, in place of `outcome`.
#[test]
fn turn_ends_saved_without_their_outcome_still_load() {
    let saved = |fields: &str| {
        let line = format!(
            r#"{{"type":"turn_end","duration_ms":6100,"cost":0.01,"usage":{{"input":0,"output":0,"cache_creation":0,"cache_read":0}},{fields}}}"#
        );
        let ItemBody::TurnEnd(end) = serde_json::from_str::<ItemBody>(&line).unwrap() else {
            panic!("not a turn end");
        };
        (end.outcome, end.error)
    };
    let error = |text: &str| Some(text.to_owned());

    assert_eq!(saved(r#""error":null"#), (TurnOutcome::Completed, None));
    assert_eq!(
        saved(r#""error":"Interrupted""#),
        (TurnOutcome::Failed, error("Interrupted"))
    );
    assert_eq!(
        saved(r#""error":null,"stopped":true,"not_signed_in":false"#),
        (TurnOutcome::Stopped, None)
    );
    assert_eq!(
        saved(r#""error":"The account is not signed in","stopped":false,"not_signed_in":true"#),
        (
            TurnOutcome::NotSignedIn,
            error("The account is not signed in")
        )
    );
}

/// An event log of an older daemon holds `queued` and `multi_select`, and `settings.json` holds
/// `use_environment_api_key`: `bool`s in place of enums. A user message's `queued` or
/// `delivery` is ignored: the waiting prompts are in the details.
#[test]
fn fields_saved_as_bools_still_load() {
    let message = |field: &str| {
        let message = format!(r#"{{"type":"user_message","text":"hello","images":[],{field}}}"#);
        serde_json::from_str::<ItemBody>(&message).unwrap()
    };
    let hello = ItemBody::UserMessage(UserMessage {
        text: "hello".to_owned(),
        images: Vec::new(),
        sent_at: None,
    });
    let selection = |multi_select: bool| {
        let question = format!(
            r#"{{"header":"Database","text":"Which one?","options":[],"multi_select":{multi_select}}}"#
        );
        serde_json::from_str::<AskedQuestion>(&question)
            .unwrap()
            .selection
    };
    let credentials = |uses: bool| {
        let account = format!(
            r#"{{"id":2,"name":"Work","folder":"/home/sam/.claude-work","use_environment_api_key":{uses}}}"#
        );
        serde_json::from_str::<Account>(&account).unwrap()
    };

    assert_eq!(message(r#""queued":true"#), hello);
    assert_eq!(message(r#""delivery":"delivered""#), hello);
    assert_eq!(selection(true), Selection::Multiple);
    assert_eq!(selection(false), Selection::Single);
    assert_eq!(credentials(true), work_account());
    assert_eq!(credentials(false).credentials, CredentialSource::SignIn);
}

/// Older daemons write `environment_api_key` for an account that takes its credentials from the
/// daemon's environment, and for whether that environment holds them.
#[test]
fn the_older_words_for_the_environments_credentials_still_read() {
    let account = r#"{"id":2,"name":"Work","folder":"/home/sam/.claude-work","credentials":"environment_api_key"}"#;
    let answer = json!({
        "type": "accounts",
        "accounts": [],
        "environment_api_key": "set",
    });

    let account: Account = serde_json::from_str(account).unwrap();
    let answer: ResponseBody = serde_json::from_value(answer).unwrap();

    assert_eq!(account, work_account());
    assert_eq!(
        answer,
        ResponseBody::Accounts {
            accounts: Vec::new(),
            environment_credentials: EnvironmentCredentials::Set,
        }
    );
}

/// The transcript's items and pages, and the MCP toggle, with the enums that replaced `bool`s.
#[test]
fn transcript_pages_and_mcp_toggles_on_the_wire() {
    let page = ResponseBody::History(HistoryPage {
        items: vec![TranscriptItem {
            id: ItemId::new(7),
            parent: None,
            body: ItemBody::UserMessage(UserMessage {
                text: "Run the tests".to_owned(),
                images: Vec::new(),
                sent_at: Some(at(1_700_000_000_000)),
            }),
        }],
        older_items: OlderItems::None,
    });
    let toggle = Request::AgentMcpToggle(McpToggleParams {
        agent: agent(),
        server: "issues".to_owned(),
        action: McpToggleAction::Enable,
    });

    assert_wire(
        &page,
        json!({
            "type": "history",
            "items": [{
                "id": 7,
                "parent": null,
                "body": {
                    "type": "user_message",
                    "text": "Run the tests",
                    "images": [],
                    "sent_at": 1_700_000_000_000_u64,
                },
            }],
            "older_items": "none",
        }),
    );
    assert_wire(
        &asked_question(),
        json!({
            "header": "Database",
            "text": "Which database should the tests use?",
            "options": [
                {"label": "SQLite", "description": "In memory, no setup"},
                {"label": "Postgres", "description": "Like production"},
            ],
            "selection": "single",
        }),
    );
    assert_wire(
        &toggle,
        json!({"method": "agent.mcp_toggle", "params": {
            "agent": 3,
            "server": "issues",
            "action": "enable",
        }}),
    );
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
fn the_unknown_method_message_is_pinned() {
    // Older daemons send this text for a method they do not know, and clients match it.
    assert_eq!(
        Method::unknown_message("agent.fly"),
        "unknown method `agent.fly`"
    );
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

#[test]
fn account_requests_on_the_wire() {
    let list = Request::AccountsList;
    let add = Request::AccountsAdd(AccountAddParams {
        name: "Work".to_owned(),
        folder: Some(PathBuf::from("/home/sam/.claude-work")),
        credentials: CredentialSource::Environment,
    });
    let change = Request::AccountsChange(AccountChangeParams {
        account: AccountId::new(2),
        name: None,
        credentials: Some(CredentialSource::SignIn),
    });
    let remove = Request::AccountsRemove(AccountParams {
        account: AccountId::new(2),
    });

    assert_wire(&list, json!({"method": "accounts.list"}));
    assert_wire(
        &add,
        json!({"method": "accounts.add", "params": {
            "name": "Work",
            "folder": "/home/sam/.claude-work",
            "credentials": "environment",
        }}),
    );
    assert_wire(
        &change,
        json!({"method": "accounts.change", "params": {
            "account": 2,
            "name": null,
            "credentials": "sign_in",
        }}),
    );
    assert_wire(
        &remove,
        json!({"method": "accounts.remove", "params": {"account": 2}}),
    );
}

#[test]
fn an_account_added_with_only_a_name_takes_the_proposed_folder_and_no_api_key() {
    let added: Request =
        serde_json::from_value(json!({"method": "accounts.add", "params": {"name": "Work"}}))
            .unwrap();

    assert_eq!(
        added,
        Request::AccountsAdd(AccountAddParams {
            name: "Work".to_owned(),
            folder: None,
            credentials: CredentialSource::SignIn,
        })
    );
}

#[test]
fn the_accounts_answer_on_the_wire() {
    let body = ResponseBody::Accounts {
        accounts: vec![work_account_state()],
        environment_credentials: EnvironmentCredentials::Unset,
    };

    assert_wire(
        &body,
        json!({
            "type": "accounts",
            "accounts": [{
                "account": {
                    "id": 2,
                    "name": "Work",
                    "folder": "/home/sam/.claude-work",
                    "credentials": "environment",
                },
                "sign_in": "not_signed_in",
                "sign_in_command": "CLAUDE_CONFIG_DIR='/home/sam/.claude-work' claude",
                "route": "bedrock",
            }],
            "environment_credentials": "unset",
        }),
    );
}

/// An older daemon sends no route: its accounts read as on Anthropic's service.
#[test]
fn an_account_without_a_route_reads_as_anthropic() {
    let state: AccountState = serde_json::from_value(json!({
        "account": {"id": 0, "name": "Personal", "folder": "/home/sam/.claude", "credentials": "sign_in"},
        "sign_in": "signed_in",
        "sign_in_command": "claude",
    }))
    .unwrap();

    assert_eq!(state.route, ClaudeRoute::Anthropic);
}

#[test]
fn account_events_on_the_wire() {
    let usage = Event::AccountPlanUsageChanged(AccountPlanUsage {
        account: AccountId::new(2),
        usage: PlanUsage::default(),
    });
    let accounts = Event::AccountsChanged {
        accounts: vec![default_account()],
    };

    assert_wire(
        &usage,
        json!({"type": "account_plan_usage_changed", "account": 2, "usage": {"windows": {}}}),
    );
    assert_wire(
        &accounts,
        json!({"type": "accounts_changed", "accounts": [{
            "id": 0,
            "name": "Personal",
            "folder": "/home/sam/.claude",
            "credentials": "sign_in",
        }]}),
    );
}

/// An app that predates accounts reads the default account's usage from `plan_usage_changed`,
/// whose shape stays the same.
#[test]
fn the_default_accounts_plan_usage_keeps_its_event() {
    let event = Event::PlanUsageChanged(PlanUsage::default());

    assert_wire(&event, json!({"type": "plan_usage_changed", "windows": {}}));
}

/// What a client or a daemon that predates accounts sends still reads, as the default account.
#[test]
fn messages_without_accounts_read_as_the_default_account() {
    let start: Request = serde_json::from_value(json!({
        "method": "agent.start",
        "params": {"folder": "/srv/projects/shop", "title": null, "model": null,
                   "effort": null, "mode": null, "prompt": null},
    }))
    .unwrap();
    let resume: Request = serde_json::from_value(json!({
        "method": "sessions.resume",
        "params": {"session": "0b7e1c2a-3d4f-4a5b-8c6d-7e8f9a0b1c2d"},
    }))
    .unwrap();
    let session: PastSession = serde_json::from_value(json!({
        "id": "0b7e1c2a-3d4f-4a5b-8c6d-7e8f9a0b1c2d",
        "title": "Fix the login form",
        "folder": "/srv/projects/shop",
        "last_activity": 1_700_000_500_000_u64,
        "turns": 7,
    }))
    .unwrap();
    let snapshot: Snapshot = serde_json::from_value(json!({
        "agents": [],
        "plan_usage": {"windows": {}},
        "settings": {},
        "last_seq": null,
    }))
    .unwrap();

    let Request::AgentStart(start) = start else {
        panic!("not a start: {start:?}");
    };
    assert_eq!(start.account, AccountId::DEFAULT);
    let Request::SessionsResume(resume) = resume else {
        panic!("not a resume: {resume:?}");
    };
    assert_eq!(resume.account, AccountId::DEFAULT);
    assert_eq!(session.account, AccountId::DEFAULT);
    assert!(snapshot.accounts.is_empty());
    assert!(snapshot.other_plan_usage.is_empty());
}

/// Adds `shift` to every agent id in a message's JSON: `agent` anywhere, `agent.move`'s `before`,
/// and the `id` of an agent's summary, the object that also has a `folder` and a `status`.
/// `agent.history`'s `before` is an item: its params also have a `limit`.
fn shift_agent_ids(value: &mut Value, shift: u64) {
    match value {
        Value::Object(fields) => {
            let summary = fields.contains_key("folder") && fields.contains_key("status");
            let moves = fields.contains_key("agent") && !fields.contains_key("limit");
            for (key, field) in fields.iter_mut() {
                let is_agent =
                    key == "agent" || (moves && key == "before") || (summary && key == "id");
                match field {
                    Value::Number(number) if is_agent => {
                        *field = json!(number.as_u64().unwrap() + shift);
                    }
                    _ => shift_agent_ids(field, shift),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                shift_agent_ids(item, shift);
            }
        }
        _ => {}
    }
}

/// Checks that `map_agents` changes exactly the agent ids the JSON holds.
fn assert_maps_every_agent<T: Serialize + Clone>(message: &T, map: impl Fn(&mut T)) {
    let mut expected = serde_json::to_value(message).unwrap();
    shift_agent_ids(&mut expected, 1000);
    let mut mapped = message.clone();
    map(&mut mapped);
    assert_eq!(serde_json::to_value(&mapped).unwrap(), expected);
}

#[test]
fn every_agent_id_of_every_message_can_be_mapped() {
    let shift = |agent: AgentId| AgentId::new(agent.get() + 1000);
    for request in requests() {
        assert_maps_every_agent(&request, |request| request.map_agents(shift));
    }
    for body in response_bodies() {
        assert_maps_every_agent(&body, |body| body.map_agents(shift));
    }
    for event in events() {
        assert_maps_every_agent(&event, |event| event.map_agents(shift));
    }
    assert_maps_every_agent(&snapshot(), |snapshot| snapshot.map_agents(shift));
}

#[test]
fn a_request_names_the_agent_it_is_about() {
    let mut requests = requests();
    let agents: Vec<Option<Method>> = requests
        .iter_mut()
        .map(|request| {
            let method = request.method();
            request.agent_mut().map(|_| method)
        })
        .collect();

    assert!(agents.contains(&Some(Method::AgentMove)));
    assert!(agents.contains(&None), "sessions.list is about no agent");
}

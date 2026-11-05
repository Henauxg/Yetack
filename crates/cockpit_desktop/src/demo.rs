//! `--demo`: the window filled with made-up agents and no daemon, to look at the interface. The
//! requests that read data are answered from the sample; the others do nothing.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cockpit_client::RequestError;
use cockpit_protocol::{
    AgentDetails, AgentId, AgentStatus, AgentSummary, AnswerState, Approval, AskedQuestion,
    BackgroundTask, BackgroundTaskId, BackgroundTaskKind, BackgroundTaskStatus, ChangedFile,
    ClaudeSessionId, CommandKind, ContextCategory, ContextUsage, CostUsd, Effort, ErrorBody,
    ErrorCode, FileEdit, HistoryPage, ItemBody, ItemId, McpScope, McpServer, McpServerStatus,
    McpTool, McpTransport, PastSession, PendingId, PermissionMode, Plan, PlanUsage, Question,
    QuestionOption, RateLimitStatus, Request, ResponseBody, SlashCommand, TaskListItem,
    TaskProgress, TaskStatus, Timestamp, TokenUsage, ToolCall, ToolCallStatus, TranscriptItem,
    TurnEnd, UsageWindow, UserMessage, WindowUsage,
};
use serde_json::Value;

use crate::config::DaemonLabel;
use crate::state::{AgentEntry, AppState, ConnectionState, Loadable};

/// The command-line flag that starts the demo.
pub const DEMO_FLAG: &str = "--demo";

const PARSER_FOLDER: &str = "/home/dev/projects/calc-parser";
const SERVICE_FOLDER: &str = "/home/dev/projects/calc-service";

const ERROR_FILE: &str = "src/error.rs";
const TEST_FILE: &str = "tests/columns.rs";

const ERROR_AFTER: &str = "/// A parse error, with where it happened.
pub struct ParseError {
    pub line: u32,
    /// Counted from 1, as editors show it.
    pub column: u32,
    pub message: String,
}
";

const TEST_TEXT: &str = "use calc_parser::parse;

#[test]
fn an_unexpected_token_reports_its_column() {
    let error = parse(\"1 + * 2\").unwrap_err();
    assert_eq!(error.column, 5);
    assert_eq!(error.message, \"unexpected `*`\");
}
";

const FAILED_TEST_OUTPUT: &str = "running 4 tests
test lexer::tests::counts_lines ... ok
test parser::tests::parses_a_sum ... ok
test parser::tests::reports_the_line ... ok
test an_unexpected_token_reports_its_column ... FAILED

failures:

---- an_unexpected_token_reports_its_column stdout ----
assertion `left == right` failed
  left: 4
 right: 5

test result: FAILED. 3 passed; 1 failed; 0 ignored";

const FINDINGS: &str = "### What the subagent found

The parser builds every error at `src/error.rs:2`. The lexer already counts the column of each token.

| Place | What it does |
| --- | --- |
| `src/error.rs:2` | Declares the error |
| `tests/columns.rs:4` | New: checks the column |

- The column is a `u32` on `Token`.
- No new dependency is needed.";

const BENCHMARK_ANSWER: &str = "The benchmark runs in CI now. To compare two runs by hand:

```bash
# Run both versions and keep the medians
cargo bench --bench eval -- --save-baseline before
git switch cache-layer
cargo bench --bench eval -- --baseline before
```

The slowest case is still `deep_nesting`, at **2.4 ms**.";

const PLAN_TEXT: &str = "## Add a result cache

1. Add a `Cache` type in `src/cache.rs`, keyed by the normalized expression.
2. Look the expression up before evaluating it in `src/server.rs`.
3. Drop entries older than ten minutes.
4. Add a test that evaluates the same expression twice.";

pub fn is_requested(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == DEMO_FLAG)
}

/// The sample, as a connected daemon would leave it.
pub fn state(daemon: DaemonLabel) -> AppState {
    let now = SystemTime::now();
    let mut state = AppState::new(daemon);
    state.connection = ConnectionState::Connected;
    let agents = [
        parser_agent(now),
        question_agent(now),
        approval_agent(now),
        plan_agent(now),
        benchmark_agent(now),
        exited_agent(now),
    ];
    for sample in agents {
        let mut entry = AgentEntry::new(sample.summary);
        entry.transcript.merge_page(HistoryPage {
            items: sample.items,
            has_older: false,
        });
        entry.details = Loadable::Loaded(sample.details);
        state.agents.insert(entry.id(), entry);
    }
    state.plan_usage = plan_usage(now);
    state.ui.selected = Some(AgentId::new(1));
    state.ui.unread.insert(AgentId::new(5));
    state
}

/// What a daemon holding the sample would answer.
pub fn answer(state: &AppState, request: &Request) -> Result<ResponseBody, RequestError> {
    let agent_details = |agent: AgentId| -> Result<AgentDetails, RequestError> {
        state
            .agents
            .get(&agent)
            .and_then(|entry| entry.details.loaded().cloned())
            .ok_or_else(|| refused(ErrorCode::NotFound, "The demo has no such agent."))
    };
    match request {
        Request::AgentDetails(params) => agent_details(params.agent).map(ResponseBody::Details),
        Request::AgentMcpStatus(params) => {
            agent_details(params.agent).map(|details| ResponseBody::McpServers {
                servers: details.mcp_servers,
            })
        }
        Request::AgentCommands(params) => {
            agent_details(params.agent).map(|details| ResponseBody::Commands {
                commands: details.commands,
            })
        }
        Request::AgentContextUsage(params) => agent_details(params.agent)?
            .context
            .map(ResponseBody::ContextUsage)
            .ok_or_else(|| refused(ErrorCode::NotFound, "No context usage in the demo.")),
        Request::AgentReadFile(params) => file_text(&params.path)
            .map(|text| ResponseBody::File {
                text: text.to_owned(),
            })
            .ok_or_else(|| refused(ErrorCode::NotFound, "The demo has no such file.")),
        Request::AgentHistory(_) => Ok(ResponseBody::History(HistoryPage {
            items: Vec::new(),
            has_older: false,
        })),
        Request::SessionsList => Ok(ResponseBody::Sessions {
            sessions: sessions(SystemTime::now()),
        }),
        Request::AgentStart(_) | Request::SessionsResume(_) => Err(refused(
            ErrorCode::InvalidRequest,
            "The demo does not start agents.",
        )),
        _ => Ok(ResponseBody::Done),
    }
}

fn refused(code: ErrorCode, message: &str) -> RequestError {
    RequestError::Failed(ErrorBody::new(code, message.to_owned()))
}

fn file_text(path: &Path) -> Option<&'static str> {
    let relative = path.strip_prefix(PARSER_FOLDER).unwrap_or(path);
    if relative == Path::new(ERROR_FILE) {
        Some(ERROR_AFTER)
    } else if relative == Path::new(TEST_FILE) {
        Some(TEST_TEXT)
    } else {
        None
    }
}

fn at(now: SystemTime, seconds_ago: u64) -> Timestamp {
    let moment = now
        .checked_sub(Duration::from_secs(seconds_ago))
        .unwrap_or(now);
    Timestamp::from_system_time(moment).unwrap_or(Timestamp::from_unix_millis(0))
}

fn in_future(now: SystemTime, seconds: u64) -> Timestamp {
    let moment = now.checked_add(Duration::from_secs(seconds)).unwrap_or(now);
    Timestamp::from_system_time(moment).unwrap_or(Timestamp::from_unix_millis(0))
}

struct SummaryParts<'text> {
    id: u32,
    title: &'text str,
    folder: &'text str,
    status: AgentStatus,
    activity: Option<&'text str>,
    pending: u16,
}

fn summary(parts: SummaryParts<'_>, now: SystemTime) -> AgentSummary {
    AgentSummary {
        id: AgentId::new(parts.id),
        title: parts.title.to_owned(),
        folder: PathBuf::from(parts.folder),
        status: parts.status,
        activity: parts.activity.map(str::to_owned),
        pending_count: parts.pending,
        model: Some("claude-opus-5-5".to_owned()),
        effort: Some(Effort::High),
        mode: PermissionMode::Default,
        task_progress: None,
        context_percent: Some(31),
        cache_expires_at: Some(in_future(now, 6 * 60)),
        turns: 9,
        cost: CostUsd::new(1.84),
        started_at: at(now, 3600),
        session: Some(ClaudeSessionId::new(format!("demo-session-{}", parts.id))),
    }
}

fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
    TranscriptItem {
        id: ItemId::new(id),
        parent: parent.map(ItemId::new),
        body,
    }
}

fn user(text: &str, queued: bool) -> ItemBody {
    ItemBody::UserMessage(UserMessage {
        text: text.to_owned(),
        images: Vec::new(),
        queued,
    })
}

fn text(markdown: &str) -> ItemBody {
    ItemBody::AssistantText {
        markdown: markdown.to_owned(),
    }
}

fn call(tool: &str, summary: &str, status: ToolCallStatus, output: Option<&str>) -> ToolCall {
    ToolCall {
        tool: tool.to_owned(),
        summary: summary.to_owned(),
        input: Value::Null,
        status,
        output: output.map(str::to_owned),
        exit_code: None,
        edit: None,
    }
}

fn edit_call(tool: &str, path: &str, old_text: &str, new_text: &str) -> ItemBody {
    ItemBody::ToolCall(ToolCall {
        edit: Some(FileEdit {
            path: Path::new(PARSER_FOLDER).join(path),
            old_text: old_text.to_owned(),
            new_text: new_text.to_owned(),
        }),
        ..call(tool, path, ToolCallStatus::Done, None)
    })
}

fn tool(name: &str, description: &str, read_only: bool, destructive: bool, calls: u32) -> McpTool {
    McpTool {
        name: name.to_owned(),
        description: Some(description.to_owned()),
        read_only,
        destructive,
        open_world: false,
        calls,
    }
}

fn mcp_servers() -> Vec<McpServer> {
    vec![
        McpServer {
            name: "search".to_owned(),
            status: McpServerStatus::Connected,
            error: None,
            version: Some("search-server 1.4.0".to_owned()),
            transport: Some(McpTransport::Stdio {
                command: "search-server --index ./docs".to_owned(),
            }),
            scope: Some(McpScope::User),
            tools: vec![
                tool(
                    "search_docs",
                    "Finds pages in the docs index",
                    true,
                    false,
                    2,
                ),
                tool("fetch_page", "Reads one page of the docs", true, false, 0),
                tool("reindex", "Rebuilds the docs index", false, true, 0),
            ],
        },
        McpServer {
            name: "tickets".to_owned(),
            status: McpServerStatus::Connected,
            error: None,
            version: Some("tickets 0.9.2".to_owned()),
            transport: Some(McpTransport::Http {
                url: "https://tickets.example.com/mcp".to_owned(),
            }),
            scope: Some(McpScope::Project),
            tools: vec![
                tool("list_tickets", "Lists the open tickets", true, false, 1),
                tool("close_ticket", "Closes a ticket", false, true, 0),
            ],
        },
        McpServer {
            name: "metrics".to_owned(),
            status: McpServerStatus::Failed,
            error: Some(
                "Connection closed during the handshake. The server process exited with status 1."
                    .to_owned(),
            ),
            version: None,
            transport: Some(McpTransport::Stdio {
                command: "metrics-server --port 0".to_owned(),
            }),
            scope: Some(McpScope::User),
            tools: Vec::new(),
        },
        McpServer {
            name: "calendar".to_owned(),
            status: McpServerStatus::NeedsAuth,
            error: None,
            version: None,
            transport: Some(McpTransport::Http {
                url: "https://calendar.example.com/mcp".to_owned(),
            }),
            scope: Some(McpScope::ClaudeAi),
            tools: Vec::new(),
        },
    ]
}

fn commands() -> Vec<SlashCommand> {
    let command = |name: &str, description: &str, kind: CommandKind| SlashCommand {
        name: name.to_owned(),
        description: description.to_owned(),
        kind,
    };
    vec![
        command(
            "review",
            "Review the changes on the branch",
            CommandKind::Skill,
        ),
        command("changelog", "Draft the changelog entry", CommandKind::Skill),
        command(
            "compact",
            "Summarize the conversation to free context",
            CommandKind::Command,
        ),
        command(
            "context",
            "Show what fills the context",
            CommandKind::Command,
        ),
        command("model", "Change the model", CommandKind::Command),
        command("effort", "Change the effort level", CommandKind::Command),
    ]
}

fn details() -> AgentDetails {
    AgentDetails {
        mcp_servers: mcp_servers(),
        commands: commands(),
        ..AgentDetails::default()
    }
}

struct Sample {
    summary: AgentSummary,
    items: Vec<TranscriptItem>,
    details: AgentDetails,
}

fn parser_agent(now: SystemTime) -> Sample {
    let mut summary = summary(
        SummaryParts {
            id: 1,
            title: "Parser error columns",
            folder: PARSER_FOLDER,
            status: AgentStatus::Working,
            activity: Some("Running cargo test parser"),
            pending: 0,
        },
        now,
    );
    summary.task_progress = Some(TaskProgress { done: 3, total: 5 });
    summary.context_percent = Some(56);
    summary.cache_expires_at = Some(in_future(now, 48 * 60));
    summary.mode = PermissionMode::Auto;
    summary.effort = Some(Effort::Xhigh);
    summary.turns = 19;
    summary.cost = CostUsd::new(3.4);
    let failed = ToolCall {
        exit_code: Some(101),
        ..call(
            "Bash",
            "cargo test parser",
            ToolCallStatus::Failed,
            Some(FAILED_TEST_OUTPUT),
        )
    };
    let items = vec![
        item(
            1,
            None,
            user(
                "Make the parser report the column of an unexpected token, and add a test for it.",
                false,
            ),
        ),
        item(
            2,
            None,
            text("I will have a subagent map where the errors are built first."),
        ),
        item(
            3,
            None,
            ItemBody::Subagent(call(
                "Task",
                "Find where the parser builds its errors",
                ToolCallStatus::Done,
                Some(
                    "Errors are built in one place. The lexer keeps the column, but the parser drops it.",
                ),
            )),
        ),
        item(
            4,
            Some(3),
            ItemBody::ToolCall(call("Read", "src/parser.rs", ToolCallStatus::Done, None)),
        ),
        item(
            5,
            Some(3),
            ItemBody::ToolCall(call(
                "Grep",
                "ParseError in src/",
                ToolCallStatus::Done,
                Some("3 matches"),
            )),
        ),
        item(
            6,
            Some(3),
            ItemBody::ToolCall(call("Read", ERROR_FILE, ToolCallStatus::Done, None)),
        ),
        item(7, None, text(FINDINGS)),
        item(
            8,
            None,
            edit_call(
                "Edit",
                ERROR_FILE,
                "    pub line: u32,\n",
                "    pub line: u32,\n    /// Counted from 1, as editors show it.\n    pub column: u32,\n",
            ),
        ),
        item(9, None, edit_call("Write", TEST_FILE, "", TEST_TEXT)),
        item(10, None, ItemBody::ToolCall(failed)),
        item(
            11,
            None,
            text(
                "The test expects the column to count from 1, and the lexer counts from 0. See `tests/columns.rs:6`. I am fixing the lexer.",
            ),
        ),
        item(
            12,
            None,
            ItemBody::ToolCall(call(
                "Bash",
                "cargo test parser",
                ToolCallStatus::Running,
                None,
            )),
        ),
        item(
            13,
            None,
            user("Also cover a token at the end of the input.", true),
        ),
    ];
    let task = |content: &str, status: TaskStatus| TaskListItem {
        content: content.to_owned(),
        status,
        active_form: String::new(),
    };
    let details = AgentDetails {
        tasks: vec![
            task("Find where errors are built", TaskStatus::Completed),
            task("Add the column to the error", TaskStatus::Completed),
            task("Write the column test", TaskStatus::Completed),
            task("Fix the failing test", TaskStatus::InProgress),
            task("Cover a token at the end of the input", TaskStatus::Pending),
        ],
        background_tasks: vec![
            BackgroundTask {
                id: BackgroundTaskId::new("demo-task-1".to_owned()),
                kind: BackgroundTaskKind::Agent,
                description: "Review the error type for missing cases".to_owned(),
                status: BackgroundTaskStatus::Running,
                started_at: at(now, 41),
                ended_at: None,
                tool_calls: 5,
            },
            BackgroundTask {
                id: BackgroundTaskId::new("demo-task-2".to_owned()),
                kind: BackgroundTaskKind::Command,
                description: "cargo test parser".to_owned(),
                status: BackgroundTaskStatus::Running,
                started_at: at(now, 12),
                ended_at: None,
                tool_calls: 0,
            },
        ],
        changed_files: vec![
            ChangedFile {
                path: Path::new(PARSER_FOLDER).join(ERROR_FILE),
                lines_added: 2,
                lines_removed: 0,
            },
            ChangedFile {
                path: Path::new(PARSER_FOLDER).join(TEST_FILE),
                lines_added: 8,
                lines_removed: 0,
            },
        ],
        context: Some(ContextUsage {
            tokens: 112_000,
            max_tokens: 200_000,
            percent: 56,
            categories: vec![
                ContextCategory {
                    name: "Messages".to_owned(),
                    tokens: 88_300,
                },
                ContextCategory {
                    name: "Tools".to_owned(),
                    tokens: 14_200,
                },
                ContextCategory {
                    name: "Memory files".to_owned(),
                    tokens: 6_100,
                },
                ContextCategory {
                    name: "System prompt".to_owned(),
                    tokens: 3_400,
                },
            ],
        }),
        ..details()
    };
    Sample {
        summary,
        items,
        details,
    }
}

fn question_agent(now: SystemTime) -> Sample {
    let summary = summary(
        SummaryParts {
            id: 2,
            title: "Release notes",
            folder: PARSER_FOLDER,
            status: AgentStatus::NeedsYou,
            activity: Some("Asks which changes to include"),
            pending: 1,
        },
        now,
    );
    let option = |label: &str, description: &str| QuestionOption {
        label: label.to_owned(),
        description: description.to_owned(),
    };
    let items = vec![
        item(
            1,
            None,
            user("Draft the release notes for the next version.", false),
        ),
        item(
            2,
            None,
            ItemBody::ToolCall(call(
                "Bash",
                "git log --oneline v0.3.0..",
                ToolCallStatus::Done,
                Some("12 commits"),
            )),
        ),
        item(
            3,
            None,
            ItemBody::Question(Question {
                pending: PendingId::new("demo-question".to_owned()),
                questions: vec![AskedQuestion {
                    header: "Scope".to_owned(),
                    text: "Which changes should the notes cover?".to_owned(),
                    options: vec![
                        option("User-facing only", "Leave out refactors and test changes"),
                        option("Everything", "List every merged change"),
                        option("Breaking changes first", "Start with what needs action"),
                    ],
                    multi_select: false,
                }],
                answer: AnswerState::Waiting,
            }),
        ),
    ];
    Sample {
        summary,
        items,
        details: details(),
    }
}

fn approval_agent(now: SystemTime) -> Sample {
    let summary = summary(
        SummaryParts {
            id: 3,
            title: "Docs preview",
            folder: "/home/dev/projects/docs-site",
            status: AgentStatus::NeedsYou,
            activity: Some("Waits for approval: git push"),
            pending: 1,
        },
        now,
    );
    let items = vec![
        item(
            1,
            None,
            user("Fix the broken links and push a preview.", false),
        ),
        item(
            2,
            None,
            ItemBody::ToolCall(call(
                "Bash",
                "npm run check-links",
                ToolCallStatus::Done,
                Some("0 broken links"),
            )),
        ),
        item(
            3,
            None,
            ItemBody::Approval(Approval {
                pending: PendingId::new("demo-approval".to_owned()),
                tool: "Bash".to_owned(),
                summary: "git push -u origin fix-links".to_owned(),
                input: Value::Null,
                edit: None,
                reason: Some("Push the branch so the preview builds.".to_owned()),
                answer: AnswerState::Waiting,
            }),
        ),
    ];
    Sample {
        summary,
        items,
        details: details(),
    }
}

fn plan_agent(now: SystemTime) -> Sample {
    let mut summary = summary(
        SummaryParts {
            id: 4,
            title: "Cache layer plan",
            folder: SERVICE_FOLDER,
            status: AgentStatus::NeedsYou,
            activity: Some("Waits for plan approval"),
            pending: 1,
        },
        now,
    );
    summary.mode = PermissionMode::Plan;
    let items = vec![
        item(
            1,
            None,
            user("Plan a cache for repeated expressions.", false),
        ),
        item(
            2,
            None,
            ItemBody::ToolCall(call("Read", "src/server.rs", ToolCallStatus::Done, None)),
        ),
        item(
            3,
            None,
            ItemBody::Plan(Plan {
                pending: PendingId::new("demo-plan".to_owned()),
                markdown: PLAN_TEXT.to_owned(),
                answer: AnswerState::Waiting,
            }),
        ),
    ];
    Sample {
        summary,
        items,
        details: details(),
    }
}

fn benchmark_agent(now: SystemTime) -> Sample {
    let mut summary = summary(
        SummaryParts {
            id: 5,
            title: "Benchmark in CI",
            folder: SERVICE_FOLDER,
            status: AgentStatus::Idle,
            activity: Some("Done: the benchmark runs in CI"),
            pending: 0,
        },
        now,
    );
    summary.task_progress = Some(TaskProgress { done: 4, total: 4 });
    summary.model = Some("claude-sonnet-5-5".to_owned());
    summary.mode = PermissionMode::AcceptEdits;
    let items = vec![
        item(
            1,
            None,
            user(
                "Run the benchmark in CI and tell me how to compare two runs.",
                false,
            ),
        ),
        item(
            2,
            None,
            ItemBody::ToolCall(call(
                "Bash",
                "cargo bench --bench eval",
                ToolCallStatus::Done,
                Some("eval/deep_nesting  time: [2.38 ms 2.41 ms 2.44 ms]"),
            )),
        ),
        item(3, None, text(BENCHMARK_ANSWER)),
        item(
            4,
            None,
            ItemBody::TurnEnd(TurnEnd {
                duration: Duration::from_secs(252),
                cost: CostUsd::new(0.62),
                usage: TokenUsage::default(),
                error: None,
            }),
        ),
    ];
    Sample {
        summary,
        items,
        details: details(),
    }
}

fn exited_agent(now: SystemTime) -> Sample {
    let mut summary = summary(
        SummaryParts {
            id: 6,
            title: "Schema migration",
            folder: "/home/dev/projects/legacy-db",
            status: AgentStatus::Exited,
            activity: None,
            pending: 0,
        },
        now,
    );
    summary.cache_expires_at = Some(at(now, 600));
    let items = vec![
        item(
            1,
            None,
            user("Write the migration that adds the archive table.", false),
        ),
        item(
            2,
            None,
            text("The migration is written and tested. The branch is ready for review."),
        ),
        item(
            3,
            None,
            ItemBody::Notice {
                text: "claude exited with status 0".to_owned(),
            },
        ),
    ];
    Sample {
        summary,
        items,
        details: details(),
    }
}

fn plan_usage(now: SystemTime) -> PlanUsage {
    let mut usage = PlanUsage::default();
    usage.windows.insert(
        UsageWindow::FiveHour,
        WindowUsage {
            utilization: Some(0.42),
            resets_at: Some(in_future(now, 3 * 3600 + 12 * 60)),
            status: RateLimitStatus::Allowed,
        },
    );
    usage.windows.insert(
        UsageWindow::SevenDay,
        WindowUsage {
            utilization: Some(0.18),
            resets_at: Some(in_future(now, 4 * 24 * 3600)),
            status: RateLimitStatus::Allowed,
        },
    );
    usage
}

fn sessions(now: SystemTime) -> Vec<PastSession> {
    vec![
        PastSession {
            id: ClaudeSessionId::new("demo-past-1".to_owned()),
            title: "Tokenizer speedup".to_owned(),
            folder: PathBuf::from(PARSER_FOLDER),
            last_activity: at(now, 3 * 3600),
            turns: 37,
        },
        PastSession {
            id: ClaudeSessionId::new("demo-past-2".to_owned()),
            title: "Error page redesign".to_owned(),
            folder: PathBuf::from("/home/dev/projects/docs-site"),
            last_activity: at(now, 3 * 24 * 3600),
            turns: 22,
        },
    ]
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::ReadFileParams;

    use super::*;
    use crate::state::{WaitingKind, file_edits};

    /// `src/error.rs` before the sample's edit.
    const ERROR_BEFORE: &str = "/// A parse error, with where it happened.
pub struct ParseError {
    pub line: u32,
    pub message: String,
}
";

    fn label() -> DaemonLabel {
        DaemonLabel {
            machine: "demo".to_owned(),
            over_ssh: false,
        }
    }

    #[test]
    fn every_waiting_agent_has_its_card() {
        let state = state(label());

        let kinds: Vec<Option<WaitingKind>> = state
            .agents
            .values()
            .map(|entry| entry.transcript.waiting().map(|waiting| waiting.kind))
            .collect();

        assert_eq!(
            kinds,
            [
                None,
                Some(WaitingKind::Question),
                Some(WaitingKind::Approval),
                Some(WaitingKind::Plan),
                None,
                None,
            ]
        );
    }

    #[test]
    fn the_changed_file_undoes_to_its_original() {
        let state = state(label());
        let entry = &state.agents[&AgentId::new(1)];
        let folder = Path::new(PARSER_FOLDER);
        let request = Request::AgentReadFile(ReadFileParams {
            agent: entry.id(),
            path: PathBuf::from(ERROR_FILE),
        });

        let Ok(ResponseBody::File { text }) = answer(&state, &request) else {
            panic!("the demo answers with the file");
        };
        let edits = file_edits::edits_for(&entry.transcript, folder, Path::new(ERROR_FILE));

        assert_eq!(
            file_edits::original_text(&text, &edits).as_deref(),
            Some(ERROR_BEFORE)
        );
    }
}

//! `--demo`: the window filled with made-up agents and no daemon, to look at the interface. The
//! requests that read data are answered from the sample; the others do nothing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cockpit_client::RequestError;
use cockpit_protocol::{
    AgentDetails, AgentId, AgentStatus, AgentSummary, AnswerState, Approval, AskedQuestion,
    BackgroundTask, BackgroundTaskId, BackgroundTaskKind, BackgroundTaskStatus, ChangedFile,
    ClaudeSessionId, CommandKind, ContextCategory, ContextUsage, CostUsd, Effort, ErrorBody,
    ErrorCode, FileEdit, HistoryPage, ItemBody, ItemId, McpScope, McpServer, McpServerStatus,
    McpTool, McpTransport, PastSession, PendingId, PermissionMode, Plan, PlanUsage, Position,
    Question, QuestionOption, RateLimitStatus, Request, ResponseBody, SlashCommand, TaskListItem,
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
const PLATFORM_FOLDER: &str = "/home/dev/projects/acme-platform";
const STREAM_FOLDER: &str = "/home/dev/projects/calc-stream";

/// The agent whose conversation grows while the demo runs, to try the chat's scrolling.
const LIVE_AGENT: u32 = 7;

/// How often the live agent adds or finishes an entry.
pub const LIVE_TICK: Duration = Duration::from_millis(1500);

/// The live agent's entries: the older page that "Load older messages" brings, the first page,
/// then the live ones, one id per tick.
const LIVE_OLDER_FIRST_ID: u32 = 60;
const LIVE_PAGE_FIRST_ID: u32 = 100;
const LIVE_PAGE_LAST_ID: u32 = 123;
const LIVE_TICK_FIRST_ID: u32 = 1000;

/// Other git worktrees of the parser, beside its folder, where the parser agent also edited files.
const PARSER_COLUMNS_WORKTREE: &str =
    "/home/dev/projects/calc-parser_GH42Q-report-the-column-of-every-unexpected-token";
const PARSER_BENCH_WORKTREE: &str = "/home/dev/projects/calc-parser_benchmarks-for-large-inputs";

/// Files of the columns worktree: one the agent changed, one it only read, and a name the parser's
/// own folder has too.
const TOKEN_FILE: &str = "crates/calc-lexer/src/tokens/unexpected_token.rs";
const POSITION_FILE: &str = "crates/calc-lexer/src/position.rs";
const WORKTREE_LIB_FILE: &str = "crates/calc-lexer/src/lib.rs";
const LIB_FILE: &str = "src/lib.rs";

const TOKEN_TEXT: &str = "use crate::position::Position;

/// A token the parser did not expect, with where it starts.
pub struct UnexpectedToken {
    pub text: String,
    pub at: Position,
}
";

const POSITION_TEXT: &str = "/// Where a token starts, as editors count: lines and columns from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

impl Position {
    pub fn start() -> Self {
        Self { line: 1, column: 1 }
    }

    /// Moves past `character`: a new line starts again at column 1.
    pub fn advance(&mut self, character: char) {
        if character == '\\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
    }
}
";

const WORKTREE_LIB_TEXT: &str = "pub mod position;
pub mod tokens;

pub use position::Position;
";

const LIB_TEXT: &str = "mod error;
mod lexer;

pub use error::ParseError;
";

/// Plain `name:line` references, which the daemon looks up: a file the agent read, one it
/// changed, a name two files share, and one that matches nothing.
const LOOKUP_ANSWER: &str = "In the columns worktree, the column math is in position.rs:12-20 and the token type in unexpected_token.rs:4. Both crates re-export it from lib.rs:4. The old cache.rs:10 is gone.";

/// How long the working agent's turn has run when the demo starts.
const TURN_ELAPSED: Duration = Duration::from_secs(4 * 60 + 12);

const ERROR_FILE: &str = "src/error.rs";
const TEST_FILE: &str = "tests/columns.rs";
const LEXER_FILE: &str = "src/lexer.rs";
const NOTES_FILE: &str = "AGENTS.md";

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

/// Long lines, to look at the file view's sideways scroll and its wrap toggle.
const LEXER_TEXT: &str = "//! Splits an expression into tokens, each with the line and the column where it starts, so that the parser can point at the token an error is about.

use crate::token::{Token, TokenKind};

/// The characters that end a number or a name: an operator, a parenthesis, a comma, or white space between two tokens.
const SEPARATORS: [char; 12] = ['+', '-', '*', '/', '%', '^', '(', ')', ',', ' ', '\\t', '\\n'];

pub fn tokens(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let (mut line, mut column) = (1, 1);
    for (offset, character) in input.char_indices() {
        if character == '\\n' {
            line += 1;
            column = 1;
            continue;
        }
        tokens.push(Token { kind: TokenKind::from_char(character), line, column, offset, text: input[offset..offset + character.len_utf8()].to_owned() });
        column += 1;
    }
    tokens
}
";

const LEXER_LINE_BEFORE: &str = "        tokens.push(Token { kind: TokenKind::from_char(character), line, column: column - 1, offset, text: input[offset..offset + character.len_utf8()].to_owned() });\n";

const LEXER_LINE_AFTER: &str = "        tokens.push(Token { kind: TokenKind::from_char(character), line, column, offset, text: input[offset..offset + character.len_utf8()].to_owned() });\n";

const NOTES_TEXT: &str = "# calc-parser

Notes for agents working in this repository.

- Lines and columns count from 1, as editors show them.
- Every error carries the line and the column of the token it is about.
- Run `cargo test` before saying a change is done.
";

/// Markdown links to files of the agent's folder: relative, at a line, and absolute.
const FIX_ANSWER: &str = "Fixed in [the lexer](src/lexer.rs#L17): columns now count from 1, as [AGENTS.md](AGENTS.md) asks. The [error type](/home/dev/projects/calc-parser/src/error.rs) stays as it is. Running the tests again.";

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

/// A command as long as the ones agents write against a database: a few lines, some of them
/// very long.
const MEDIANS_COMMAND: &str = r#"psql "$CI_DATABASE_URL" --no-psqlrc --tuples-only --no-align --field-separator '|' --command "SELECT b.case_name, r.branch, percentile_cont(0.5) WITHIN GROUP (ORDER BY b.nanoseconds) AS median_ns, count(*) AS samples FROM bench_results b JOIN ci_runs r ON r.id = b.run_id WHERE r.finished_at > now() - interval '7 days' AND r.project = 'calc-service' GROUP BY b.case_name, r.branch ORDER BY median_ns DESC" \
  | jq --raw-input --slurp 'split("\n") | map(select(length > 0) | split("|")) | map({case: .[0], branch: .[1], median_ms: ((.[2] | tonumber) / 1000000), samples: (.[3] | tonumber)})' \
  > bench-medians.json
jq --arg branch main '[.[] | select(.branch == $branch)] | sort_by(-.median_ms) | .[:3]' bench-medians.json > main-medians.json
jq --arg branch cache-layer '[.[] | select(.branch == $branch)] | sort_by(-.median_ms) | .[:3]' bench-medians.json > branch-medians.json
curl --silent --fail --request POST "https://ci.example.com/api/v2/annotations" --header "Content-Type: application/json" --data '{"project": "calc-service", "kind": "benchmark-comparison", "window": "7d", "baseline": "main", "candidate": "cache-layer"}'
jq --slurp '[.[0], .[1]] | transpose | map({case: .[0].case, main_ms: .[0].median_ms, branch_ms: .[1].median_ms, change_percent: (((.[1].median_ms / .[0].median_ms) - 1) * 100 | round)})' main-medians.json branch-medians.json"#;

const MEDIANS_OUTPUT: &str = r#"{"annotation": {"id": 4812, "project": "calc-service", "kind": "benchmark-comparison", "created_at": "2026-03-02T10:14:07Z"}}
[
  {
    "case": "deep_nesting",
    "main_ms": 2.61,
    "branch_ms": 2.41,
    "change_percent": -8
  },
  {
    "case": "long_sum",
    "main_ms": 1.08,
    "branch_ms": 1.1,
    "change_percent": 2
  },
  {
    "case": "many_parentheses",
    "main_ms": 0.93,
    "branch_ms": 0.9,
    "change_percent": -3
  }
]"#;

/// The instructions Claude Code adds after a `Skill` call loads the skill.
const BENCH_COMPARE_SKILL: &str = "# /bench-compare — compare two benchmark runs

Compare the medians of two saved runs, and say whether the difference is noise.

## Steps

1. Save a baseline on `main` with `cargo bench -- --save-baseline main`.
2. Run the branch against it with `cargo bench -- --baseline main`.
3. Report each case whose median moved by more than its noise threshold.

## Rules

- Never compare runs from two machines.
- Quote the medians, not the means.
";

const BENCHMARK_ANSWER: &str = "The benchmark runs in CI now. The slowest cases, against `main`:

| Case | `main` | This branch | Change | Samples | Slowest input |
| --- | --- | --- | --- | --- | --- |
| deep_nesting | 2.61 ms | 2.41 ms | -7.7% | 100 | `((((((((((1+2)*3)-4)/5)+6)*7)-8)/9)+10)*11)-12` |
| long_sum | 1.08 ms | 1.10 ms | +1.9% | 100 | `1+2+3+4+5+6+7+8+9+10+11+12+13+14+15+16+17+18+19+20` |
| many_parentheses | 0.93 ms | 0.90 ms | -3.2% | 100 | `((((((((((((((((((((1))))))))))))))))))))` |

To compare two runs by hand:

```bash
# Run both versions and keep the medians
cargo bench --bench eval -- --save-baseline before
git switch cache-layer
cargo bench --bench eval -- --baseline before
```

The CI job fails when a case gets **10%** slower than on `main`.";

const TICKETS_OUTPUT: &str = "#41 Errors point at the wrong column (open, label: errors)
#57 Column is off by one after a tab (open, label: errors)";

const PLAN_TEXT: &str = "## Add a result cache

1. Add a `Cache` type in `src/cache.rs`, keyed by the normalized expression.
2. Look the expression up before evaluating it in `src/server.rs`.
3. Drop entries older than ten minutes.
4. Add a test that evaluates the same expression twice.";

/// The sidebar's order, top first, as a user arranged it: neither by id nor by status.
const SIDEBAR_ORDER: [u32; 7] = [1, LIVE_AGENT, 4, 2, 5, 3, 6];

/// The room between two agents' positions, as the daemon leaves it.
const POSITION_STEP: u32 = 1024;

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
        live_agent(now),
    ];
    for sample in agents {
        let mut entry = AgentEntry::new(sample.summary);
        let has_older = entry.id() == AgentId::new(LIVE_AGENT);
        entry.transcript.merge_page(HistoryPage {
            items: sample.items,
            has_older,
        });
        entry.details = Loadable::Loaded(sample.details);
        if entry.summary.status == AgentStatus::Working {
            entry.turn_started = Instant::now().checked_sub(TURN_ELAPSED);
        }
        state.agents.insert(entry.id(), entry);
    }
    // The bottom agent first: positions grow towards the top.
    for (step, id) in (1_u32..).zip(SIDEBAR_ORDER.into_iter().rev()) {
        if let Some(entry) = state.agents.get_mut(&AgentId::new(id)) {
            entry.summary.position = Position::new(step * POSITION_STEP);
        }
    }
    state.plan_usage = plan_usage(now);
    state.ui.selected = Some(AgentId::new(1));
    state.ui.unread.insert(AgentId::new(5));
    state
}

/// How long the demo takes to answer, as a daemon would for the past sessions and the context
/// usage, so that their loading states show.
pub fn latency(request: &Request) -> Duration {
    match request {
        Request::SessionsList => Duration::from_millis(900),
        Request::AgentContextUsage(_) => Duration::from_millis(700),
        _ => Duration::ZERO,
    }
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
        Request::AgentReadFile(params) => read_file(&params.path),
        Request::AgentHistory(params) if params.agent == AgentId::new(LIVE_AGENT) => {
            let before = params.before.map_or(LIVE_PAGE_FIRST_ID, ItemId::get);
            let items = (LIVE_OLDER_FIRST_ID..before)
                .map(|id| item(id, None, live_entry(id)))
                .collect();
            Ok(ResponseBody::History(HistoryPage {
                items,
                has_older: false,
            }))
        }
        Request::AgentHistory(_) => Ok(ResponseBody::History(HistoryPage {
            items: Vec::new(),
            has_older: false,
        })),
        Request::SessionsList => Ok(ResponseBody::Sessions {
            sessions: sessions(SystemTime::now()),
        }),
        Request::AgentMove(params) => {
            let known = |agent: AgentId| state.agents.contains_key(&agent);
            if known(params.agent) && params.before.is_none_or(known) {
                Ok(ResponseBody::Done)
            } else {
                Err(refused(ErrorCode::NotFound, "The demo has no such agent."))
            }
        }
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

/// The sample's files, absolute, with their text.
fn files() -> [(PathBuf, &'static str); 9] {
    let parser = Path::new(PARSER_FOLDER);
    let columns = Path::new(PARSER_COLUMNS_WORKTREE);
    [
        (parser.join(ERROR_FILE), ERROR_AFTER),
        (parser.join(TEST_FILE), TEST_TEXT),
        (parser.join(LEXER_FILE), LEXER_TEXT),
        (parser.join(NOTES_FILE), NOTES_TEXT),
        (parser.join(LIB_FILE), LIB_TEXT),
        (columns.join(TOKEN_FILE), TOKEN_TEXT),
        (columns.join(POSITION_FILE), POSITION_TEXT),
        (columns.join(WORKTREE_LIB_FILE), WORKTREE_LIB_TEXT),
        (
            Path::new(PARSER_BENCH_WORKTREE).join("README.md"),
            NOTES_TEXT,
        ),
    ]
}

/// As the daemon answers for the parser agent: the path itself, else the sample's files whose
/// path ends with it, one read or several listed.
fn read_file(path: &Path) -> Result<ResponseBody, RequestError> {
    let files = files();
    let wanted = Path::new(PARSER_FOLDER).join(path);
    let matches: Vec<&(PathBuf, &str)> = match files.iter().find(|(file, _)| *file == wanted) {
        Some(exact) => vec![exact],
        None => files
            .iter()
            .filter(|(file, _)| file.ends_with(path))
            .collect(),
    };
    match matches.as_slice() {
        [] => Err(refused(
            ErrorCode::NotFound,
            &format!(
                "{} is not in the agent's folder or in the files it read or changed",
                path.display()
            ),
        )),
        [(path, text)] => Ok(ResponseBody::File {
            path: Some(path.clone()),
            text: (*text).to_owned(),
        }),
        several => Ok(ResponseBody::FileMatches {
            matches: several.iter().map(|(path, _)| path.clone()).collect(),
        }),
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
        effort_is_default: false,
        mode: PermissionMode::Default,
        task_progress: None,
        context_percent: Some(31),
        cache_expires_at: Some(in_future(now, 6 * 60)),
        turns: 9,
        cost: CostUsd::new(1.84),
        started_at: at(now, 3600),
        session: Some(ClaudeSessionId::new(format!("demo-session-{}", parts.id))),
        position: Position::default(),
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

/// A `Bash` call with the description Claude Code sends with it.
fn bash(
    description: &str,
    command: &str,
    status: ToolCallStatus,
    output: Option<&str>,
) -> ToolCall {
    ToolCall {
        input: serde_json::json!({"command": command, "description": description}),
        ..call("Bash", command, status, output)
    }
}

/// A `Read` of an absolute path, as Claude Code writes its input.
fn read_call(path: &Path) -> ToolCall {
    let shown = path.display().to_string();
    ToolCall {
        input: serde_json::json!({ "file_path": shown }),
        ..call("Read", &shown, ToolCallStatus::Done, None)
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
            "summarize-open-pull-requests",
            "List the open pull requests with their review state, the checks that fail and who waits on whom",
            CommandKind::Skill,
        ),
        command(
            "update-dependency-lockfiles",
            "Bump the locked versions of every dependency, run the tests and list what changed",
            CommandKind::Skill,
        ),
        command(
            "write-release-notes-from-tags",
            "Write the release notes from the commits between the last two tags",
            CommandKind::Skill,
        ),
        command(
            "explain-failing-ci-job",
            "Read the log of the last failed CI job and explain why it failed",
            CommandKind::Skill,
        ),
        command("benchmark-hot-paths", "", CommandKind::Skill),
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
        command(
            "clear",
            "Clear the conversation history and free up context",
            CommandKind::Command,
        ),
    ]
}

fn details() -> AgentDetails {
    AgentDetails {
        mcp_servers: mcp_servers(),
        commands: commands(),
        ..AgentDetails::default()
    }
}

/// Three running tasks among many finished ones, in the order they started, as the daemon lists
/// them: the block shows the running ones first and folds the older finished ones.
fn parser_background_tasks(now: SystemTime) -> Vec<BackgroundTask> {
    use BackgroundTaskKind::{Agent, Command};
    use BackgroundTaskStatus::{Done, Failed, Running, Stopped};
    // Kind, description, status, started and ended so many seconds ago, tool calls.
    #[rustfmt::skip]
    let tasks = [
        (Agent, "Map where the lexer builds tokens", Done, 1500, Some(1380), 14),
        (Agent, "List the parser's error messages", Done, 1460, Some(1300), 9),
        (Command, "cargo build --all-targets", Done, 1400, Some(1340), 0),
        (Agent, "Check the grammar for ambiguous rules", Failed, 1350, Some(1200), 21),
        (Agent, "Review the error type for missing cases", Running, 1290, None, 5),
        (Agent, "Find tests that assert on columns", Done, 1250, Some(1050), 12),
        (Command, "cargo clippy --all-targets", Done, 1210, Some(1150), 0),
        (Agent, "Compare with the column rules of other parsers", Done, 1100, Some(800), 18),
        (Command, "cargo test lexer", Stopped, 980, Some(950), 0),
        (Agent, "Draft the changelog entry", Done, 900, Some(700), 6),
        (Agent, "Look for callers of Token::column", Done, 860, Some(640), 11),
        (Agent, "Write fuzz inputs with unicode", Running, 600, None, 8),
        (Command, "cargo doc --no-deps", Done, 420, Some(380), 0),
        (Agent, "Check the docs for column examples", Done, 300, Some(150), 7),
        (Agent, "Summarize the error type review", Done, 200, Some(90), 4),
        (Command, "cargo test parser", Running, 12, None, 0),
    ];
    (1_u32..)
        .zip(tasks)
        .map(
            |(index, (kind, description, status, started, ended, tool_calls))| BackgroundTask {
                id: BackgroundTaskId::new(format!("demo-task-{index}")),
                kind,
                description: description.to_owned(),
                status,
                started_at: at(now, started),
                ended_at: ended.map(|ended| at(now, ended)),
                tool_calls,
            },
        )
        .collect()
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
        ..bash(
            "Run the parser tests",
            "cargo test parser",
            ToolCallStatus::Failed,
            Some(FAILED_TEST_OUTPUT),
        )
    };
    let tickets = ToolCall {
        input: serde_json::json!({"project": "calc-parser", "label": "errors", "state": "open"}),
        ..call(
            "mcp__tickets__list_tickets",
            "",
            ToolCallStatus::Done,
            Some(TICKETS_OUTPUT),
        )
    };
    let position_read = read_call(&Path::new(PARSER_COLUMNS_WORKTREE).join(POSITION_FILE));
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
            ItemBody::Thinking {
                text: "The lexer knows each token's column. The question is where the parser builds its errors, and whether one place builds them all.".to_owned(),
            },
        ),
        item(
            3,
            None,
            text("I will have a subagent map where the errors are built first."),
        ),
        item(
            4,
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
            5,
            Some(4),
            ItemBody::ToolCall(call("Read", "src/parser.rs", ToolCallStatus::Done, None)),
        ),
        item(
            6,
            Some(4),
            ItemBody::ToolCall(call(
                "Grep",
                "ParseError in src/",
                ToolCallStatus::Done,
                Some("3 matches"),
            )),
        ),
        item(
            7,
            Some(4),
            ItemBody::ToolCall(bash(
                "Count the places that build an error",
                "rg -c 'ParseError \\{' src/",
                ToolCallStatus::Done,
                Some("src/parser.rs:3"),
            )),
        ),
        item(
            8,
            Some(4),
            ItemBody::ToolCall(call("Read", ERROR_FILE, ToolCallStatus::Done, None)),
        ),
        item(9, None, ItemBody::ToolCall(tickets)),
        item(10, None, text(FINDINGS)),
        item(
            11,
            None,
            edit_call(
                "Edit",
                ERROR_FILE,
                "    pub line: u32,\n",
                "    pub line: u32,\n    /// Counted from 1, as editors show it.\n    pub column: u32,\n",
            ),
        ),
        item(12, None, edit_call("Write", TEST_FILE, "", TEST_TEXT)),
        item(13, None, ItemBody::ToolCall(failed)),
        item(
            14,
            None,
            text(
                "The test expects the column to count from 1, and the lexer counts from 0. See `tests/columns.rs:6`. I am fixing the lexer.",
            ),
        ),
        item(
            15,
            None,
            edit_call("Edit", LEXER_FILE, LEXER_LINE_BEFORE, LEXER_LINE_AFTER),
        ),
        item(16, None, text(FIX_ANSWER)),
        item(17, None, ItemBody::ToolCall(position_read)),
        item(18, None, text(LOOKUP_ANSWER)),
        item(
            19,
            None,
            ItemBody::ToolCall(bash(
                "Run the parser tests again",
                "cargo test parser",
                ToolCallStatus::Running,
                None,
            )),
        ),
        item(
            20,
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
        background_tasks: parser_background_tasks(now),
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
            ChangedFile {
                path: Path::new(PARSER_FOLDER).join(LEXER_FILE),
                lines_added: 1,
                lines_removed: 1,
            },
            ChangedFile {
                path: Path::new(PARSER_COLUMNS_WORKTREE)
                    .join("crates/calc-lexer/src/tokens/unexpected_token.rs"),
                lines_added: 31,
                lines_removed: 4,
            },
            ChangedFile {
                path: Path::new(PARSER_COLUMNS_WORKTREE)
                    .join("crates/calc-lexer/tests/columns_of_unexpected_tokens.rs"),
                lines_added: 24,
                lines_removed: 0,
            },
            ChangedFile {
                path: Path::new(PARSER_BENCH_WORKTREE).join("benches/lexer_on_large_inputs.rs"),
                lines_added: 12,
                lines_removed: 2,
            },
        ],
        context: Some(ContextUsage {
            tokens: 112_000,
            max_tokens: 200_000,
            percent: 56,
            categories: [
                ("System prompt", 3_400),
                ("System tools", 14_200),
                ("MCP tools", 5_800),
                ("MCP server instructions", 900),
                ("Custom agents", 1_300),
                ("Memory files", 6_100),
                ("Skills", 2_000),
                ("Messages", 78_300),
                ("Autocompact buffer", 45_000),
                ("Free space", 43_000),
            ]
            .into_iter()
            .map(|(name, tokens)| ContextCategory {
                name: name.to_owned(),
                tokens,
            })
            .collect(),
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
            ItemBody::ToolCall(bash(
                "List the commits since the last release",
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
            ItemBody::ToolCall(bash(
                "Check the links",
                "npm run check-links",
                ToolCallStatus::Done,
                Some("0 broken links"),
            )),
        ),
        item(
            3,
            None,
            ItemBody::ToolCall(bash(
                "Push the branch so the preview builds",
                "git push -u origin fix-links",
                ToolCallStatus::Running,
                None,
            )),
        ),
        item(
            4,
            None,
            ItemBody::Approval(Approval {
                pending: PendingId::new("demo-approval".to_owned()),
                tool: "Bash".to_owned(),
                summary: "git push -u origin fix-links".to_owned(),
                input: Value::Null,
                edit: None,
                reason: Some("Push the branch so the preview builds.".to_owned()),
                answer: AnswerState::Waiting,
                tool_call: None,
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
            ItemBody::ToolCall(ToolCall {
                input: serde_json::json!({"skill": "bench-compare"}),
                ..call(
                    "Skill",
                    "bench-compare",
                    ToolCallStatus::Done,
                    Some("Launching skill: bench-compare"),
                )
            }),
        ),
        item(
            3,
            None,
            ItemBody::Notice {
                text: "Skill instructions: /bench-compare".to_owned(),
                detail: Some(BENCH_COMPARE_SKILL.to_owned()),
            },
        ),
        item(
            4,
            None,
            ItemBody::ToolCall(bash(
                "Run the benchmark",
                "cargo bench --bench eval",
                ToolCallStatus::Done,
                Some("eval/deep_nesting  time: [2.38 ms 2.41 ms 2.44 ms]"),
            )),
        ),
        item(
            5,
            None,
            ItemBody::ToolCall(bash(
                "Compare the last week's medians of main and the branch",
                MEDIANS_COMMAND,
                ToolCallStatus::Done,
                Some(MEDIANS_OUTPUT),
            )),
        ),
        item(6, None, text(BENCHMARK_ANSWER)),
        item(
            7,
            None,
            ItemBody::TurnEnd(TurnEnd {
                duration: Duration::from_secs(252),
                cost: CostUsd::new(0.62),
                usage: TokenUsage::default(),
                error: None,
                stopped: false,
            }),
        ),
    ];
    let details = AgentDetails {
        background_tasks: vec![BackgroundTask {
            id: BackgroundTaskId::new("demo-task-3".to_owned()),
            kind: BackgroundTaskKind::Command,
            description: "cargo bench --bench eval -- --save-baseline before".to_owned(),
            status: BackgroundTaskStatus::Stopped,
            started_at: at(now, 900),
            ended_at: Some(at(now, 840)),
            tool_calls: 0,
        }],
        ..details()
    };
    Sample {
        summary,
        items,
        details,
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
                detail: None,
            },
        ),
    ];
    Sample {
        summary,
        items,
        details: details(),
    }
}

fn live_agent(now: SystemTime) -> Sample {
    let summary = summary(
        SummaryParts {
            id: LIVE_AGENT,
            title: "Stream the parser's input",
            folder: STREAM_FOLDER,
            status: AgentStatus::Working,
            activity: Some("Running the case tests"),
            pending: 0,
        },
        now,
    );
    let first = item(
        LIVE_PAGE_FIRST_ID,
        None,
        user(
            "Make the parser read its input as a stream, and run every case test.",
            false,
        ),
    );
    let page =
        (LIVE_PAGE_FIRST_ID + 1..=LIVE_PAGE_LAST_ID).map(|id| item(id, None, live_entry(id)));
    Sample {
        summary,
        items: std::iter::once(first).chain(page).collect(),
        details: details(),
    }
}

/// The kinds of the live agent's entries, as `live_entry` makes them.
const LIVE_TEXT: u32 = 0;
const LIVE_READ: u32 = 2;

/// A made-up entry of the live agent, picked by its id.
fn live_entry(id: u32) -> ItemBody {
    live_entry_of(id % 4, id)
}

/// An entry of the live agent of the `kind` (0 to 3) about the case `id`.
fn live_entry_of(kind: u32, id: u32) -> ItemBody {
    match kind {
        LIVE_TEXT => text(&format!(
            "Case {id} passes: the stream splits the input at each separator and keeps every token's column. On to the next case."
        )),
        1 => ItemBody::ToolCall(bash(
            "Run one case test",
            &format!("cargo test -p calc-stream case_{id}"),
            ToolCallStatus::Done,
            Some(&live_test_output(id)),
        )),
        LIVE_READ => ItemBody::ToolCall(call(
            "Read",
            &format!("src/stream/case_{id}.rs"),
            ToolCallStatus::Done,
            None,
        )),
        _ => ItemBody::ToolCall(call(
            "Grep",
            &format!("fn case_{id}"),
            ToolCallStatus::Done,
            Some(&format!("src/stream/case_{id}.rs:12")),
        )),
    }
}

fn live_test_output(id: u32) -> String {
    format!(
        "running 1 test\ntest case_{id}::splits_at_separators ... ok\ntest case_{id}::keeps_columns ... ok\ntest case_{id}::reads_in_chunks ... ok\ntest case_{id}::ends_cleanly ... ok\ntest case_{id}::reports_offsets ... ok\n\ntest result: ok. 5 passed; 0 failed"
    )
}

/// Brings back an agent the app closed, as a daemon would from its session: on top of the list,
/// under its old id. Answers `sessions.resume` for a session in `closed`, and nothing else.
pub fn reopen(
    state: &mut AppState,
    closed: &mut Vec<AgentEntry>,
    request: &Request,
) -> Option<Result<ResponseBody, RequestError>> {
    let Request::SessionsResume(params) = request else {
        return None;
    };
    let index = closed
        .iter()
        .position(|entry| entry.summary.session.as_ref() == Some(&params.session))?;
    let mut entry = closed.remove(index);
    let top = state
        .agents
        .values()
        .map(|other| other.summary.position.get())
        .max()
        .unwrap_or(0);
    entry.summary.position = Position::new(top.saturating_add(POSITION_STEP));
    let agent = entry.id();
    state.ui.closing.remove(&agent);
    state.agents.insert(agent, entry);
    Some(Ok(ResponseBody::AgentStarted { agent }))
}

/// One step of the live agent, as its daemon would send it: tick by tick, a test command starts,
/// then ends with its output, which makes its entry taller, then a line of text, then a read.
pub fn live_tick(state: &mut AppState, tick: u32) {
    let Some(entry) = state.agents.get_mut(&AgentId::new(LIVE_AGENT)) else {
        return;
    };
    let id = LIVE_TICK_FIRST_ID.saturating_add(tick);
    let command = |id: u32, status, output: Option<&str>| {
        ItemBody::ToolCall(bash(
            "Run one case test",
            &format!("cargo test -p calc-stream case_{id}"),
            status,
            output,
        ))
    };
    match tick % 4 {
        0 => entry
            .transcript
            .append(item(id, None, command(id, ToolCallStatus::Running, None))),
        1 => {
            let started = id.saturating_sub(1);
            let output = live_test_output(started);
            let done = command(started, ToolCallStatus::Done, Some(&output));
            entry.transcript.change(item(started, None, done));
        }
        2 => entry
            .transcript
            .append(item(id, None, live_entry_of(LIVE_TEXT, id))),
        _ => entry
            .transcript
            .append(item(id, None, live_entry_of(LIVE_READ, id))),
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
            utilization: Some(0.86),
            resets_at: Some(in_future(now, 4 * 24 * 3600)),
            status: RateLimitStatus::Allowed,
        },
    );
    usage
}

fn sessions(now: SystemTime) -> Vec<PastSession> {
    // Long folders used more recently than the agents', so the new agent dialog lists six of them.
    let long_folders = [
        (
            "services/billing/reconciliation-worker",
            "Reconcile late payments",
        ),
        ("services/billing/invoice-pdf-renderer", "Invoice PDF fonts"),
        (
            "frontends/customer-portal/packages/checkout-flow",
            "Checkout address form",
        ),
        (
            "infrastructure/terraform/modules/object-storage",
            "Bucket lifecycle rules",
        ),
        (
            "tools/release-automation/changelog-generator",
            "Changelog from commits",
        ),
        (
            "services/search/indexer-backfill-jobs",
            "Backfill the search index",
        ),
    ];
    let mut sessions: Vec<PastSession> = Vec::new();
    for (index, (folder, title)) in (1_u64..).zip(long_folders) {
        sessions.push(PastSession {
            id: ClaudeSessionId::new(format!("demo-past-long-{index}")),
            title: title.to_owned(),
            folder: Path::new(PLATFORM_FOLDER).join(folder),
            last_activity: at(now, index * 7 * 60),
            turns: 12,
        });
    }
    sessions.extend([
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
    ]);
    sessions
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{HistoryParams, ReadFileParams};

    use super::*;
    use crate::state::{WaitingKind, file_edits};

    #[test]
    fn the_live_agent_starts_a_command_finishes_it_then_adds_entries() {
        let mut state = state(label());
        let agent = AgentId::new(LIVE_AGENT);
        let status = |state: &AppState| match state.agents[&agent]
            .transcript
            .get(ItemId::new(LIVE_TICK_FIRST_ID))
            .map(|item| &item.body)
        {
            Some(ItemBody::ToolCall(call)) => Some((call.status, call.output.is_some())),
            _ => None,
        };
        let count = |state: &AppState| state.agents[&agent].transcript.items().count();
        let before = count(&state);

        live_tick(&mut state, 0);
        let started = status(&state);
        live_tick(&mut state, 1);
        let finished = status(&state);
        live_tick(&mut state, 2);
        live_tick(&mut state, 3);

        assert_eq!(started, Some((ToolCallStatus::Running, false)));
        assert_eq!(finished, Some((ToolCallStatus::Done, true)));
        assert_eq!(count(&state), before + 3);
    }

    #[test]
    fn the_live_agent_has_an_older_page_to_load() {
        let state = state(label());
        let request = Request::AgentHistory(HistoryParams {
            agent: AgentId::new(LIVE_AGENT),
            before: Some(ItemId::new(LIVE_PAGE_FIRST_ID)),
            limit: 100,
        });

        let Ok(ResponseBody::History(page)) = answer(&state, &request) else {
            panic!("the live agent answers its history");
        };

        assert_eq!(page.items.len(), 40);
        assert!(!page.has_older);
    }

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
                None,
            ]
        );
    }

    #[test]
    fn names_are_looked_up_as_the_daemon_does() {
        let read = |path: &str| read_file(Path::new(path));
        let position = Path::new(PARSER_COLUMNS_WORKTREE).join(POSITION_FILE);
        let lib = Path::new(PARSER_FOLDER).join(LIB_FILE);

        assert!(matches!(
            read("position.rs"),
            Ok(ResponseBody::File { path, .. }) if path == Some(position)
        ));
        assert!(
            matches!(read("lib.rs"), Ok(ResponseBody::FileMatches { matches }) if matches.len() == 2)
        );
        assert!(matches!(
            read("src/lib.rs"),
            Ok(ResponseBody::File { path, .. }) if path == Some(lib)
        ));
        assert!(matches!(
            read("cache.rs"),
            Err(RequestError::Failed(body)) if body.code == ErrorCode::NotFound
        ));
    }

    #[test]
    fn undo_brings_back_an_agent_the_demo_closed() {
        let mut state = state(label());
        let mut drafts = crate::state::Drafts::default();
        let closing =
            crate::state::actions::request_close(&mut state, &mut drafts, AgentId::new(5));
        let mut closed: Vec<AgentEntry> = closing
            .iter()
            .filter_map(|outgoing| match &outgoing.purpose {
                crate::state::Purpose::Close { entry } => Some(entry.as_ref().clone()),
                _ => None,
            })
            .collect();
        let undo = crate::state::actions::undo_close(&mut state);

        let reopened = undo
            .first()
            .and_then(|outgoing| reopen(&mut state, &mut closed, &outgoing.request));

        assert!(matches!(
            reopened,
            Some(Ok(ResponseBody::AgentStarted { agent })) if agent == AgentId::new(5)
        ));
        assert!(state.agents.contains_key(&AgentId::new(5)));
        assert!(closed.is_empty());
    }

    #[test]
    fn the_sidebar_shows_the_order_the_user_set() {
        let state = state(label());

        let order: Vec<u32> = state
            .agents_in_order()
            .iter()
            .map(|entry| entry.id().get())
            .collect();

        assert_eq!(order, SIDEBAR_ORDER);
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

        let Ok(ResponseBody::File { text, .. }) = answer(&state, &request) else {
            panic!("the demo answers with the file");
        };
        let edits = file_edits::edits_for(&entry.transcript, folder, Path::new(ERROR_FILE));

        assert_eq!(
            file_edits::original_text(&text, &edits).as_deref(),
            Some(ERROR_BEFORE)
        );
    }
}

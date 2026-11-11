//! `--demo`: the window filled with made-up agents and no daemon, to look at the interface. The
//! requests that read data are answered from the sample; the others do nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cockpit_client::RequestError;
use cockpit_protocol::{
    Account, AccountId, AccountState, AgentDetails, AgentHistory, AgentId, AgentLoad, AgentStatus,
    AgentSummary, Answer, AnswerState, Approval, AskedQuestion, BackgroundTask, BackgroundTaskId,
    BackgroundTaskKind, BackgroundTaskStatus, ChangedFile, ClaudeRoute, ClaudeSessionId,
    CommandKind, CommandLoad, Completeness, ContextCategory, ContextUsage, Cores, CostUsd,
    CredentialSource, DirEntry, DirListing, Effort, EffortSource, EntryKind, EntryLink,
    EnvironmentCredentials, ErrorBody, ErrorCode, FileEdit, GitIgnore, HistoryPage, Image,
    ImageMediaType, ItemBody, ItemId, LoadAverage, LoadHistory, MachineLoad, Mark, McpScope,
    McpServer, McpServerStatus, McpTool, McpTransport, OlderItems, PastSession, PendingId,
    PermissionMode, Plan, PlanUsage, Position, ProcessId, Prompt, PromptId, Question,
    QuestionOption, RateLimitStatus, ReleaseVersion, Request, ResourceUse, ResponseBody, Selection,
    ServerLoad, ServerPoint, SignIn, SignInWait, SlashCommand, Subagent, SubagentRun,
    SubagentStatus, SubagentToolCall, TaskListItem, TaskProgress, TaskStatus, Timestamp,
    TokenUsage, ToolCall, ToolCallStatus, TranscriptItem, TurnEnd, TurnOutcome, UsageWindow,
    UserMessage, WaitingPrompt, WaitingState, WindowUsage,
};
use serde_json::Value;

use crate::daemon_label::DaemonLabel;
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::CommentPlacement;
use crate::state::daemon_update::{
    self, OutputLine, OutputStream, ProtocolGap, UpdateFailure, UpdatePlan, VersionGap,
};
use crate::state::file_tree::TreeFilter;
use crate::state::no_daemon::{self, Absence};
use crate::state::{
    AgentEntry, AppState, ConnectionState, Dialog, Drafts, Loadable, MachineState, Utf16Offset,
    file_edits, file_tree, quotes, sending,
};
use crate::update_script::APP_VERSION;

/// The command-line flag that starts the demo.
pub const DEMO_FLAG: &str = "--demo";

/// With the demo, opens one of the screens of a daemon's update: `--demo-update older`.
const UPDATE_SCREEN_FLAG: &str = "--demo-update";

/// With the demo, trims it for the README's picture.
const SHOWCASE_FLAG: &str = "--demo-showcase";

/// The showcase's third machine.
const GPU_SERVER: MachineId = MachineId::new(3);
const GPU_HOST: &str = "gpu-box";
const GPU_FOLDER: &str = "/home/dev/ml/ranking";

/// The agents of this computer the showcase leaves out: the sign-in, the map and the plan.
const SHOWCASE_LEFT_OUT: [u32; 3] = [SIGN_IN_AGENT, MAP_AGENT, PLAN_AGENT];
/// The agent that waits for an approval, which the showcase moves to the server.
const APPROVAL_AGENT: u32 = 3;
/// The agent that waits for its plan to be approved.
const PLAN_AGENT: u32 = 4;
/// The agent that finished its benchmark while the user looked elsewhere.
const BENCHMARK_AGENT: u32 = 5;
/// A subagent the selected agent runs in the background, which the showcase leaves out.
const FUZZ_TASK: &str = "Write fuzz inputs with unicode";

const UPDATE_MACHINE: &str = "build-box";
/// A made-up commit.
const UPDATE_COMMIT: &str = "3f9c2e17ab54c90d1e2f3a4b5c6d7e8f90a1b2c3";
const REFUSED_FOR_OLDER: &str = "the daemon speaks protocol 1, the client 4";

/// How long the demo's update waits before each line of its output.
pub const UPDATE_LINE_PAUSE: Duration = Duration::from_millis(700);

/// The script's lines on the demo's successful update.
pub const UPDATE_OUTPUT: &[&str] = &[
    "update-daemon: building 3f9c2e17ab54c90d1e2f3a4b5c6d7e8f90a1b2c3",
    "   Compiling cockpit_protocol v0.1.0",
    "   Compiling cockpit_client v0.1.0",
    "   Compiling cockpit_daemon v0.1.0",
    "    Finished `release` profile [optimized] target(s) in 3m 12s",
    "update-daemon: restarting the daemon",
    "update-daemon: the daemon runs 3f9c2e17ab54c90d1e2f3a4b5c6d7e8f90a1b2c3",
];

const PARSER_FOLDER: &str = "/home/dev/projects/calc-parser";
const SERVICE_FOLDER: &str = "/home/dev/projects/calc-service";
const PLATFORM_FOLDER: &str = "/home/dev/projects/acme-platform";
const STREAM_FOLDER: &str = "/home/dev/projects/calc-stream";

/// The agent whose conversation grows while the demo runs, to try the chat's scrolling. Its
/// conversation is long, and it loads when the agent first opens, as a daemon's would.
const LIVE_AGENT: u32 = 7;

/// How long the demo's running tool calls have run when the window opens.
const RUNNING_CALLS_FOR: Duration = Duration::from_secs(48);
/// A second Claude account, which the service's agents and the platform's past sessions use.
const WORK_ACCOUNT: u32 = 1;
/// The agents of the work account.
const WORK_AGENTS: [u32; 2] = [PLAN_AGENT, BENCHMARK_AGENT];
/// An account that bills the daemon's API key, which the map's agent uses.
const LAB_ACCOUNT: u32 = 2;
/// An account no one signed in to yet, whose agent waits for the sign-in.
const CLIENT_ACCOUNT: u32 = 3;
const SIGN_IN_AGENT: u32 = 9;
const SIGN_IN_FOLDER: &str = "/home/dev/projects/billing";

/// The screenshot of the failing test, in a message of the selected agent.
const FAILING_TEST_PNG: &[u8] = include_bytes!("../assets/demo-failing-test.png");

/// The second machine: a server with agents of its own.
const SERVER: MachineId = MachineId::new(1);
const SERVER_HOST: &str = "devbox";
const SERVER_FOLDER: &str = "/home/dev/services/invoices";

/// The selected agent's explanation of the failing test, and its last answer.
const COUNT_ITEM: u32 = 15;
const COUNT_ANSWER: &str = "The test expects the column to count from 1, and the lexer counts from 0. See `tests/columns.rs:6`. I am fixing the lexer.";
const LOOKUP_ITEM: u32 = 20;

/// A passage of the selected agent's answers that the demo's draft quotes, with its comment.
struct DemoQuote {
    item: u32,
    /// The text of the answer the passage is in.
    answer: &'static str,
    passage: &'static str,
    comment: &'static str,
}

/// The demo draft's quotes, in the chat's order.
const DEMO_QUOTES: [DemoQuote; 3] = [
    DemoQuote {
        item: COUNT_ITEM,
        answer: COUNT_ANSWER,
        passage: "the lexer counts from 0",
        comment: "Keep 0 inside the lexer, and add 1 where errors are built.",
    },
    DemoQuote {
        item: LOOKUP_ITEM,
        answer: LOOKUP_ANSWER,
        passage: "the column math is in position.rs:12-20",
        comment: "Does it count a tab as one column?",
    },
    DemoQuote {
        item: LOOKUP_ITEM,
        answer: LOOKUP_ANSWER,
        passage: "The old cache.rs:10 is gone.",
        comment: "Then drop its import in lib.rs too.",
    },
];

/// How often the live agent adds or finishes an entry.
pub const LIVE_TICK: Duration = Duration::from_millis(1500);

/// The live agent's entries: the older page that "Load older messages" brings, the first page,
/// then the live ones, one id per tick.
const LIVE_OLDER_FIRST_ID: u32 = 60;
const LIVE_PAGE_FIRST_ID: u32 = 100;
const LIVE_PAGE_LAST_ID: u32 = 239;
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

const CARGO_TEXT: &str = "[package]
name = \"calc-parser\"
version = \"0.4.0\"
edition = \"2024\"

[dependencies]

[dev-dependencies]
criterion = \"0.5\"

[[bench]]
name = \"eval\"
harness = false
";

const README_TEXT: &str = "# calc-parser

Parses and evaluates arithmetic expressions, and says where an error is: line and column.

    cargo run -- \"1 + 2 * 3\"
";

const MAIN_TEXT: &str = "use calc_parser::parse;

fn main() {
    let input = std::env::args().nth(1).unwrap_or_default();
    match parse(&input) {
        Ok(value) => println!(\"{value}\"),
        Err(error) => eprintln!(\"{}:{}: {}\", error.line, error.column, error.message),
    }
}
";

const PARSER_TEXT: &str = "use crate::error::ParseError;
use crate::lexer::tokens;

/// Parses `input` and evaluates it.
pub fn parse(input: &str) -> Result<f64, ParseError> {
    let tokens = tokens(input);
    evaluate(&tokens)
}
";

const TOKEN_KIND_TEXT: &str = "/// What a token is.
pub enum TokenKind {
    Number,
    Operator,
    Parenthesis,
}
";

const BENCH_TEXT: &str = "use criterion::{criterion_group, criterion_main, Criterion};

fn deep_nesting(c: &mut Criterion) {
    let input = format!(\"{}1{}\", \"(\".repeat(200), \")\".repeat(200));
    c.bench_function(\"deep_nesting\", |b| b.iter(|| calc_parser::parse(&input)));
}

criterion_group!(benches, deep_nesting);
criterion_main!(benches);
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

## Error positions

| Input | Error | Line | Column |
| --- | --- | --- | --- |
| `1 +` | unexpected end | 1 | 4 |
| `2 * (3` | missing `)` | 1 | 7 |

## Before a release

- [x] Count columns from 1
- [ ] Report the line of an error in a multi-line input
- [ ] Add a benchmark for deep nesting, in `benches/`
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

The CI job fails when a case gets **10%** slower than on `main`.

![The last run's chart](/home/dev/projects/calc-parser/target/criterion/report.png)

The CI run is at https://ci.example.org/calc-parser/runs/812.";

/// A long request, which the chat folds to its first lines.
const MIGRATION_REQUEST: &str = "Write the migration that adds the archive table.

The table keeps the orders closed for more than a year:

- `id`, the order's id, as the primary key.
- `closed_at`, a timestamp, indexed.
- `payload`, the order as JSON.

Move the rows in batches of 1000, so that the lock on `orders` stays short. Keep the old rows until the copy is checked, then delete them in a second migration.

The migration must run on the staging copy first. Write down how long it takes there, and the size of both tables before and after.

Add a down migration that moves the rows back, and a test that runs both ways on a small sample.";

const TICKETS_OUTPUT: &str = "#41 Errors point at the wrong column (open, label: errors)
#57 Column is off by one after a tab (open, label: errors)";

const PLAN_TEXT: &str = "## Add a result cache

1. Add a `Cache` type in `src/cache.rs`, keyed by the normalized expression.
2. Look the expression up before evaluating it in `src/server.rs`.
3. Drop entries older than ten minutes.
4. Add a test that evaluates the same expression twice.";

/// The sidebar's order, top first, as a user arranged it: neither by id nor by status.
const SIDEBAR_ORDER: [u32; 9] = [
    1,
    SIGN_IN_AGENT,
    MAP_AGENT,
    LIVE_AGENT,
    PLAN_AGENT,
    2,
    BENCHMARK_AGENT,
    APPROVAL_AGENT,
    6,
];

/// The room between two agents' positions, as the daemon leaves it.
const POSITION_STEP: u32 = 1024;

/// The screen named by `--demo-update`, which only the demo opens.
pub fn update_screen(arguments: &[String]) -> Option<&str> {
    if !is_requested(arguments) {
        return None;
    }
    let flag = arguments
        .iter()
        .position(|argument| argument == UPDATE_SCREEN_FLAG)?;
    arguments.get(flag + 1).map(String::as_str)
}

/// The update of a daemon, in the state of one of the screens: the rest of the demo stays. A name
/// the demo does not know opens nothing.
pub fn show_update_screen(state: &mut AppState, screen: &str) {
    match screen {
        "older" => this_computer_refused(state, older_gap(Some(update_plan()))),
        "no-commit" => this_computer_refused(state, older_gap(None)),
        "newer" => this_computer_refused(state, newer_gap()),
        "running" => {
            this_computer_refused(state, older_gap(Some(update_plan())));
            start_demo_update(state, MachineId::THIS);
        }
        "refused" => failed_update(state, UpdateFailure::Refused, Some(3)),
        "failed" => failed_update(state, UpdateFailure::Failed, Some(1)),
        "cancelled" => failed_update(state, UpdateFailure::Cancelled, Some(1)),
        "install-failed" => failed_update(state, UpdateFailure::InstallFailed, Some(4)),
        "rolled-back" => failed_update(state, UpdateFailure::RolledBack, Some(5)),
        "already-running" => failed_update(state, UpdateFailure::AlreadyRunning, Some(6)),
        "lost" => failed_update(state, UpdateFailure::Unknown, Some(255)),
        "sidebar" => {
            server_refused(state);
        }
        "dialog" => {
            let machine = server_refused(state);
            state.ui.dialog = Some(Dialog::DaemonUpdate(machine));
        }
        _ => {}
    }
}

fn update_plan() -> UpdatePlan {
    UpdatePlan {
        host: UPDATE_MACHINE.to_owned(),
        remote_command: "/opt/agents-cockpit/bin/agents-cockpit".to_owned(),
        source_dir: None,
        commit: UPDATE_COMMIT.to_owned(),
    }
}

fn older_gap(plan: Option<UpdatePlan>) -> VersionGap {
    VersionGap {
        kind: ProtocolGap::Older,
        details: REFUSED_FOR_OLDER.to_owned(),
        plan,
    }
}

fn newer_gap() -> VersionGap {
    VersionGap {
        kind: ProtocolGap::Newer,
        details: "the daemon speaks protocol 5, the client 4".to_owned(),
        plan: None,
    }
}

/// This computer's daemon, over SSH, refuses the app: the window shows the update screens alone.
fn this_computer_refused(state: &mut AppState, gap: VersionGap) {
    let label = DaemonLabel::ssh(UPDATE_MACHINE.to_owned());
    no_daemon::show(
        state,
        &label,
        format!("the daemon refused the connection: {}", gap.details),
        Absence::Failed,
        None,
    );
    state.machines.this.daemon = label;
    state.machines.this.version_gap = Some(gap);
}

/// A server beside this computer that refuses the app, the demo's agents staying in use.
fn server_refused(state: &mut AppState) -> MachineId {
    // After the demo's server, devbox.
    let machine = MachineId::new(2);
    let mut server = MachineState::new(DaemonLabel::ssh(UPDATE_MACHINE.to_owned()));
    server.connection = ConnectionState::Disconnected {
        error: format!("the daemon refused the connection: {REFUSED_FOR_OLDER}"),
        retry_at: Instant::now() + Duration::from_secs(10),
    };
    server.version_gap = Some(older_gap(Some(update_plan())));
    state.machines.others.insert(machine, server);
    machine
}

/// Starts the update and takes it partway through the build.
fn start_demo_update(state: &mut AppState, machine: MachineId) {
    if daemon_update::start(state, machine).is_none() {
        return;
    }
    let lines: Vec<OutputLine> = UPDATE_OUTPUT
        .iter()
        .take(4)
        .map(|text| OutputLine {
            stream: OutputStream::Stdout,
            text: (*text).to_owned(),
        })
        .collect();
    daemon_update::output(state, machine, lines);
}

/// The update ended on `code`, with what the script said on such a failure.
fn failed_update(state: &mut AppState, failure: UpdateFailure, code: Option<i32>) {
    this_computer_refused(state, older_gap(Some(update_plan())));
    let machine = MachineId::THIS;
    if daemon_update::start(state, machine).is_none() {
        return;
    }
    if failure == UpdateFailure::Cancelled {
        daemon_update::ask_cancel(state, machine);
    }
    let stderr = |text: &str| OutputLine {
        stream: OutputStream::Stderr,
        text: text.to_owned(),
    };
    let lines = match failure {
        UpdateFailure::Refused => vec![
            stderr("update-daemon: not restarting the daemon, these agents are busy:"),
            stderr("3    working   0    12    Fix the parser"),
            stderr("5    needs_you 1    4     Release notes"),
        ],
        UpdateFailure::Failed => vec![
            stderr("error[E0432]: unresolved import `crate::sampler`"),
            stderr("update-daemon: the build of 3f9c2e1 failed; the daemon was not touched"),
        ],
        UpdateFailure::Cancelled => vec![stderr(
            "update-daemon: the update was cancelled; the daemon was not touched",
        )],
        UpdateFailure::InstallFailed => vec![stderr(
            "update-daemon: the new version did not start: the daemon did not answer within 30 seconds; the previous version did not come back either: the daemon did not answer within 30 seconds",
        )],
        UpdateFailure::RolledBack => vec![stderr(
            "update-daemon: the new version did not start: the daemon did not answer within 30 seconds; the previous version runs again",
        )],
        UpdateFailure::AlreadyRunning => vec![stderr("update-daemon: another update is running")],
        UpdateFailure::Unknown => vec![stderr("Connection to build-box closed by remote host.")],
    };
    daemon_update::output(state, machine, lines);
    daemon_update::finished(state, machine, code);
}

/// Ends the demo's update as its first connection does.
pub fn finish_update(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine) {
        known.connection = ConnectionState::Connected;
    }
    daemon_update::connected(state, machine);
}

pub fn is_requested(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == DEMO_FLAG)
}

/// The sample, as a connected daemon would leave it.
pub fn state(daemon: DaemonLabel) -> AppState {
    let now = SystemTime::now();
    let mut state = AppState::new(daemon);
    state.machines.this.connection = ConnectionState::Connected;
    state.machines.this.daemon_version = Some(ReleaseVersion::new(APP_VERSION.to_owned()));
    let agents = [
        parser_agent(now),
        question_agent(now),
        approval_agent(now),
        plan_agent(now),
        benchmark_agent(now),
        exited_agent(now),
        live_agent(now),
        map_agent(now),
        sign_in_agent(now),
    ];
    for mut sample in agents {
        date_messages(&mut sample.items, now);
        let mut entry = AgentEntry::new(sample.summary);
        if entry.id() != AgentId::new(LIVE_AGENT) {
            // The running calls started a little before the window opened, so their timers show.
            let started = Instant::now()
                .checked_sub(RUNNING_CALLS_FOR)
                .unwrap_or_else(Instant::now);
            for item in &sample.items {
                entry.note_call(item, started);
            }
            entry.transcript.merge_page(HistoryPage {
                items: sample.items,
                older_items: OlderItems::None,
            });
        }
        entry.details = Loadable::Loaded(sample.details);
        state.agents.insert(entry.id(), entry);
    }
    // The bottom agent first: positions grow towards the top.
    for (step, id) in (1_u32..).zip(SIDEBAR_ORDER.into_iter().rev()) {
        if let Some(entry) = state.agents.get_mut(&AgentId::new(id)) {
            entry.summary.position = Position::new(step * POSITION_STEP);
        }
    }
    let agent_accounts = WORK_AGENTS
        .map(|agent| (agent, WORK_ACCOUNT))
        .into_iter()
        .chain([(MAP_AGENT, LAB_ACCOUNT), (SIGN_IN_AGENT, CLIENT_ACCOUNT)]);
    for (agent, account) in agent_accounts {
        if let Some(entry) = state.agents.get_mut(&AgentId::new(agent)) {
            entry.summary.account = AccountId::new(account);
        }
    }
    state.machines.this.plan_usage = plan_usage(now);
    state.machines.this.accounts = accounts();
    state.machines.this.other_plan_usage =
        BTreeMap::from([(AccountId::new(WORK_ACCOUNT), work_plan_usage(now))]);
    state.ui.selected = Some(AgentId::new(1));
    state.ui.unread.insert(AgentId::new(BENCHMARK_AGENT));
    add_server(&mut state, now);
    state
}

/// A server with three agents of its own, which show in a second panel of the sidebar.
fn add_server(state: &mut AppState, now: SystemTime) {
    let parts = |id, title, status, activity, since| SummaryParts {
        id,
        title,
        folder: SERVER_FOLDER,
        status,
        activity,
        pending: 0,
        since,
    };
    // Top first.
    let agents = [
        (
            parts(
                1,
                "Migrate the invoices",
                AgentStatus::Working,
                Some("Running the migration tests"),
                Duration::from_secs(95),
            ),
            [
                user("Move the invoices to the new schema, one table at a time."),
                text("I start with `invoice_lines`, which no other service writes to."),
            ],
        ),
        (
            parts(
                2,
                "Find the slow invoice query",
                AgentStatus::Idle,
                None,
                Duration::from_secs(12 * 60),
            ),
            [
                user("The invoice list takes 4 s to load. Find out why."),
                text(
                    "The list sorts on `issued_at`, which has no index. Adding one brings it to 40 ms.",
                ),
            ],
        ),
        (
            parts(
                3,
                "Renew the staging certificates",
                AgentStatus::Idle,
                None,
                Duration::from_secs(5 * 3600),
            ),
            [
                user("Renew the staging certificates before Friday."),
                text("Renewed. They now expire on 9 February."),
            ],
        ),
    ];
    insert_server(state, now, SERVER, SERVER_HOST, agents);
}

/// Adds a connected server with `agents`, listed top first, each with a short conversation.
fn insert_server<const N: usize>(
    state: &mut AppState,
    now: SystemTime,
    machine: MachineId,
    host: &str,
    agents: [(SummaryParts<'_>, [ItemBody; 2]); N],
) {
    let mut server = MachineState::new(DaemonLabel::ssh(host.to_owned()));
    server.connection = ConnectionState::Connected;
    server.daemon_version = Some(ReleaseVersion::new(APP_VERSION.to_owned()));
    server.plan_usage = plan_usage(now);
    server.accounts = accounts().into_iter().take(1).collect();
    state.machines.others.insert(machine, server);
    // The bottom agent first: positions grow towards the top.
    let bottom_first = (1_u32..).zip(agents.into_iter().rev());
    for (step, (parts, bodies)) in bottom_first {
        let mut summary = summary(parts, now);
        // Undo finds a closed agent by its session, which this computer's agents use too.
        summary.session = Some(server_session(machine, summary.id));
        summary.id = machine.app_agent(summary.id);
        summary.position = Position::new(step * POSITION_STEP);
        let mut items: Vec<TranscriptItem> = (1..)
            .zip(bodies)
            .map(|(id, body)| item(id, None, body))
            .collect();
        date_messages(&mut items, now);
        let mut entry = AgentEntry::new(summary);
        entry.transcript.merge_page(HistoryPage {
            items,
            older_items: OlderItems::None,
        });
        entry.details = Loadable::Loaded(details());
        state.agents.insert(entry.id(), entry);
    }
}

/// The Claude session of a server's agent, which no other machine's agent has.
fn server_session(machine: MachineId, agent: AgentId) -> ClaudeSessionId {
    ClaudeSessionId::new(format!("demo-session-{}-{}", machine.get(), agent.get()))
}

/// Whether the arguments ask for the showcase: `--demo --demo-showcase`.
pub fn showcase_requested(arguments: &[String]) -> bool {
    is_requested(arguments) && arguments.iter().any(|argument| argument == SHOWCASE_FLAG)
}

/// Trims the demo for the README's picture: two accounts, one agent waiting on this computer, no
/// emoji in titles, an approval on the server, and a third machine.
pub fn showcase(state: &mut AppState) {
    for agent in SHOWCASE_LEFT_OUT {
        state.agents.remove(&AgentId::new(agent));
    }
    let shown_account =
        |account: &Account| account.id.is_default() || account.id == AccountId::new(WORK_ACCOUNT);
    state.machines.this.accounts.retain(shown_account);
    for entry in state.agents.values_mut() {
        let title = entry
            .summary
            .title
            .trim_start_matches(|character: char| !character.is_alphanumeric());
        entry.summary.title = title.to_owned();
    }
    // It goes on top of the server's three agents.
    if let Some(mut approval) = state.agents.remove(&AgentId::new(APPROVAL_AGENT)) {
        let agent = AgentId::new(4);
        approval.summary.session = Some(server_session(SERVER, agent));
        approval.summary.id = SERVER.app_agent(agent);
        approval.summary.position = Position::new(4 * POSITION_STEP);
        state.agents.insert(approval.id(), approval);
    }
    let accounts = state.machines.this.accounts.clone();
    if let Some(server) = state.machines.others.get_mut(&SERVER) {
        server.accounts = accounts;
    }
    if let Some(entry) = state.agents.get_mut(&SERVER.app_agent(AgentId::new(1))) {
        entry.summary.account = AccountId::new(WORK_ACCOUNT);
    }
    let parts = |id, title, status, activity, since| SummaryParts {
        id,
        title,
        folder: GPU_FOLDER,
        status,
        activity,
        pending: 0,
        since,
    };
    let agents = [
        (
            parts(
                1,
                "Train the ranking model",
                AgentStatus::Working,
                Some("Running epoch 3 of 10"),
                Duration::from_secs(4 * 60),
            ),
            [
                user("Train the ranking model on last week's clicks."),
                text(
                    "Training runs on both GPUs. Epoch 2 reached 0.81 NDCG on the validation set.",
                ),
            ],
        ),
        (
            parts(
                2,
                "Profile the data loader",
                AgentStatus::Idle,
                None,
                Duration::from_secs(40 * 60),
            ),
            [
                user("Why does the data loader use only one core?"),
                text(
                    "It reads the shards one at a time. Reading four at once keeps both GPUs busy.",
                ),
            ],
        ),
    ];
    insert_server(state, SystemTime::now(), GPU_SERVER, GPU_HOST, agents);
    // Finished and unread, with nothing left running in the background.
    if let Some(entry) = state.agents.get_mut(&AgentId::new(BENCHMARK_AGENT)) {
        entry.summary.background_running = 0;
        if let Loadable::Loaded(details) = &mut entry.details {
            details.background_tasks.clear();
        }
    }
    let Some(selected) = state.ui.selected else {
        return;
    };
    if let Some(entry) = state.agents.get_mut(&selected) {
        trim_for_notes(entry);
        entry
            .tree
            .expanded
            .insert(PathBuf::from(PARSER_FOLDER).join("src"));
    }
    // Opens the pane, as the header's changes chip does.
    file_tree::toggle_from_changes(state, selected);
    file_tree::set_filter(state, selected, TreeFilter::All);
}

/// Shortens the selected agent's blocks by a task and a running subagent, so that the third
/// comment's note fits under them, and counts the task progress again.
fn trim_for_notes(entry: &mut AgentEntry) {
    let Loadable::Loaded(details) = &mut entry.details else {
        return;
    };
    details
        .background_tasks
        .retain(|task| task.description != FUZZ_TASK);
    details
        .subagents
        .retain(|subagent| subagent.description != FUZZ_TASK);
    if let Some(first_done) = details
        .tasks
        .iter()
        .position(|task| task.status == TaskStatus::Completed)
    {
        details.tasks.remove(first_done);
    }
    let done = details
        .tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    entry.summary.task_progress = Some(TaskProgress {
        done: u16::try_from(done).unwrap_or(u16::MAX),
        total: u16::try_from(details.tasks.len()).unwrap_or(u16::MAX),
    });
}

/// The demo's drafts: a reply under way to the selected agent, which quotes passages of its
/// answers with a comment beside each.
pub fn drafts() -> Drafts {
    let mut drafts = Drafts::default();
    for quote in DEMO_QUOTES {
        let Some((before, _)) = quote.answer.split_once(quote.passage) else {
            continue;
        };
        let passage = quotes::Passage {
            anchor: quotes::QuoteAnchor {
                item: ItemId::new(quote.item),
                start: Utf16Offset::end_of(before),
                end: Utf16Offset::end_of(&format!("{before}{}", quote.passage)),
            },
            raw: quote.passage.to_owned(),
            text: quote.passage.to_owned(),
            code: false,
        };
        quotes::start(&mut drafts, AgentId::new(1), passage);
        quotes::type_comment(&mut drafts, quote.comment.to_owned());
        quotes::commit(&mut drafts, CommentPlacement::Beside);
    }
    drafts
}

/// How long the demo takes to answer, as a daemon would for the past sessions, the context usage
/// and the conversation's pages, so that their loading states show.
pub fn latency(request: &Request) -> Duration {
    match request {
        Request::SessionsList => Duration::from_millis(900),
        Request::AgentContextUsage(_) => Duration::from_millis(700),
        Request::AgentHistory(_) => Duration::from_millis(300),
        // A daemon on a server: the message shows on its way until this echo.
        Request::AgentPrompt(_) => Duration::from_millis(400),
        _ => Duration::ZERO,
    }
}

/// What the daemon's events would change after a request it accepts: a rename shows in the
/// title, a mark on the row, which a prompt clears, an answered approval and a stopped task show
/// in the agent map, and the accounts change.
pub fn apply(state: &mut AppState, request: &Request) {
    apply_map_request(state, request);
    apply_account_request(state, request);
    match request {
        Request::AgentRename(params) => {
            if let Some(entry) = state.agents.get_mut(&params.agent) {
                entry.summary.title.clone_from(&params.title);
            }
        }
        Request::AgentMark(params) => {
            if let Some(entry) = state.agents.get_mut(&params.agent) {
                entry.summary.mark = params.mark;
            }
        }
        Request::AgentChangeFolder(params) => {
            if let Some(entry) = state.agents.get_mut(&params.agent) {
                entry.summary.folder.clone_from(&params.folder);
            }
        }
        Request::AgentPrompt(params) => {
            if let Some(entry) = state.agents.get_mut(&params.agent) {
                entry.summary.mark = Mark::Unmarked;
                echo_prompt(entry, &params.prompt);
            }
        }
        _ => {}
    }
}

/// The daemon's echo of a sent message: in the chat of an agent between turns, else with the
/// messages Claude has not read.
fn echo_prompt(entry: &mut AgentEntry, prompt: &Prompt) {
    let busy = matches!(
        entry.summary.status,
        AgentStatus::Working | AgentStatus::NeedsYou
    );
    if busy && let Loadable::Loaded(details) = &mut entry.details {
        let next = details
            .waiting_prompts
            .iter()
            .map(|waiting| waiting.id.get())
            .max()
            .map_or(1, |last| last.saturating_add(1));
        details.waiting_prompts.push(WaitingPrompt {
            id: PromptId::new(next),
            text: prompt.text.clone(),
            image_count: u32::try_from(prompt.images.len()).unwrap_or(u32::MAX),
            state: WaitingState::Sent,
            sent_at: Some(at(SystemTime::now(), 0)),
        });
        sending::waiting_changed(entry);
        return;
    }
    let next = entry
        .transcript
        .last_id()
        .map_or(1, |last| last.get().saturating_add(1));
    let item = TranscriptItem {
        id: ItemId::new(next),
        parent: None,
        body: ItemBody::UserMessage(UserMessage {
            text: prompt.text.clone(),
            images: prompt.images.clone(),
            sent_at: Some(at(SystemTime::now(), 0)),
        }),
    };
    sending::item_appended(entry, &item);
    entry.transcript.append(item);
}

/// What `machine`'s daemon would answer: the server's own answer where it differs from this
/// computer's.
pub fn answer_from(
    state: &AppState,
    machine: MachineId,
    request: &Request,
) -> Result<ResponseBody, RequestError> {
    if machine == MachineId::THIS {
        return answer(state, request);
    }
    match request {
        Request::SessionsList => Ok(ResponseBody::Sessions {
            sessions: Vec::new(),
        }),
        Request::FoldersList(params) => list_folders(state, machine, &params.path),
        // The server's demo agents run nothing: its load is the machine's alone.
        Request::MachineLoad => {
            let mut load = machine_load(SystemTime::now());
            load.agents.clear();
            Ok(ResponseBody::MachineLoad(load))
        }
        Request::AccountsList => Ok(ResponseBody::Accounts {
            accounts: state
                .machines
                .get(machine)
                .map(|known| known.accounts.clone())
                .unwrap_or_default()
                .into_iter()
                .map(|account| account_state(state, account))
                .collect(),
            environment_credentials: EnvironmentCredentials::Set,
        }),
        Request::AccountsAdd(_) | Request::AccountsChange(_) | Request::AccountsRemove(_) => {
            Err(refused(
                ErrorCode::InvalidRequest,
                "The demo changes the accounts of this computer only.",
            ))
        }
        Request::AgentListDir(_) | Request::AgentReadFile(_) | Request::AgentFindFiles(_) => Err(
            refused(ErrorCode::NotFound, "The demo has no files on its server."),
        ),
        _ => answer(state, request),
    }
}

/// What this computer's daemon, holding the sample, would answer.
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
        Request::AgentReadFile(params) => {
            read_file(&params.path).map(|body| with_edits(state, params.agent, body))
        }
        Request::AgentListDir(params) => list_dir(&params.path),
        Request::AgentFindFiles(params) => Ok(find_files(&params.name)),
        Request::FoldersList(params) => list_folders(state, MachineId::THIS, &params.path),
        Request::AgentFork(_) => Err(refused(
            ErrorCode::InvalidRequest,
            "The demo runs no Claude session to fork.",
        )),
        Request::AgentHistory(params) if params.agent == AgentId::new(LIVE_AGENT) => {
            let page = match params.before {
                None => HistoryPage {
                    items: live_page(),
                    older_items: OlderItems::Exist,
                },
                Some(before) => HistoryPage {
                    items: (LIVE_OLDER_FIRST_ID..before.get())
                        .map(|id| item(id, None, live_entry(id)))
                        .collect(),
                    older_items: OlderItems::None,
                },
            };
            Ok(ResponseBody::History(page))
        }
        Request::AgentHistory(_) => Ok(ResponseBody::History(HistoryPage {
            items: Vec::new(),
            older_items: OlderItems::None,
        })),
        Request::SessionsList => Ok(ResponseBody::Sessions {
            sessions: sessions(SystemTime::now()),
        }),
        Request::MachineLoad => Ok(ResponseBody::MachineLoad(machine_load(SystemTime::now()))),
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
        Request::AccountsList => Ok(ResponseBody::Accounts {
            accounts: state
                .machines
                .this
                .accounts
                .iter()
                .map(|account| account_state(state, account.clone()))
                .collect(),
            environment_credentials: EnvironmentCredentials::Set,
        }),
        Request::AccountsAdd(params) => {
            let id = state
                .machines
                .this
                .accounts
                .iter()
                .map(|account| account.id)
                .max()
                .and_then(AccountId::next)
                .unwrap_or(AccountId::DEFAULT);
            let folder = params.folder.clone().unwrap_or_else(|| {
                in_folder(
                    DEMO_HOME,
                    cockpit_protocol::account_folder_name(&params.name),
                )
            });
            let account = Account {
                id,
                name: params.name.trim().to_owned(),
                folder,
                credentials: params.credentials,
            };
            Ok(ResponseBody::Account(account_state(state, account)))
        }
        Request::AccountsChange(params) => {
            let Some(mut account) = state
                .machines
                .this
                .accounts
                .iter()
                .find(|account| account.id == params.account)
                .cloned()
            else {
                return Err(refused(
                    ErrorCode::NotFound,
                    "The demo has no such account.",
                ));
            };
            if let Some(name) = &params.name {
                account.name.clone_from(name);
            }
            if let Some(credentials) = params.credentials {
                account.credentials = credentials;
            }
            Ok(ResponseBody::Account(account_state(state, account)))
        }
        _ => Ok(ResponseBody::Done),
    }
}

/// The demo server: 64 cores and 503 GB, as a large build machine.
const DEMO_CORES: u16 = 64;
const GIGABYTE: u64 = 1 << 30;
const DEMO_MEMORY: u64 = 503 * GIGABYTE;
/// The daemon's sampling interval and history length.
const LOAD_INTERVAL_SECONDS: u64 = 3;
const LOAD_SAMPLES: u64 = 100;
/// The building agent, the testing one, and the waiting and idle ones, which a daemon measures:
/// the exited agent has no process.
const BUILDING_AGENT: u32 = 1;
const TESTING_AGENT: u32 = LIVE_AGENT;
const QUIET_AGENTS: [u32; 4] = [2, 3, 4, 5];

/// What one agent uses at sample `step`, in cores: a release build that ramps up and holds, a
/// test loop that spikes, and quiet agents.
fn demo_cores(agent: u32, step: u64) -> f64 {
    let phase = f64::from(u32::try_from(step % 10_000).unwrap_or(0));
    match agent {
        BUILDING_AGENT => 18.5 + 2.5 * (phase * 0.13).sin() + (phase * 0.71).sin(),
        TESTING_AGENT => 3.5 + 3.0 * (phase * 0.9).sin().abs(),
        3 => 0.1,
        _ => 0.0,
    }
}

/// The server's CPU at sample `step`: its agents', and 21 to 35 cores of other processes.
fn demo_server_cores(step: u64) -> f64 {
    let phase = f64::from(u32::try_from(step % 10_000).unwrap_or(0));
    let agents: f64 = [BUILDING_AGENT, TESTING_AGENT]
        .iter()
        .chain(QUIET_AGENTS.iter())
        .map(|&agent| demo_cores(agent, step))
        .sum();
    agents + 28.0 + 7.0 * (phase * 0.05).sin()
}

fn demo_memory(agent: u32) -> u64 {
    match agent {
        BUILDING_AGENT => 22 * GIGABYTE + GIGABYTE / 10,
        TESTING_AGENT => 8 * GIGABYTE + 4 * GIGABYTE / 10,
        3 => 4 * GIGABYTE / 10,
        _ => 3 * GIGABYTE / 10,
    }
}

fn demo_command(pid: u32, command: &str, cores: f64, memory_bytes: u64) -> CommandLoad {
    CommandLoad {
        pid: ProcessId::new(pid),
        command: command.to_owned(),
        cpu: rounded_cores(cores),
        memory_bytes,
    }
}

fn rounded_cores(cores: f64) -> Cores {
    Cores::new((cores * 100.0).round() / 100.0)
}

/// What the agent runs, its heaviest command first, at `cores` for the whole agent.
fn demo_heaviest(agent: u32, cores: f64) -> Vec<CommandLoad> {
    match agent {
        BUILDING_AGENT => vec![
            demo_command(
                41_872,
                "rustc --crate-name calc_parser --edition=2024 src/lib.rs --crate-type lib -C opt-level=3",
                cores * 0.45,
                6 * GIGABYTE,
            ),
            demo_command(
                41_866,
                "rustc --crate-name calc_lexer --edition=2024 src/lib.rs --crate-type lib -C opt-level=3",
                cores * 0.3,
                4 * GIGABYTE + 8 * GIGABYTE / 10,
            ),
            demo_command(41_790, "cargo build --release", 0.3, GIGABYTE / 5),
        ],
        TESTING_AGENT => vec![
            demo_command(
                52_310,
                "calc_stream-9c1d2e0f4b7a8e31 case_ --test-threads=8",
                cores * 0.8,
                3 * GIGABYTE,
            ),
            demo_command(52_288, "cargo test -p calc-stream case_", 0.2, GIGABYTE / 4),
        ],
        3 => vec![demo_command(
            33_402,
            "mdbook serve --port 3000",
            0.1,
            GIGABYTE / 10,
        )],
        _ => Vec::new(),
    }
}

/// The demo's answer to `machine.load`: the numbers move with the clock, every 3 seconds.
fn machine_load(now: SystemTime) -> MachineLoad {
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let current = seconds / LOAD_INTERVAL_SECONDS;
    let measured: Vec<u32> = [BUILDING_AGENT, TESTING_AGENT]
        .into_iter()
        .chain(QUIET_AGENTS)
        .collect();
    let agents: Vec<AgentLoad> = measured
        .iter()
        .map(|&agent| {
            let cores = demo_cores(agent, current);
            let heaviest = demo_heaviest(agent, cores);
            AgentLoad {
                agent: AgentId::new(agent),
                cpu: rounded_cores(cores),
                memory_bytes: demo_memory(agent),
                processes: u32::try_from(heaviest.len()).unwrap_or(0) + 1,
                heaviest,
            }
        })
        .collect();
    let server_cores = demo_server_cores(current);
    let agents_cores: f64 = agents.iter().map(|agent| agent.cpu.get()).sum();
    let agents_memory: u64 = agents.iter().map(|agent| agent.memory_bytes).sum();
    let memory_used = 312 * GIGABYTE;
    let steps: Vec<u64> = (0..LOAD_SAMPLES)
        .map(|back| current.saturating_sub(LOAD_SAMPLES - 1 - back))
        .collect();
    let history = LoadHistory {
        server: steps
            .iter()
            .map(|&step| ServerPoint {
                taken_at: at(now, (current - step) * LOAD_INTERVAL_SECONDS),
                cpu: rounded_cores(demo_server_cores(step)),
            })
            .collect(),
        agents: measured
            .iter()
            .map(|&agent| AgentHistory {
                agent: AgentId::new(agent),
                cpu: steps
                    .iter()
                    .map(|&step| Some(rounded_cores(demo_cores(agent, step))))
                    .collect(),
            })
            .collect(),
    };
    MachineLoad {
        taken_at: at(now, 0),
        server: ServerLoad {
            cores: DEMO_CORES,
            cpu: rounded_cores(server_cores),
            memory_used_bytes: memory_used,
            memory_total_bytes: DEMO_MEMORY,
            load_average: Some(LoadAverage {
                one_minute: (server_cores * 0.95 * 10.0).round() / 10.0,
                five_minutes: 38.2,
                fifteen_minutes: 31.4,
            }),
        },
        agents,
        others: ResourceUse {
            cpu: rounded_cores(server_cores - agents_cores),
            memory_bytes: memory_used - agents_memory,
        },
        history,
    }
}

fn refused(code: ErrorCode, message: &str) -> RequestError {
    RequestError::Failed(ErrorBody::new(code, message.to_owned()))
}

/// `path` in the made-up folder `folder`, with `/` as the demo's Linux daemon writes it, also
/// on Windows.
fn in_folder(folder: &str, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref().to_string_lossy();
    if PathStyle::Unix.is_absolute(&path) {
        PathBuf::from(path.as_ref())
    } else {
        PathBuf::from(PathStyle::Unix.join(folder, &path))
    }
}

/// The sample's files, absolute, with their text.
fn files() -> [(PathBuf, &'static str); 17] {
    [
        (in_folder(PARSER_FOLDER, "Cargo.toml"), CARGO_TEXT),
        (in_folder(PARSER_FOLDER, "README.md"), README_TEXT),
        (in_folder(PARSER_FOLDER, ".gitignore"), "/target\n"),
        (
            in_folder(PARSER_FOLDER, IGNORED_FILE),
            "A build output, which the .gitignore leaves out.\n",
        ),
        (in_folder(PARSER_FOLDER, "src/main.rs"), MAIN_TEXT),
        (in_folder(PARSER_FOLDER, "src/parser.rs"), PARSER_TEXT),
        (in_folder(PARSER_FOLDER, "src/token.rs"), TOKEN_KIND_TEXT),
        (in_folder(PARSER_FOLDER, "benches/eval.rs"), BENCH_TEXT),
        (in_folder(PARSER_FOLDER, ERROR_FILE), ERROR_AFTER),
        (in_folder(PARSER_FOLDER, TEST_FILE), TEST_TEXT),
        (in_folder(PARSER_FOLDER, LEXER_FILE), LEXER_TEXT),
        (in_folder(PARSER_FOLDER, NOTES_FILE), NOTES_TEXT),
        (in_folder(PARSER_FOLDER, LIB_FILE), LIB_TEXT),
        (in_folder(PARSER_COLUMNS_WORKTREE, TOKEN_FILE), TOKEN_TEXT),
        (
            in_folder(PARSER_COLUMNS_WORKTREE, POSITION_FILE),
            POSITION_TEXT,
        ),
        (
            in_folder(PARSER_COLUMNS_WORKTREE, WORKTREE_LIB_FILE),
            WORKTREE_LIB_TEXT,
        ),
        (in_folder(PARSER_BENCH_WORKTREE, "README.md"), NOTES_TEXT),
    ]
}

/// The folders the sample's agents reach, as the daemon allows them: the parser's folder and the
/// worktrees beside it.
const REACHABLE: [&str; 3] = [
    PARSER_FOLDER,
    PARSER_COLUMNS_WORKTREE,
    PARSER_BENCH_WORKTREE,
];

/// As the daemon lists a folder for the parser agent: the sample's files under it, and the folders
/// that hold them.
fn list_dir(path: &Path) -> Result<ResponseBody, RequestError> {
    let folder = in_folder(PARSER_FOLDER, path);
    if !REACHABLE.iter().any(|root| folder.starts_with(root)) {
        return Err(refused(
            ErrorCode::InvalidRequest,
            "the folder is outside the agent's folder and the checkouts it worked in",
        ));
    }
    let mut entries: Vec<DirEntry> = Vec::new();
    for (file, text) in files() {
        let Ok(rest) = file.strip_prefix(&folder) else {
            continue;
        };
        let mut names = rest.iter();
        let Some(name) = names.next().map(|name| name.to_string_lossy().into_owned()) else {
            continue;
        };
        let inside = names.next().is_some();
        let name_is_ignored = name == IGNORED_FOLDER;
        if entries.iter().any(|entry| entry.name == name) {
            continue;
        }
        entries.push(DirEntry {
            name,
            kind: if inside {
                EntryKind::Folder
            } else {
                EntryKind::File
            },
            size: (!inside).then(|| u64::try_from(text.len()).unwrap_or(0)),
            link: EntryLink::Direct,
            ignore: if name_is_ignored {
                GitIgnore::Ignored
            } else {
                GitIgnore::Kept
            },
        });
    }
    if entries.is_empty() && !REACHABLE.iter().any(|root| folder == Path::new(root)) {
        return Err(refused(
            ErrorCode::NotFound,
            &format!("{} does not exist", folder.display()),
        ));
    }
    entries.sort_by_key(|entry| (entry.kind != EntryKind::Folder, entry.name.to_lowercase()));
    Ok(ResponseBody::Directory(DirListing {
        path: folder,
        entries,
        completeness: Completeness::Complete,
    }))
}

/// The folder the demo project's .gitignore leaves out, and a file in it.
const IGNORED_FOLDER: &str = "target";
const IGNORED_FILE: &str = "target/debug/calc-parser.d";

/// Folders of the demo server that hold no agent, past session or sample file.
const OTHER_FOLDERS: [&str; 5] = [
    "/home/dev/.config/agents-cockpit",
    "/home/dev/.cache",
    "/home/dev/notes",
    "/home/dev/projects/archive/2025-prototypes",
    "/tmp/scratch",
];

/// As the daemon lists the folders in `path` for the new agent dialog: the folders of the sample's
/// agents, past sessions and files, the folders above them, and a few others.
/// The folders under `path` on `machine`: its home and its agents' folders, and on this computer
/// the past sessions' folders and the sample's.
fn list_folders(
    state: &AppState,
    machine: MachineId,
    path: &Path,
) -> Result<ResponseBody, RequestError> {
    if !PathStyle::Unix.is_absolute(&path.to_string_lossy()) {
        return Err(refused(
            ErrorCode::InvalidRequest,
            &format!("the folder {} is not absolute", path.display()),
        ));
    }
    let mut folders: Vec<PathBuf> = state
        .agents
        .values()
        .filter(|entry| MachineId::of(entry.id()) == machine)
        .map(|entry| entry.summary.folder.clone())
        .chain([PathBuf::from(DEMO_HOME)])
        .collect();
    if machine == MachineId::THIS {
        let past = sessions(SystemTime::now())
            .into_iter()
            .map(|session| session.folder);
        let holding = files()
            .into_iter()
            .filter_map(|(file, _)| file.parent().map(Path::to_owned));
        let others = OTHER_FOLDERS.iter().map(PathBuf::from);
        folders.extend(past.chain(holding).chain(others));
    }
    let mut exists = false;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for folder in folders {
        for above in folder.ancestors() {
            exists |= above == path;
            if above.parent() == Some(path)
                && let Some(name) = above.file_name()
            {
                names.insert(name.to_string_lossy().into_owned());
            }
        }
    }
    if !exists {
        return Err(refused(
            ErrorCode::NotFound,
            &format!("{} does not exist", path.display()),
        ));
    }
    let mut entries: Vec<DirEntry> = names
        .into_iter()
        .map(|name| DirEntry {
            name,
            kind: EntryKind::Folder,
            size: None,
            link: EntryLink::Direct,
            ignore: GitIgnore::Kept,
        })
        .collect();
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(ResponseBody::Directory(DirListing {
        path: path.to_owned(),
        entries,
        completeness: Completeness::Complete,
    }))
}

/// As the daemon finds files by name: the sample's files whose name holds `name`.
fn find_files(name: &str) -> ResponseBody {
    let needle = name.to_lowercase();
    let matches = files()
        .into_iter()
        .map(|(file, _)| file)
        .filter(|file| {
            file.file_name()
                .is_some_and(|file| file.to_string_lossy().to_lowercase().contains(&needle))
        })
        .collect();
    ResponseBody::FileMatches { matches }
}

/// A file's answer with the edits `agent` made to it, as the daemon gives them: the demo's
/// transcript holds them all.
fn with_edits(state: &AppState, agent: AgentId, body: ResponseBody) -> ResponseBody {
    let (
        ResponseBody::File {
            path: Some(found),
            text,
            ..
        },
        Some(entry),
    ) = (&body, state.agents.get(&agent))
    else {
        return body;
    };
    let folder = &entry.summary.folder;
    let relative = file_edits::relative_path(folder, found);
    let edits = file_edits::edits_for(&entry.transcript, folder, &relative);
    ResponseBody::File {
        path: Some(found.clone()),
        text: text.clone(),
        edits: Some(edits.into_iter().cloned().collect()),
    }
}

/// As the daemon answers for the parser agent: the path itself, else the sample's files whose
/// path ends with it, one read or several listed. Any PNG is the app's icon.
fn read_file(path: &Path) -> Result<ResponseBody, RequestError> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;

    if path.extension().is_some_and(|extension| extension == "png") {
        return Ok(ResponseBody::Image {
            path: path.to_owned(),
            image: Image {
                media_type: ImageMediaType::Png,
                data: BASE64.encode(crate::ICON_PNG),
            },
        });
    }
    let files = files();
    let wanted = in_folder(PARSER_FOLDER, path);
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
            edits: Some(Vec::new()),
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
    /// How long the turn has run for a working agent, else how long ago the agent last did
    /// something.
    since: Duration,
}

fn summary(parts: SummaryParts<'_>, now: SystemTime) -> AgentSummary {
    let since = parts.since.as_secs();
    let (last_activity, turn_started) = match parts.status {
        AgentStatus::Working => (since, Some(since)),
        AgentStatus::NeedsYou => (since, Some(since + 3 * 60)),
        AgentStatus::Idle | AgentStatus::Exited => (since, None),
    };
    AgentSummary {
        id: AgentId::new(parts.id),
        title: parts.title.to_owned(),
        folder: PathBuf::from(parts.folder),
        status: parts.status,
        activity: parts.activity.map(str::to_owned),
        pending_count: parts.pending,
        model: Some("claude-opus-5-5".to_owned()),
        effort: Some(Effort::High),
        effort_source: EffortSource::Chosen,
        mode: PermissionMode::Default,
        task_progress: None,
        context_percent: Some(31),
        cache_expires_at: Some(in_future(now, 6 * 60)),
        turns: 9,
        cost: CostUsd::new(1.84),
        started_at: at(now, since + 3600),
        last_activity_at: Some(at(now, last_activity)),
        turn_started_at: turn_started.map(|seconds| at(now, seconds)),
        session: Some(ClaudeSessionId::new(format!("demo-session-{}", parts.id))),
        position: Position::default(),
        account: AccountId::DEFAULT,
        sign_in_wait: SignInWait::NotWaiting,
        background_running: 0,
        mark: Mark::Unmarked,
        repository: demo_repository(parts.folder),
    }
}

/// A made-up remote for a demo folder, named after the folder without a worktree's suffix.
fn demo_repository(folder: &str) -> Option<String> {
    let name = folder.rsplit('/').next()?;
    let name = name.split('_').next().unwrap_or(name);
    (!name.is_empty()).then(|| format!("example.com/acme/{name}"))
}

fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
    TranscriptItem {
        id: ItemId::new(id),
        parent: parent.map(ItemId::new),
        body,
    }
}

/// The demo's messages were sent minutes apart, the last one 3 minutes ago.
fn date_messages(items: &mut [TranscriptItem], now: SystemTime) {
    let messages = items
        .iter_mut()
        .rev()
        .filter_map(|item| match &mut item.body {
            ItemBody::UserMessage(message) => Some(message),
            _ => None,
        });
    for (message, minutes_ago) in messages.zip((3..).step_by(7)) {
        message.sent_at = Some(at(now, minutes_ago * 60));
    }
}

fn user(text: &str) -> ItemBody {
    ItemBody::UserMessage(UserMessage {
        text: text.to_owned(),
        images: Vec::new(),
        sent_at: None,
    })
}

/// A message with a PNG screenshot attached.
fn user_with_image(text: &str, png: &[u8]) -> ItemBody {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;

    ItemBody::UserMessage(UserMessage {
        text: text.to_owned(),
        images: vec![Image {
            media_type: ImageMediaType::Png,
            data: BASE64.encode(png),
        }],
        sent_at: None,
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
        ..call(
            "Read",
            &shown,
            ToolCallStatus::Done,
            Some(&read_output(1, 28)),
        )
    }
}

/// What Claude Code's `Read` returns for `count` lines from `first`: each line after its number.
fn read_output(first: u32, count: u32) -> String {
    (first..first + count)
        .map(|line| format!("{line:>6}\u{2192}    let line_{line} = {line};\n"))
        .collect()
}

/// A benchmark's output, whose warnings differ only in the field they name.
const BENCH_OUTPUT: &str = "   Compiling calc-service v0.3.0 (/home/dev/projects/calc-service)
warning: field `retries` is never read
warning: field `timeout` is never read
warning: field `backoff` is never read
warning: field `jitter` is never read
    Finished `bench` profile [optimized] target(s) in 41.20s
     Running benches/eval.rs
eval/shallow        time: [412.1 \u{b5}s 415.3 \u{b5}s 418.9 \u{b5}s]
eval/wide_sum       time: [1.02 ms 1.04 ms 1.05 ms]
eval/deep_nesting   time: [2.38 ms 2.41 ms 2.44 ms]";

fn edit_call(tool: &str, path: &str, old_text: &str, new_text: &str) -> ItemBody {
    ItemBody::ToolCall(ToolCall {
        edit: Some(FileEdit {
            path: in_folder(PARSER_FOLDER, path),
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
        (Agent, FUZZ_TASK, Running, 600, None, 8),
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
            since: TURN_ELAPSED,
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
    let position_read = read_call(&in_folder(PARSER_COLUMNS_WORKTREE, POSITION_FILE));
    let items = vec![
        item(
            1,
            None,
            user(
                "Make the parser report the column of an unexpected token:\n\n- count columns from 1, as editors do\n- add a test for it in `tests/columns.rs`",
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
            user_with_image(
                "Fix the count in the lexer, not in the parser. The test says:",
                FAILING_TEST_PNG,
            ),
        ),
        item(COUNT_ITEM, None, text(COUNT_ANSWER)),
        item(
            16,
            None,
            edit_call("Edit", LEXER_FILE, LEXER_LINE_BEFORE, LEXER_LINE_AFTER),
        ),
        item(17, None, text(FIX_ANSWER)),
        item(
            18,
            None,
            user("Is the column math the same in the columns worktree?"),
        ),
        item(19, None, ItemBody::ToolCall(position_read)),
        item(LOOKUP_ITEM, None, text(LOOKUP_ANSWER)),
        item(
            21,
            None,
            ItemBody::ToolCall(bash(
                "Run the parser tests again",
                "cargo test parser",
                ToolCallStatus::Running,
                None,
            )),
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
        subagents: parser_subagents(now),
        waiting_prompts: vec![
            WaitingPrompt {
                id: PromptId::new(1),
                text: "Also cover a token at the end of the input.".to_owned(),
                image_count: 0,
                state: WaitingState::Sent,
                sent_at: Some(at(now, 120)),
            },
            WaitingPrompt {
                id: PromptId::new(2),
                text:
                    "Then add a line about the column fix to the changelog, under the next version."
                        .to_owned(),
                image_count: 1,
                state: WaitingState::Queued,
                sent_at: Some(at(now, 60)),
            },
        ],
        changed_files: vec![
            ChangedFile {
                path: in_folder(PARSER_FOLDER, ERROR_FILE),
                lines_added: 2,
                lines_removed: 0,
            },
            ChangedFile {
                path: in_folder(PARSER_FOLDER, TEST_FILE),
                lines_added: 8,
                lines_removed: 0,
            },
            ChangedFile {
                path: in_folder(PARSER_FOLDER, LEXER_FILE),
                lines_added: 1,
                lines_removed: 1,
            },
            ChangedFile {
                path: in_folder(
                    PARSER_COLUMNS_WORKTREE,
                    "crates/calc-lexer/src/tokens/unexpected_token.rs",
                ),
                lines_added: 31,
                lines_removed: 4,
            },
            ChangedFile {
                path: in_folder(
                    PARSER_COLUMNS_WORKTREE,
                    "crates/calc-lexer/tests/columns_of_unexpected_tokens.rs",
                ),
                lines_added: 24,
                lines_removed: 0,
            },
            ChangedFile {
                path: in_folder(PARSER_BENCH_WORKTREE, "benches/lexer_on_large_inputs.rs"),
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
            title: "📝 Release notes",
            folder: PARSER_FOLDER,
            status: AgentStatus::NeedsYou,
            activity: Some("Asks which changes to include"),
            pending: 1,
            since: Duration::from_secs(40),
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
            user("Draft the release notes for the next version."),
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
                    selection: Selection::Single,
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
            id: APPROVAL_AGENT,
            title: "Docs preview",
            folder: "/home/dev/projects/docs-site",
            status: AgentStatus::NeedsYou,
            activity: Some("Waits for approval: git push"),
            pending: 1,
            since: Duration::from_secs(12),
        },
        now,
    );
    let items = vec![
        item(
            1,
            None,
            user_with_image(
                "Fix the broken links and push a preview. Here is what I see:",
                crate::ICON_PNG,
            ),
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
            id: PLAN_AGENT,
            title: "🧭 Cache layer plan",
            folder: SERVICE_FOLDER,
            status: AgentStatus::NeedsYou,
            activity: Some("Waits for plan approval"),
            pending: 1,
            since: Duration::from_secs(2 * 60 + 30),
        },
        now,
    );
    summary.mode = PermissionMode::Plan;
    let items = vec![
        item(1, None, user("Plan a cache for repeated expressions.")),
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
            id: BENCHMARK_AGENT,
            title: "⏱\u{fe0f} Benchmark in CI",
            folder: SERVICE_FOLDER,
            status: AgentStatus::Idle,
            activity: None,
            pending: 0,
            since: Duration::from_secs(25 * 60),
        },
        now,
    );
    summary.background_running = 1;
    summary.task_progress = Some(TaskProgress { done: 4, total: 4 });
    summary.model = Some("claude-sonnet-5-5".to_owned());
    summary.mode = PermissionMode::AcceptEdits;
    let items = vec![
        item(
            1,
            None,
            user("Run the benchmark in CI and tell me how to compare two runs."),
        ),
        item(
            2,
            None,
            ItemBody::ToolCall(ToolCall {
                input: serde_json::json!({"skill": "bench-compare", "args": "main against the branch"}),
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
                Some(BENCH_OUTPUT),
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
                outcome: TurnOutcome::Completed,
                error: None,
            }),
        ),
    ];
    let details = AgentDetails {
        background_tasks: vec![BackgroundTask {
            id: BackgroundTaskId::new("demo-task-3".to_owned()),
            kind: BackgroundTaskKind::Command,
            description: "cargo bench --bench eval -- --save-baseline before".to_owned(),
            status: BackgroundTaskStatus::Running,
            started_at: at(now, 900),
            ended_at: None,
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
            title: "🏗\u{fe0f} Schema migration",
            folder: "/home/dev/projects/legacy-db",
            status: AgentStatus::Exited,
            activity: None,
            pending: 0,
            since: Duration::from_secs(3 * 24 * 3600 + 5 * 3600),
        },
        now,
    );
    summary.cache_expires_at = Some(at(now, 600));
    summary.mark = Mark::ToHandle;
    let items = vec![
        item(1, None, user(MIGRATION_REQUEST)),
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

/// An agent of the client account, whose first message found no login.
fn sign_in_agent(now: SystemTime) -> Sample {
    let mut summary = summary(
        SummaryParts {
            id: SIGN_IN_AGENT,
            title: "Invoice export",
            folder: SIGN_IN_FOLDER,
            status: AgentStatus::NeedsYou,
            activity: None,
            pending: 0,
            since: Duration::from_secs(60),
        },
        now,
    );
    summary.sign_in_wait = SignInWait::Waiting;
    summary.turn_started_at = None;
    summary.turns = 1;
    summary.cost = CostUsd::new(0.0);
    let items = vec![
        item(
            1,
            None,
            user("Add a CSV export to the invoices page, with the same columns as the table. Keep the totals row out of it."),
        ),
        item(
            2,
            None,
            ItemBody::Notice {
                text: "The account Client is not signed in, or its login expired. Run this command in a terminal on the daemon's machine to sign in, then send your message again: CLAUDE_CONFIG_DIR='/home/dev/.claude-client' claude auth login".to_owned(),
                detail: None,
            },
        ),
        item(
            3,
            None,
            ItemBody::TurnEnd(TurnEnd {
                duration: Duration::from_millis(900),
                cost: CostUsd::new(0.0),
                usage: TokenUsage::default(),
                outcome: TurnOutcome::NotSignedIn,
                error: Some("The account is not signed in".to_owned()),
            }),
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
            since: TURN_ELAPSED,
        },
        now,
    );
    Sample {
        summary,
        items: Vec::new(),
        details: details(),
    }
}

/// The live agent's latest page, which `agent.history` answers.
fn live_page() -> Vec<TranscriptItem> {
    let first = item(
        LIVE_PAGE_FIRST_ID,
        None,
        user("Make the parser read its input as a stream, and run every case test."),
    );
    let page =
        (LIVE_PAGE_FIRST_ID + 1..=LIVE_PAGE_LAST_ID).map(|id| item(id, None, live_entry(id)));
    std::iter::once(first).chain(page).collect()
}

/// The kinds of the live agent's entries, as `live_entry` makes them.
const LIVE_TEXT: u32 = 0;
const LIVE_READ: u32 = 2;

/// How many kinds of entries `live_entry_of` makes.
const LIVE_KINDS: u32 = 6;

/// A made-up entry of the live agent, picked by its id.
fn live_entry(id: u32) -> ItemBody {
    live_entry_of(id % LIVE_KINDS, id)
}

/// An entry of the live agent of the `kind` (0 to 5) about the case `id`.
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
            Some(&read_output(1, 24)),
        )),
        4 => text(&format!(
            "Case {id} needed one change: the reader kept a partial token across two chunks.\n\n- The chunk boundary falls inside a number.\n- The column comes from the token's start, not the chunk's.\n\n```rust\nfn case_{id}() {{\n    let mut stream = Stream::new(Chunks::of(\"12 + 3\", 2));\n    assert_eq!(stream.next(), Some(Token::number(12, 1)));\n    assert_eq!(stream.next(), Some(Token::plus(4)));\n}}\n```"
        )),
        5 => ItemBody::ToolCall(ToolCall {
            edit: Some(FileEdit {
                path: in_folder(STREAM_FOLDER, format!("src/stream/case_{id}.rs")),
                old_text: format!("    let input = \"case {id}\";\n    let chunks = 1;\n"),
                new_text: format!(
                    "    let input = \"case {id}\";\n    let chunks = 4;\n    let keep_partial = true;\n"
                ),
            }),
            ..call(
                "Edit",
                &format!("src/stream/case_{id}.rs"),
                ToolCallStatus::Done,
                None,
            )
        }),
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

/// The agent of the agent map's sample, as the mockup draws it: subagents in the foreground and in
/// the background, nested two deep, one failed, one stopped, one waiting for you, and four of
/// earlier turns.
const MAP_AGENT: u32 = 8;
const MAP_FOLDER: &str = "/home/dev/projects/parcel-api";
/// The approval a subagent of the map's agent waits for.
const MAP_APPROVAL: &str = "demo-map-approval";

/// A subagent of the map's sample, before its times are set from now.
struct DemoSubagent {
    item: u32,
    parent: Option<u32>,
    description: &'static str,
    agent_type: &'static str,
    run: SubagentRun,
    status: SubagentStatus,
    /// Started and ended so many seconds ago.
    started: u64,
    ended: Option<u64>,
    tokens: u64,
    tool_calls: u32,
    turn: u32,
}

impl DemoSubagent {
    fn subagent(&self, now: SystemTime) -> Subagent {
        Subagent {
            item: ItemId::new(self.item),
            parent: self.parent.map(ItemId::new),
            description: self.description.to_owned(),
            agent_type: Some(self.agent_type.to_owned()),
            run: self.run,
            status: self.status,
            started_at: at(now, self.started),
            ended_at: self.ended.map(|ended| at(now, ended)),
            tokens: Some(self.tokens),
            tool_calls: self.tool_calls,
            last_tool: None,
            error: None,
            turn: self.turn,
            task: None,
        }
    }
}

/// A subagent's call, as its item shows it.
fn agent_call(
    description: &str,
    agent_type: &str,
    prompt: &str,
    status: ToolCallStatus,
    output: Option<&str>,
) -> ItemBody {
    ItemBody::Subagent(ToolCall {
        input: serde_json::json!({
            "description": description,
            "prompt": prompt,
            "subagent_type": agent_type,
        }),
        ..call("Agent", description, status, output)
    })
}

fn map_subagents(now: SystemTime) -> Vec<Subagent> {
    use SubagentRun::{Background, Foreground};
    use SubagentStatus::{Done, Failed, NeedsYou, Running, Stopped};
    let demo = |item,
                parent,
                description,
                agent_type,
                run,
                status,
                started,
                ended,
                tokens,
                tool_calls,
                turn| {
        DemoSubagent {
            item,
            parent,
            description,
            agent_type,
            run,
            status,
            started,
            ended,
            tokens,
            tool_calls,
            turn,
        }
    };
    #[rustfmt::skip]
    let sample = [
        demo(10, None, "Survey the upload handlers", "Explore", Foreground, Done, 3900, Some(3820), 21_300, 9, 1),
        demo(20, None, "List the config options", "Explore", Foreground, Done, 3880, Some(3850), 6_200, 3, 1),
        demo(30, None, "Find the rate limit crates in use", "Explore", Foreground, Done, 2400, Some(2340), 8_800, 4, 2),
        demo(40, None, "Check the existing upload tests", "general-purpose", Foreground, Done, 2390, Some(2280), 15_100, 6, 2),
        demo(103, None, "Map the upload request path", "Explore", Foreground, Done, 460, Some(388), 18_400, 4, 3),
        demo(105, None, "Benchmark uploads before the change", "general-purpose", Background, Stopped, 380, Some(198), 40_100, 2, 3),
        demo(106, None, "Implement the token bucket", "general-purpose", Background, Running, 368, None, 54_100, 5, 3),
        demo(151, Some(106), "Find existing clock abstractions", "Explore", Foreground, Done, 340, Some(302), 9_100, 3, 3),
        demo(154, Some(106), "Write property tests for the bucket", "general-purpose", Foreground, Running, 163, None, 22_300, 7, 3),
        demo(171, Some(154), "Check the proptest version in the lockfile", "Explore", Foreground, Done, 158, Some(146), 3_400, 2, 3),
        demo(107, None, "Review the auth middleware change", "code-reviewer", Foreground, Failed, 364, Some(204), 27_900, 3, 3),
        demo(108, None, "Check the upload limits in the deploy config", "Explore", Foreground, NeedsYou, 120, None, 4_700, 2, 3),
    ];
    let mut subagents: Vec<Subagent> = sample.iter().map(|demo| demo.subagent(now)).collect();
    let last_tool = |item: u32, tool: &str, summary: &str| SubagentToolCall {
        item: Some(ItemId::new(item)),
        tool: tool.to_owned(),
        summary: summary.to_owned(),
    };
    for subagent in &mut subagents {
        match subagent.item.get() {
            105 => subagent.task = Some(BackgroundTaskId::new("demo-map-task-1".to_owned())),
            106 => {
                subagent.task = Some(BackgroundTaskId::new("demo-map-task-2".to_owned()));
                subagent.last_tool = Some(last_tool(
                    154,
                    "Agent",
                    "Write property tests for the bucket",
                ));
            }
            154 => subagent.last_tool = Some(last_tool(178, "Edit", "tests/bucket_props.rs")),
            107 => subagent.error = Some("API error 529: overloaded".to_owned()),
            108 => {
                subagent.last_tool = Some(last_tool(
                    196,
                    "Bash",
                    "kubectl get configmap uploads -o yaml",
                ));
            }
            _ => {}
        }
    }
    subagents
}

const BUCKET_TEST_OUTPUT: &str = "running 6 tests
test never_exceeds_capacity ... FAILED
minimal failing input: refill = 0, takes = [1, 1]
assertion failed: taken <= capacity (left: 21, right: 20)

test result: FAILED. 5 passed; 1 failed";

fn map_agent(now: SystemTime) -> Sample {
    let mut summary = summary(
        SummaryParts {
            id: MAP_AGENT,
            title: "Rate limits for uploads",
            folder: MAP_FOLDER,
            status: AgentStatus::NeedsYou,
            activity: Some("Waits for approval: Bash"),
            pending: 1,
            since: Duration::from_secs(25),
        },
        now,
    );
    summary.turns = 2;
    summary.task_progress = Some(TaskProgress { done: 2, total: 5 });
    let done = ToolCallStatus::Done;
    let read_text = read_output(40, 64);
    let read =
        |path: &str| ItemBody::ToolCall(call("Read", path, ToolCallStatus::Done, Some(&read_text)));
    let grep = |pattern: &str, output: &str| {
        ItemBody::ToolCall(call("Grep", pattern, ToolCallStatus::Done, Some(output)))
    };
    let failed_test = ToolCall {
        exit_code: Some(101),
        ..bash(
            "Run the bucket's property tests",
            "cargo test -q -p parcel-limit --test bucket_props",
            ToolCallStatus::Failed,
            Some(BUCKET_TEST_OUTPUT),
        )
    };
    let edit =
        |path: &str, status: ToolCallStatus| ItemBody::ToolCall(call("Edit", path, status, None));
    let items = vec![
        item(
            101,
            None,
            user(
                "Add per-client rate limits to the upload endpoint. Use a token bucket, make the limits configurable, add tests, and benchmark before and after.",
            ),
        ),
        item(
            102,
            None,
            text("I'll map the request path first, then build the limiter."),
        ),
        item(
            103,
            None,
            agent_call(
                "Map the upload request path",
                "Explore",
                "Trace an upload request from the router to the storage write. Name each function it passes through, and say where the client id is first known.",
                done,
                Some(
                    "Uploads go through auth, then the size check, then storage. The client id is known right after auth, in `AuthLayer::call`.",
                ),
            ),
        ),
        item(131, Some(103), read("src/routes/upload.rs")),
        item(132, Some(103), grep("fn upload", "src/routes/upload.rs:14")),
        item(133, Some(103), read("src/auth/mod.rs")),
        item(134, Some(103), read("src/middleware.rs")),
        item(
            104,
            None,
            text(
                "Auth already knows the client id, so the limiter can sit right after it. I'll run the baseline benchmark and the implementation in the background, and get a review of the auth change meanwhile.",
            ),
        ),
        item(
            105,
            None,
            agent_call(
                "Benchmark uploads before the change",
                "general-purpose",
                "Run the upload benchmarks on `main` and save the results to `target/bench-baseline.json`.",
                done,
                Some("Started in the background."),
            ),
        ),
        item(141, Some(105), read("benches/uploads.rs")),
        item(
            142,
            Some(105),
            ItemBody::ToolCall(bash(
                "Run the upload benchmarks",
                "cargo bench --bench uploads -- --save-baseline main",
                ToolCallStatus::Failed,
                Some("Stopped."),
            )),
        ),
        item(
            106,
            None,
            agent_call(
                "Implement the token bucket",
                "general-purpose",
                "Implement a per-client token bucket in `src/limit/bucket.rs`, with the capacity and the refill rate read from the config. Use the clock abstraction the code already has. Write property tests, run them, and fix what fails.",
                done,
                Some("Started in the background."),
            ),
        ),
        item(150, Some(106), read("src/limit/mod.rs")),
        item(
            151,
            Some(106),
            agent_call(
                "Find existing clock abstractions",
                "Explore",
                "Find how the code reads the time, and whether tests can fake it.",
                done,
                Some("There is a `Clock` trait in `src/clock.rs`, with a `FakeClock` for tests."),
            ),
        ),
        item(160, Some(151), grep("Instant::now", "src/clock.rs:8")),
        item(161, Some(151), read("src/clock.rs")),
        item(162, Some(151), read("src/limit/mod.rs")),
        item(
            152,
            Some(106),
            ItemBody::ToolCall(call("Write", "src/limit/bucket.rs", done, None)),
        ),
        item(
            153,
            Some(106),
            ItemBody::ToolCall(bash(
                "Check that it builds",
                "cargo check -q -p parcel-limit",
                done,
                Some(""),
            )),
        ),
        item(
            154,
            Some(106),
            agent_call(
                "Write property tests for the bucket",
                "general-purpose",
                "Write property tests for `Bucket` in `src/limit/bucket.rs` with proptest. Cover: a client never takes more than the capacity in a burst; after waiting `n / refill` seconds, `n` more takes succeed; clients do not share tokens. Use `FakeClock`. Run the tests and fix the bucket if a property fails.",
                ToolCallStatus::Running,
                None,
            ),
        ),
        item(
            170,
            Some(154),
            text("I'll check which proptest version the lockfile pins before writing the tests."),
        ),
        item(
            171,
            Some(154),
            agent_call(
                "Check the proptest version in the lockfile",
                "Explore",
                "Which proptest version does Cargo.lock pin?",
                done,
                Some("proptest 1.5.0"),
            ),
        ),
        item(180, Some(171), read("Cargo.lock")),
        item(181, Some(171), grep("proptest", "Cargo.lock:1204")),
        item(172, Some(154), read("src/limit/bucket.rs")),
        item(
            173,
            Some(154),
            ItemBody::ToolCall(call("Write", "tests/bucket_props.rs", done, None)),
        ),
        item(174, Some(154), ItemBody::ToolCall(failed_test)),
        item(
            175,
            Some(154),
            text(
                "`try_take` checks the count before it subtracts, so one take passes over the capacity. I'll fix the comparison and rerun.",
            ),
        ),
        item(176, Some(154), edit("src/limit/bucket.rs", done)),
        item(
            177,
            Some(154),
            ItemBody::ToolCall(bash(
                "Run the bucket's property tests",
                "cargo test -q -p parcel-limit --test bucket_props",
                done,
                Some("test result: ok. 6 passed; 0 failed"),
            )),
        ),
        item(
            178,
            Some(154),
            edit("tests/bucket_props.rs", ToolCallStatus::Running),
        ),
        item(
            107,
            None,
            agent_call(
                "Review the auth middleware change",
                "code-reviewer",
                "Review the change to `src/auth/middleware.rs` on this branch for correctness and for requests that skip auth.",
                ToolCallStatus::Failed,
                Some("API error 529: overloaded"),
            ),
        ),
        item(190, Some(107), read("src/auth/middleware.rs")),
        item(
            191,
            Some(107),
            ItemBody::ToolCall(bash(
                "Show the change",
                "git diff main -- src/auth",
                done,
                Some("+    let client = ClientId::from_token(&token)?;"),
            )),
        ),
        item(192, Some(107), read("src/auth/token.rs")),
        item(
            108,
            None,
            agent_call(
                "Check the upload limits in the deploy config",
                "Explore",
                "Read the upload limits the deployment sets today, so the new defaults match them.",
                ToolCallStatus::Running,
                None,
            ),
        ),
        item(195, Some(108), read("deploy/uploads.toml")),
        item(
            196,
            Some(108),
            ItemBody::ToolCall(bash(
                "Read the live upload config",
                "kubectl get configmap uploads -o yaml",
                ToolCallStatus::Running,
                None,
            )),
        ),
        item(
            197,
            Some(108),
            ItemBody::Approval(Approval {
                pending: PendingId::new(MAP_APPROVAL.to_owned()),
                tool: "Bash".to_owned(),
                summary: "kubectl get configmap uploads -o yaml".to_owned(),
                input: serde_json::json!({"command": "kubectl get configmap uploads -o yaml", "description": "Read the live upload config"}),
                edit: None,
                reason: Some("The command reads the cluster's configuration".to_owned()),
                answer: AnswerState::Waiting,
                tool_call: Some(ItemId::new(196)),
            }),
        ),
        item(
            109,
            None,
            text("The review stopped on an API error. I'll retry it once the bucket lands."),
        ),
    ];
    let background =
        |id: &str, kind, description: &str, status, started, ended: Option<u64>, tool_calls| {
            BackgroundTask {
                id: BackgroundTaskId::new(id.to_owned()),
                kind,
                description: description.to_owned(),
                status,
                started_at: at(now, started),
                ended_at: ended.map(|ended| at(now, ended)),
                tool_calls,
            }
        };
    let details = AgentDetails {
        background_tasks: vec![
            background(
                "demo-map-task-1",
                BackgroundTaskKind::Agent,
                "Benchmark uploads before the change",
                BackgroundTaskStatus::Stopped,
                380,
                Some(198),
                2,
            ),
            background(
                "demo-map-task-2",
                BackgroundTaskKind::Agent,
                "Implement the token bucket",
                BackgroundTaskStatus::Running,
                368,
                None,
                5,
            ),
            background(
                "demo-map-task-3",
                BackgroundTaskKind::Command,
                "cargo watch -x check",
                BackgroundTaskStatus::Running,
                423,
                None,
                0,
            ),
        ],
        subagents: map_subagents(now),
        ..details()
    };
    Sample {
        summary,
        items,
        details,
    }
}

/// The parser agent's background agents as the map shows them, with its foreground subagent:
/// more than the map lists before it folds the oldest ended ones.
fn parser_subagents(now: SystemTime) -> Vec<Subagent> {
    let tasks = parser_background_tasks(now);
    let foreground = Subagent {
        item: ItemId::new(4),
        parent: None,
        description: "Find where the parser builds its errors".to_owned(),
        agent_type: Some("Explore".to_owned()),
        run: SubagentRun::Foreground,
        status: SubagentStatus::Done,
        started_at: at(now, 1600),
        ended_at: Some(at(now, 1540)),
        tokens: Some(11_800),
        tool_calls: 4,
        last_tool: None,
        error: None,
        turn: 20,
        task: None,
    };
    let background = tasks
        .into_iter()
        .filter(|task| task.kind == BackgroundTaskKind::Agent)
        .zip(200_u32..)
        .map(|(task, item)| Subagent {
            item: ItemId::new(item),
            parent: None,
            description: task.description,
            agent_type: Some("general-purpose".to_owned()),
            run: SubagentRun::Background,
            status: match task.status {
                BackgroundTaskStatus::Running => SubagentStatus::Running,
                BackgroundTaskStatus::Done => SubagentStatus::Done,
                BackgroundTaskStatus::Failed => SubagentStatus::Failed,
                BackgroundTaskStatus::Stopped => SubagentStatus::Stopped,
            },
            started_at: task.started_at,
            ended_at: task.ended_at,
            tokens: Some(u64::from(task.tool_calls) * 2_300),
            tool_calls: task.tool_calls,
            last_tool: None,
            error: (task.status == BackgroundTaskStatus::Failed)
                .then(|| "The grammar file did not parse: unexpected `|` on line 41".to_owned()),
            turn: 20,
            task: Some(task.id),
        });
    std::iter::once(foreground).chain(background).collect()
}

/// What the daemon would change after an answer or a stop in the map's sample: the approval is
/// answered and its subagent runs again, a stopped task's subagent stops.
fn apply_map_request(state: &mut AppState, request: &Request) {
    let now = SystemTime::now();
    match request {
        Request::AgentAnswer(params) => {
            let Some(entry) = state.agents.get_mut(&params.agent) else {
                return;
            };
            let waiting = entry.transcript.items().find(|item| {
                matches!(
                    &item.body,
                    ItemBody::Approval(approval) if approval.pending == params.pending
                )
            });
            let (Some(waiting), Answer::Approval(answer)) = (waiting.cloned(), &params.answer)
            else {
                return;
            };
            let TranscriptItem {
                id,
                parent,
                body: ItemBody::Approval(approval),
            } = waiting
            else {
                return;
            };
            let answered = ItemBody::Approval(Approval {
                answer: AnswerState::Answered(answer.clone()),
                ..approval
            });
            entry.transcript.change(TranscriptItem {
                id,
                parent,
                body: answered,
            });
            if let (Some(parent), Some(details)) = (parent, entry.details.loaded_mut())
                && let Some(subagent) = details
                    .subagents
                    .iter_mut()
                    .find(|subagent| subagent.item == parent)
            {
                subagent.status = SubagentStatus::Running;
            }
            if entry.transcript.waiting().is_none() {
                entry.summary.status = AgentStatus::Working;
                entry.summary.pending_count = 0;
                entry.summary.activity = None;
            }
        }
        Request::AgentStopTask(params) => {
            let Some(details) = state
                .agents
                .get_mut(&params.agent)
                .and_then(|entry| entry.details.loaded_mut())
            else {
                return;
            };
            let ended = Timestamp::from_system_time(now);
            for task in &mut details.background_tasks {
                if task.id == params.task && task.status == BackgroundTaskStatus::Running {
                    task.status = BackgroundTaskStatus::Stopped;
                    task.ended_at = ended;
                }
            }
            let mut stopped: Vec<ItemId> = Vec::new();
            for subagent in &mut details.subagents {
                if subagent.task.as_ref() == Some(&params.task) && subagent.status.is_running() {
                    subagent.status = SubagentStatus::Stopped;
                    subagent.ended_at = ended;
                    stopped.push(subagent.item);
                }
            }
            // A foreground subagent cannot outlive the subagent that started it.
            while let Some(parent) = stopped.pop() {
                for subagent in &mut details.subagents {
                    let under = subagent.parent == Some(parent);
                    if under
                        && subagent.run == SubagentRun::Foreground
                        && subagent.status.is_running()
                    {
                        subagent.status = SubagentStatus::Stopped;
                        subagent.ended_at = ended;
                        stopped.push(subagent.item);
                    }
                }
            }
        }
        _ => {}
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

/// The demo user's home folder on the server.
const DEMO_HOME: &str = "/home/dev";

fn accounts() -> Vec<Account> {
    vec![
        Account {
            id: AccountId::DEFAULT,
            name: "Personal".to_owned(),
            folder: PathBuf::from("/home/dev/.claude"),
            credentials: CredentialSource::SignIn,
        },
        Account {
            id: AccountId::new(WORK_ACCOUNT),
            name: "Work".to_owned(),
            folder: PathBuf::from("/home/dev/.claude-work"),
            credentials: CredentialSource::SignIn,
        },
        Account {
            id: AccountId::new(LAB_ACCOUNT),
            name: "Lab".to_owned(),
            folder: PathBuf::from("/home/dev/.claude-lab"),
            credentials: CredentialSource::Environment,
        },
        Account {
            id: AccountId::new(CLIENT_ACCOUNT),
            name: "Client".to_owned(),
            folder: PathBuf::from("/home/dev/.claude-client"),
            credentials: CredentialSource::SignIn,
        },
    ]
}

/// Whether the demo's daemon finds a login. The client account signs in once its card asks
/// again, and an added account once it is asked a second time: the demo's stand-in for the user
/// running the command in a terminal.
fn account_state(state: &AppState, account: Account) -> AccountState {
    let known = state.machines.this.sign_ins.sign_in(account.id);
    let card_asks = state.ui.selected == Some(AgentId::new(SIGN_IN_AGENT))
        && state.ui.dialog.is_none()
        && !state.machines.this.sign_ins.states.is_empty();
    let signed_in = match account.id.get() {
        CLIENT_ACCOUNT => known == Some(SignIn::SignedIn) || card_asks,
        id if id > CLIENT_ACCOUNT => known.is_some(),
        _ => true,
    };
    let sign_in = if signed_in {
        SignIn::SignedIn
    } else {
        SignIn::NotSignedIn
    };
    let folder = account.folder.to_string_lossy().replace('\'', r"'\''");
    let sign_in_command = if account.id.is_default() {
        "claude auth login".to_owned()
    } else {
        format!("CLAUDE_CONFIG_DIR='{folder}' claude auth login")
    };
    // The work account runs through Amazon Bedrock, which its settings choose.
    let route = if account.id.get() == WORK_ACCOUNT {
        ClaudeRoute::Bedrock
    } else {
        ClaudeRoute::Anthropic
    };
    AccountState {
        account,
        sign_in,
        sign_in_command,
        route,
    }
}

/// What the daemon would change after an account request: its accounts, and the agents whose
/// account signed in.
fn apply_account_request(state: &mut AppState, request: &Request) {
    match request {
        Request::AccountsList => {
            let signed_in: Vec<AccountId> = state
                .machines
                .this
                .accounts
                .iter()
                .filter(|account| {
                    account_state(state, (*account).clone()).sign_in == SignIn::SignedIn
                })
                .map(|account| account.id)
                .collect();
            for entry in state.agents.values_mut() {
                let summary = &mut entry.summary;
                if summary.sign_in_wait == SignInWait::Waiting
                    && signed_in.contains(&summary.account)
                {
                    summary.sign_in_wait = SignInWait::NotWaiting;
                    summary.status = AgentStatus::Idle;
                }
            }
        }
        Request::AccountsAdd(_) | Request::AccountsChange(_) => {
            if let Ok(ResponseBody::Account(added)) = answer(state, request) {
                let account = added.account;
                match state
                    .machines
                    .this
                    .accounts
                    .iter_mut()
                    .find(|kept| kept.id == account.id)
                {
                    Some(kept) => *kept = account,
                    None => state.machines.this.accounts.push(account),
                }
            }
        }
        Request::AccountsRemove(params) => {
            state
                .machines
                .this
                .accounts
                .retain(|account| account.id != params.account);
        }
        _ => {}
    }
}

fn work_plan_usage(now: SystemTime) -> PlanUsage {
    let mut usage = PlanUsage::default();
    usage.windows.insert(
        UsageWindow::FiveHour,
        WindowUsage {
            utilization: Some(0.81),
            resets_at: Some(in_future(now, 40 * 60)),
            status: RateLimitStatus::Allowed,
        },
    );
    usage.windows.insert(
        UsageWindow::SevenDay,
        WindowUsage {
            utilization: Some(0.31),
            resets_at: Some(in_future(now, 6 * 24 * 3600)),
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
            folder: in_folder(PLATFORM_FOLDER, folder),
            last_activity: at(now, index * 7 * 60),
            turns: 12,
            // Every third one ran with the lab's API key.
            account: AccountId::new(if index % 3 == 0 {
                LAB_ACCOUNT
            } else {
                WORK_ACCOUNT
            }),
        });
    }
    sessions.extend([
        PastSession {
            id: ClaudeSessionId::new("demo-past-1".to_owned()),
            title: "Tokenizer speedup".to_owned(),
            folder: PathBuf::from(PARSER_FOLDER),
            last_activity: at(now, 3 * 3600),
            turns: 37,
            account: AccountId::DEFAULT,
        },
        PastSession {
            id: ClaudeSessionId::new("demo-past-2".to_owned()),
            title: "Error page redesign".to_owned(),
            folder: PathBuf::from("/home/dev/projects/docs-site"),
            last_activity: at(now, 3 * 24 * 3600),
            turns: 22,
            account: AccountId::DEFAULT,
        },
    ]);
    sessions
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{FoldersListParams, HistoryParams, ReadFileParams, RenameParams};

    use super::*;
    use crate::state::{HistoryState, WaitingKind};

    #[test]
    fn an_update_screen_needs_the_demo() {
        let arguments = |words: &[&str]| -> Vec<String> {
            words.iter().map(|word| (*word).to_owned()).collect()
        };

        assert_eq!(update_screen(&arguments(&["--demo-update", "older"])), None);
        assert_eq!(
            update_screen(&arguments(&["--demo", "--demo-update"])),
            None
        );
        assert_eq!(
            update_screen(&arguments(&["--demo", "--demo-update", "older"])),
            Some("older")
        );
    }

    #[test]
    fn the_demo_load_moves_with_the_clock_and_adds_up() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let later = now + Duration::from_secs(LOAD_INTERVAL_SECONDS);

        let first = machine_load(now);
        let second = machine_load(later);

        assert_ne!(first.server.cpu, second.server.cpu);
        assert_eq!(first.history.server.len(), 100);
        assert!(
            first
                .history
                .agents
                .iter()
                .all(|agent| agent.cpu.len() == 100)
        );
        assert!(first.others.cpu.get() > 0.0);
        assert!(first.server.cpu.get() < f64::from(first.server.cores));
        let building = first.agents.first().unwrap();
        assert_eq!(building.agent, AgentId::new(BUILDING_AGENT));
        assert!(building.cpu.get() > 8.0);
    }

    #[test]
    fn the_demo_lists_made_up_folders_as_the_daemon_would() {
        let state = state(label());
        let list = |path: &str| {
            answer(
                &state,
                &Request::FoldersList(FoldersListParams {
                    path: PathBuf::from(path),
                }),
            )
        };
        let names = |path: &str| match list(path) {
            Ok(ResponseBody::Directory(listing)) => listing
                .entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<String>>(),
            other => panic!("a listing of {path}, not {other:?}"),
        };

        assert_eq!(names("/"), ["home", "tmp"]);
        assert_eq!(
            names("/home/dev/"),
            [".cache", ".config", "notes", "projects"]
        );
        assert!(names("/home/dev/projects").contains(&"calc-parser".to_owned()));
        assert!(names("/home/dev/projects/calc-parser").contains(&"src".to_owned()));
        assert!(list("/home/dev/nowhere").is_err());
        assert!(list("~/projects").is_err());
    }

    #[test]
    fn a_rename_shows_in_the_demo_title() {
        let mut state = state(label());
        let agent = AgentId::new(LIVE_AGENT);
        let rename = Request::AgentRename(RenameParams {
            agent,
            title: "🚀 Stream the input".to_owned(),
        });

        assert!(answer(&state, &rename).is_ok());
        apply(&mut state, &rename);

        assert_eq!(state.agents[&agent].summary.title, "🚀 Stream the input");
    }

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
    fn the_live_agent_loads_a_long_conversation_when_it_opens() {
        let state = state(label());
        let agent = AgentId::new(LIVE_AGENT);
        let request = Request::AgentHistory(HistoryParams {
            agent,
            before: None,
            limit: 100,
        });

        let Ok(ResponseBody::History(page)) = answer(&state, &request) else {
            panic!("the live agent answers its history");
        };

        assert_eq!(
            state.agents[&agent].transcript.history(),
            &HistoryState::NotLoaded
        );
        assert_eq!(page.items.len(), 140);
        assert_eq!(page.older_items, OlderItems::Exist);
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
        assert_eq!(page.older_items, OlderItems::None);
    }

    /// `src/error.rs` before the sample's edit.
    const ERROR_BEFORE: &str = "/// A parse error, with where it happened.
pub struct ParseError {
    pub line: u32,
    pub message: String,
}
";

    fn label() -> DaemonLabel {
        DaemonLabel::ssh("demo".to_owned())
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
                Some(WaitingKind::Approval),
                // Its account waits for a sign-in: no answer is asked.
                None,
                // The server's agents ask nothing.
                None,
                None,
                None,
            ]
        );
        let sign_in = state.agents.get(&AgentId::new(SIGN_IN_AGENT)).unwrap();
        assert_eq!(sign_in.summary.sign_in_wait, SignInWait::Waiting);
        assert!(crate::state::accounts::sign_in_turn(sign_in).is_some());
    }

    #[test]
    fn folders_list_as_the_daemon_lists_them() {
        let names = |path: &str| -> Vec<String> {
            let Ok(ResponseBody::Directory(listing)) = list_dir(Path::new(path)) else {
                panic!("{path} lists");
            };
            listing
                .entries
                .into_iter()
                .map(|entry| entry.name)
                .collect()
        };

        assert_eq!(
            names(""),
            [
                "benches",
                "src",
                "target",
                "tests",
                ".gitignore",
                "AGENTS.md",
                "Cargo.toml",
                "README.md"
            ]
        );
        assert_eq!(names(PARSER_COLUMNS_WORKTREE), ["crates"]);
        assert!(matches!(
            list_dir(Path::new("/home/dev/projects")),
            Err(RequestError::Failed(body)) if body.code == ErrorCode::InvalidRequest
        ));
        assert!(matches!(
            find_files("LEX"),
            ResponseBody::FileMatches { matches } if matches.len() == 1
        ));
    }

    #[test]
    fn names_are_looked_up_as_the_daemon_does() {
        let read = |path: &str| read_file(Path::new(path));
        let position = in_folder(PARSER_COLUMNS_WORKTREE, POSITION_FILE);
        let lib = in_folder(PARSER_FOLDER, LIB_FILE);

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
            .filter(|entry| MachineId::of(entry.id()) == MachineId::THIS)
            .map(|entry| entry.id().get())
            .collect();

        assert_eq!(order, SIDEBAR_ORDER);
    }

    #[test]
    fn the_changed_file_undoes_to_its_original() {
        let state = state(label());
        let entry = &state.agents[&AgentId::new(1)];
        let request = Request::AgentReadFile(ReadFileParams {
            agent: entry.id(),
            path: PathBuf::from(ERROR_FILE),
        });

        let Ok(ResponseBody::File { text, edits, .. }) = answer(&state, &request) else {
            panic!("the demo answers with the file");
        };
        let edits: Vec<&FileEdit> = edits.iter().flatten().collect();

        assert_eq!(
            file_edits::original_text(&text, &edits).as_deref(),
            Some(ERROR_BEFORE)
        );
    }

    #[test]
    fn a_server_shows_its_own_agents_in_order() {
        let state = state(label());

        let server = state.machines.others.get(&SERVER).unwrap();
        let titles: Vec<&str> = state
            .agents_in_order()
            .into_iter()
            .filter(|entry| MachineId::of(entry.id()) == SERVER)
            .map(|entry| entry.summary.title.as_str())
            .collect();

        assert!(server.is_connected());
        assert_eq!(server.daemon.machine, SERVER_HOST);
        assert_eq!(
            titles,
            [
                "Migrate the invoices",
                "Find the slow invoice query",
                "Renew the staging certificates",
            ]
        );
    }

    #[test]
    fn the_draft_quotes_each_passage_of_the_answers_with_its_comment() {
        let drafts = drafts();

        let draft = drafts.draft(AgentId::new(1)).unwrap();
        let shown: Vec<(ItemId, Option<&str>, &str)> = draft
            .quotes
            .iter()
            .zip(DEMO_QUOTES)
            .map(|(quote, demo)| {
                let anchor = quote.passage.anchor;
                let quoted = demo.answer.get(
                    anchor.start.byte_offset(demo.answer)..anchor.end.byte_offset(demo.answer),
                );
                (anchor.item, quoted, quote.comment.as_str())
            })
            .collect();
        let expected: Vec<(ItemId, Option<&str>, &str)> = DEMO_QUOTES
            .iter()
            .map(|demo| (ItemId::new(demo.item), Some(demo.passage), demo.comment))
            .collect();

        assert_eq!(draft.quotes.len(), DEMO_QUOTES.len());
        assert_eq!(shown, expected);
        assert!(
            draft
                .quotes
                .iter()
                .all(|quote| quote.placement == CommentPlacement::Beside)
        );
        assert!(drafts.quote_editor.is_none());
    }

    #[test]
    fn the_server_answers_with_its_own_sessions_folders_accounts_and_no_files() {
        let state = state(label());
        let ask = |request: Request| answer_from(&state, SERVER, &request);
        let agent = SERVER.app_agent(AgentId::new(1));

        let sessions = ask(Request::SessionsList);
        let folders = ask(Request::FoldersList(FoldersListParams {
            path: PathBuf::from("/home/dev/"),
        }));
        let accounts = ask(Request::AccountsList);
        let file = ask(Request::AgentReadFile(ReadFileParams {
            agent,
            path: PathBuf::from(SERVER_FOLDER).join("README.md"),
        }));

        assert!(matches!(sessions, Ok(ResponseBody::Sessions { sessions }) if sessions.is_empty()));
        let Ok(ResponseBody::Directory(listing)) = folders else {
            panic!("a listing, not {folders:?}");
        };
        let names: Vec<String> = listing
            .entries
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, ["services"]);
        let Ok(ResponseBody::Accounts { accounts, .. }) = accounts else {
            panic!("the accounts, not {accounts:?}");
        };
        assert_eq!(accounts.len(), 1);
        assert!(file.is_err());
    }

    #[test]
    fn a_server_shows_no_agent_of_this_computer_in_its_load() {
        let state = state(label());

        let load = answer_from(&state, SERVER, &Request::MachineLoad);

        let Ok(ResponseBody::MachineLoad(load)) = load else {
            panic!("the load, not {load:?}");
        };
        assert!(load.agents.is_empty());
    }

    #[test]
    fn a_machine_without_agents_still_lists_its_home() {
        let state = state(label());
        let empty = MachineId::new(2);
        let names = |path: &str| {
            let listed = answer_from(
                &state,
                empty,
                &Request::FoldersList(FoldersListParams {
                    path: PathBuf::from(path),
                }),
            );
            let Ok(ResponseBody::Directory(listing)) = listed else {
                panic!("a listing of {path}, not {listed:?}");
            };
            listing
                .entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<String>>()
        };

        assert_eq!(names("/"), ["home"]);
        assert_eq!(names("/home"), ["dev"]);
    }

    #[test]
    fn no_two_agents_share_a_session() {
        let state = state(label());

        let sessions: BTreeSet<&ClaudeSessionId> = state
            .agents
            .values()
            .filter_map(|entry| entry.summary.session.as_ref())
            .collect();

        assert_eq!(sessions.len(), state.agents.len());
    }

    #[test]
    fn the_showcase_needs_the_demo() {
        let arguments = |list: &[&str]| -> Vec<String> {
            list.iter().map(|&argument| argument.to_owned()).collect()
        };

        assert!(showcase_requested(&arguments(&[DEMO_FLAG, SHOWCASE_FLAG])));
        assert!(!showcase_requested(&arguments(&[SHOWCASE_FLAG])));
    }

    #[test]
    fn the_showcase_waits_on_one_agent_per_machine_with_two_accounts() {
        let mut state = state(label());

        showcase(&mut state);

        let waiting_on = |machine: MachineId| {
            state
                .agents
                .values()
                .filter(|entry| MachineId::of(entry.id()) == machine)
                .filter(|entry| entry.summary.status == AgentStatus::NeedsYou)
                .count()
        };
        let titles_start_with_a_letter = state.agents.values().all(|entry| {
            entry
                .summary
                .title
                .starts_with(|character: char| character.is_alphanumeric())
        });
        let no_two_sessions: BTreeSet<&ClaudeSessionId> = state
            .agents
            .values()
            .filter_map(|entry| entry.summary.session.as_ref())
            .collect();
        assert_eq!(waiting_on(MachineId::THIS), 1);
        assert_eq!(waiting_on(SERVER), 1);
        assert_eq!(state.machines.others.len(), 2);
        assert_eq!(state.machines.this.accounts.len(), 2);
        assert!(titles_start_with_a_letter);
        assert_eq!(no_two_sessions.len(), state.agents.len());
        assert!(
            state.agents.contains_key(&AgentId::new(1)),
            "the showcase keeps agent 1, which the demo selects and its draft quotes"
        );
    }
}

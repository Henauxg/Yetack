//! A fake `claude` for the integration tests: it speaks Claude Code's JSON mode on stdin and
//! stdout, and the first word of each prompt picks a script.
//!
//! - `approve`: a `Bash` call that asks for approval, then says what the answer was.
//! - `question`: an `AskUserQuestion` call, then repeats the answers.
//! - `plan`: an `ExitPlanMode` call, then says whether the plan was approved.
//! - `slow`: starts a turn and waits for an interrupt.
//! - `garbage`: writes lines that do not parse, then answers.
//! - `linger`: starts a child process that ignores stdin, answers "Started child <pid>", and
//!   from then on keeps running after its stdin closes, as a `claude` that hangs.
//! - `/rename <title>`: names the session, as Claude Code does. With `FAKE_CLAUDE_SESSION_NAMES`
//!   set to a folder, it keeps the name there, and a resumed fake restates it before it answers
//!   `initialize`.
//! - anything else: answers "Echo: <prompt>".
//!
//! Its `get_settings` answer applies the `--effort` it was given, or else a default that depends
//! on the model, as Claude Code's does: `medium` for its own default model, `low` for any other.
//!
//! With `FAKE_CLAUDE_ARGS_FILE` set, it appends its command line to that file, one JSON array per
//! line: `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=<value>` first when that variable is set, as a shell
//! writes it, then its arguments. It exits when its stdin closes.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const ARGS_FILE_ENV: &str = "FAKE_CLAUDE_ARGS_FILE";
/// A folder that holds each session's name, in a file named after it.
const SESSION_NAMES_ENV: &str = "FAKE_CLAUDE_SESSION_NAMES";
const RESUME_INTERRUPTED_TURN_ENV: &str = "CLAUDE_CODE_RESUME_INTERRUPTED_TURN";
/// The child of `linger`, which outlives any test.
const LINGERING_CHILD: [&str; 2] = ["sleep", "600"];
const RESUME_FLAG: &str = "--resume=";
const MODE_FLAG: &str = "--permission-mode=";
const MODEL_FLAG: &str = "--model=";
const EFFORT_FLAG: &str = "--effort=";
const DEFAULT_MODEL: &str = "fake-model";
const INTERRUPTED_NOTICE: &str = "[Request interrupted by user]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Approve,
    Question,
    Plan,
}

#[derive(Debug)]
enum Turn {
    Waiting {
        request_id: String,
        tool_use_id: String,
        script: Script,
    },
    Slow,
}

struct Fake<W> {
    out: W,
    session: String,
    /// Started with `--resume`: Claude Code restates the session's name as it starts.
    resumed: bool,
    model: String,
    /// The `--effort` it was given.
    effort: Option<String>,
    mode: String,
    total_cost: f64,
    next_id: u32,
    turn: Option<Turn>,
    queued: VecDeque<String>,
    /// Set by `linger`: the fake no longer exits when its stdin closes.
    lingers: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fake-claude: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    use std::io::BufRead;

    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(path) = std::env::var_os(ARGS_FILE_ENV) {
        let resume_turn = std::env::var_os(RESUME_INTERRUPTED_TURN_ENV)
            .map(|value| format!("{RESUME_INTERRUPTED_TURN_ENV}={}", value.to_string_lossy()));
        let command_line: Vec<&str> = resume_turn
            .iter()
            .chain(&args)
            .map(String::as_str)
            .collect();
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", json!(command_line))?;
    }
    let flag = |prefix: &str| {
        args.iter()
            .find_map(|arg| arg.strip_prefix(prefix).map(str::to_owned))
    };
    let mut fake = Fake {
        out: io::stdout().lock(),
        session: flag(RESUME_FLAG)
            .unwrap_or_else(|| format!("fake-session-{}", std::process::id())),
        resumed: flag(RESUME_FLAG).is_some(),
        model: flag(MODEL_FLAG).unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        effort: flag(EFFORT_FLAG),
        mode: flag(MODE_FLAG).unwrap_or_else(|| "default".to_owned()),
        total_cost: 0.0,
        next_id: 1,
        turn: None,
        queued: VecDeque::new(),
        lingers: false,
    };
    for line in io::stdin().lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        fake.read(&line, &message)?;
    }
    if fake.lingers {
        // Only a kill ends it.
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    Ok(())
}

impl<W: Write> Fake<W> {
    fn read(&mut self, line: &str, message: &Value) -> io::Result<()> {
        match text(message, &["type"]) {
            "control_request" => self.control_request(message),
            "control_response" => {
                // Claude Code echoes the answers it gets.
                writeln!(self.out, "{line}")?;
                self.out.flush()?;
                self.answered(message)
            }
            "user" => {
                let prompt = prompt_text(message);
                if self.turn.is_some() {
                    self.queued.push_back(prompt);
                    Ok(())
                } else {
                    self.start_turn(&prompt)
                }
            }
            _ => Ok(()),
        }
    }

    fn control_request(&mut self, message: &Value) -> io::Result<()> {
        let request_id = text(message, &["request_id"]).to_owned();
        let body = match text(message, &["request", "subtype"]) {
            "initialize" => {
                self.restate_session_name()?;
                json!({"commands": [
                {"name": "review", "description": "Review the changes", "argumentHint": ""},
                {"name": "tidy", "description": "Tidy the code", "argumentHint": "[path]"}
                ]})
            }
            "get_context_usage" => json!({
                "categories": [
                    {"name": "System prompt", "tokens": 3000},
                    {"name": "Messages", "tokens": 1000}
                ],
                "totalTokens": 4000, "maxTokens": 200000, "rawMaxTokens": 200000, "percentage": 2
            }),
            "mcp_status" => json!({"mcpServers": [{
                "name": "notes", "status": "connected", "scope": "project",
                "config": {"type": "stdio", "command": "notes-server", "args": []},
                "serverInfo": {"name": "notes", "version": "1.0.0"},
                "tools": [{"name": "search", "annotations": {"readOnly": true}}]
            }]}),
            "set_permission_mode" => {
                self.mode = text(message, &["request", "mode"]).to_owned();
                json!({})
            }
            "set_model" => {
                self.model = text(message, &["request", "model"]).to_owned();
                json!({})
            }
            "get_settings" => {
                let default = if self.model == DEFAULT_MODEL {
                    "medium"
                } else {
                    "low"
                };
                let effort = self.effort.as_deref().unwrap_or(default);
                json!({"effective": {}, "sources": [],
                       "applied": {"model": self.model, "effort": effort}})
            }
            "mcp_reconnect" | "mcp_toggle" | "stop_task" => json!({}),
            "interrupt" => {
                self.emit(&json!({"type": "control_response", "response": {
                    "subtype": "success", "request_id": request_id, "response": {"still_queued": []}
                }}))?;
                return self.interrupt();
            }
            other => {
                return self.emit(&json!({"type": "control_response", "response": {
                    "subtype": "error", "request_id": request_id,
                    "error": format!("unknown request {other}")
                }}));
            }
        };
        self.emit(&json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id, "response": body
        }}))
    }

    fn start_turn(&mut self, prompt: &str) -> io::Result<()> {
        self.emit(&json!({
            "type": "system", "subtype": "init", "session_id": self.session,
            "model": self.model, "permissionMode": self.mode, "cwd": ".",
            "tools": ["Bash", "AskUserQuestion", "ExitPlanMode"],
            "slash_commands": ["review", "tidy"], "skills": ["tidy"],
            "mcp_servers": [{"name": "notes", "status": "connected"}]
        }))?;
        self.emit(&json!({"type": "rate_limit_event", "rate_limit_info": {
            "status": "allowed", "rateLimitType": "five_hour", "resetsAt": 1_900_000_000,
            "unifiedWindows": {"five_hour": {
                "utilization": 0.25, "resetsAt": 1_900_000_000, "status": "allowed"
            }}
        }}))?;
        self.emit(&json!({
            "type": "user", "isReplay": true, "session_id": self.session,
            "message": {"role": "user", "content": prompt}
        }))?;
        let first_word = prompt.split_whitespace().next().unwrap_or_default();
        match first_word {
            "approve" => self.ask(
                Script::Approve,
                "Bash",
                json!({"command": "touch made-by-fake"}),
            ),
            "question" => self.ask(
                Script::Question,
                "AskUserQuestion",
                json!({"questions": [{
                    "question": "Which color?", "header": "Color", "multiSelect": false,
                    "options": [
                        {"label": "Red", "description": "Warm"},
                        {"label": "Blue", "description": "Cold"}
                    ]
                }]}),
            ),
            "plan" => self.ask(
                Script::Plan,
                "ExitPlanMode",
                json!({"plan": "1. Look\n2. Change"}),
            ),
            "slow" => {
                self.say("Working on it.")?;
                self.turn = Some(Turn::Slow);
                Ok(())
            }
            "linger" => {
                let [program, seconds] = LINGERING_CHILD;
                let child = Command::new(program)
                    .arg(seconds)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?;
                self.lingers = true;
                self.say(&format!("Started child {}", child.id()))?;
                self.end_turn()
            }
            "garbage" => {
                writeln!(self.out, "this is not json")?;
                self.emit(&json!({"type": "brand_new_type", "value": 1}))?;
                self.emit(&json!({"type": "control_request", "request_id": "bad-1",
                                  "request": {"subtype": "can_use_tool"}}))?;
                self.say("Still here.")?;
                self.end_turn()
            }
            "/rename" => {
                let title = prompt.trim_start_matches(first_word).trim();
                self.emit(
                    &json!({"type": "system", "subtype": "session_title_changed",
                                  "title": title, "session_id": self.session}),
                )?;
                if let Some(path) = session_name_file(&self.session) {
                    if let Some(folder) = path.parent() {
                        std::fs::create_dir_all(folder)?;
                    }
                    std::fs::write(path, title)?;
                }
                self.emit(&json!({"type": "assistant", "session_id": self.session, "message": {
                    "model": "<synthetic>",
                    "content": [{"type": "text", "text": format!("Session renamed to: {title}")}]
                }}))?;
                self.turn = None;
                self.emit(&json!({
                    "type": "result", "subtype": "success", "is_error": false, "num_turns": 0,
                    "total_cost_usd": self.total_cost, "session_id": self.session
                }))?;
                self.next_queued()
            }
            "/effort" => {
                self.emit(
                    &json!({"type": "user", "session_id": self.session, "message": {
                        "role": "user",
                        "content": "<local-command-stdout>Effort set</local-command-stdout>"
                    }}),
                )?;
                self.end_turn()
            }
            _ => {
                self.say(&format!("Echo: {prompt}"))?;
                self.end_turn()
            }
        }
    }

    /// A tool call that waits for the daemon's answer.
    fn ask(&mut self, script: Script, tool: &str, input: Value) -> io::Result<()> {
        let tool_use_id = format!("toolu-{}", self.next_id);
        let request_id = format!("perm-{}", self.next_id);
        self.next_id += 1;
        self.emit(
            &json!({"type": "assistant", "session_id": self.session, "message": {
                "model": self.model,
                "content": [{"type": "tool_use", "id": tool_use_id, "name": tool, "input": input}]
            }}),
        )?;
        self.emit(
            &json!({"type": "control_request", "request_id": request_id, "request": {
                "subtype": "can_use_tool", "tool_name": tool, "tool_use_id": tool_use_id,
                "input": input
            }}),
        )?;
        self.turn = Some(Turn::Waiting {
            request_id,
            tool_use_id,
            script,
        });
        Ok(())
    }

    /// The daemon answered a `can_use_tool` request.
    fn answered(&mut self, message: &Value) -> io::Result<()> {
        let Some(Turn::Waiting {
            request_id,
            tool_use_id,
            script,
        }) = &self.turn
        else {
            return Ok(());
        };
        if text(message, &["response", "request_id"]) != request_id {
            return Ok(());
        }
        let (tool_use_id, script) = (tool_use_id.clone(), *script);
        let decision = message
            .pointer("/response/response")
            .cloned()
            .unwrap_or(Value::Null);
        let allowed = text(&decision, &["behavior"]) == "allow";
        let denial = text(&decision, &["message"]).to_owned();
        let result = if allowed { "done" } else { denial.as_str() };
        self.tool_result(&tool_use_id, result, !allowed)?;
        let reply = match (script, allowed) {
            (Script::Approve, true) => "Allowed.".to_owned(),
            (Script::Approve, false) => format!("Denied: {denial}"),
            (Script::Question, true) => format!(
                "Answers: {}",
                decision
                    .pointer("/updatedInput/answers")
                    .cloned()
                    .unwrap_or(Value::Null)
            ),
            (Script::Question, false) => "No answer.".to_owned(),
            (Script::Plan, true) => "Plan approved.".to_owned(),
            (Script::Plan, false) => format!("Keep planning: {denial}"),
        };
        self.say(&reply)?;
        self.end_turn()
    }

    fn interrupt(&mut self) -> io::Result<()> {
        let Some(turn) = self.turn.take() else {
            return Ok(());
        };
        if let Turn::Waiting { request_id, .. } = turn {
            self.emit(&json!({"type": "control_cancel_request", "request_id": request_id}))?;
        }
        self.emit(
            &json!({"type": "user", "session_id": self.session, "message": {
                "role": "user", "content": [{"type": "text", "text": INTERRUPTED_NOTICE}]
            }}),
        )?;
        self.emit(&json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "terminal_reason": "aborted_streaming", "errors": ["aborted"],
            "duration_ms": 300, "num_turns": 1, "total_cost_usd": self.total_cost,
            "session_id": self.session
        }))?;
        self.next_queued()
    }

    fn end_turn(&mut self) -> io::Result<()> {
        self.turn = None;
        self.total_cost += 0.01;
        self.emit(&json!({
            "type": "result", "subtype": "success", "is_error": false, "duration_ms": 1200,
            "num_turns": 1, "total_cost_usd": self.total_cost, "session_id": self.session,
            "usage": {"input_tokens": 10, "output_tokens": 20,
                      "cache_creation_input_tokens": 30, "cache_read_input_tokens": 40}
        }))?;
        self.next_queued()
    }

    fn next_queued(&mut self) -> io::Result<()> {
        match self.queued.pop_front() {
            Some(prompt) => self.start_turn(&prompt),
            None => Ok(()),
        }
    }

    fn say(&mut self, text: &str) -> io::Result<()> {
        self.emit(
            &json!({"type": "assistant", "session_id": self.session, "message": {
                "model": self.model, "content": [{"type": "text", "text": text}]
            }}),
        )
    }

    fn tool_result(&mut self, tool_use_id: &str, content: &str, is_error: bool) -> io::Result<()> {
        self.emit(
            &json!({"type": "user", "session_id": self.session, "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result", "tool_use_id": tool_use_id, "content": content,
                    "is_error": is_error
                }]
            }}),
        )
    }

    /// The name a `/rename` gave the resumed session, as Claude Code restates it at start.
    fn restate_session_name(&mut self) -> io::Result<()> {
        if !self.resumed {
            return Ok(());
        }
        let Some(title) =
            session_name_file(&self.session).and_then(|path| std::fs::read_to_string(path).ok())
        else {
            return Ok(());
        };
        self.emit(
            &json!({"type": "system", "subtype": "session_title_changed",
                          "title": title, "session_id": self.session}),
        )
    }

    fn emit(&mut self, message: &Value) -> io::Result<()> {
        writeln!(self.out, "{message}")?;
        self.out.flush()
    }
}

/// Where the name `/rename` gave a session is kept, when `FAKE_CLAUDE_SESSION_NAMES` is set.
fn session_name_file(session: &str) -> Option<PathBuf> {
    let folder = std::env::var_os(SESSION_NAMES_ENV)?;
    Some(PathBuf::from(folder).join(session))
}

/// The string at `path` in `value`, or "".
fn text<'value>(value: &'value Value, path: &[&str]) -> &'value str {
    let mut current = value;
    for key in path {
        match current.get(key) {
            Some(next) => current = next,
            None => return "",
        }
    }
    current.as_str().unwrap_or_default()
}

/// A prompt's text: a string, or its text blocks joined.
fn prompt_text(message: &Value) -> String {
    match message.pointer("/message/content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

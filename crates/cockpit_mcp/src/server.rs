//! The JSON-RPC side: the methods, the two tools, and their words for the model.

use std::io;

use cockpit_client::Transport;
use cockpit_daemon::{LIST_TERMINALS_TOOL, MCP_SERVER_NAME, READ_TERMINAL_TOOL};
use cockpit_protocol::{
    AgentId, MAX_TEXT_LINES, SHARE_LABEL, TERMINAL_WORD, TerminalId, TerminalInfo,
};
use serde_json::{Value, json};
use strum::{EnumString, IntoStaticStr, VariantArray};

use crate::daemon::{Daemon, Terminals};

/// The lines `read_terminal` answers when the agent names no number.
const DEFAULT_LINES: u32 = 200;
/// The version answered to a client that names one the server does not speak.
const LATEST_MCP_VERSION: &str = "2025-11-25";
/// The MCP versions the server speaks. Its tools work the same in each.
const MCP_VERSIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", LATEST_MCP_VERSION];
const JSON_RPC: &str = "2.0";
/// The keys of a JSON-RPC message, and of MCP's.
const JSON_RPC_KEY: &str = "jsonrpc";
const ID_KEY: &str = "id";
const RESULT_KEY: &str = "result";
const ERROR_KEY: &str = "error";
const NAME_KEY: &str = "name";
const PROTOCOL_VERSION_KEY: &str = "protocolVersion";
/// The arguments of `read_terminal`.
const TERMINAL_ARGUMENT: &str = "terminal";
const LINES_ARGUMENT: &str = "lines";

/// The methods the server answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
enum Method {
    #[strum(serialize = "initialize")]
    Initialize,
    #[strum(serialize = "ping")]
    Ping,
    #[strum(serialize = "tools/list")]
    ToolsList,
    #[strum(serialize = "tools/call")]
    ToolsCall,
}

/// JSON-RPC's error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorCode {
    Parse,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
}

impl ErrorCode {
    fn code(self) -> i32 {
        match self {
            Self::Parse => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
        }
    }
}

/// A JSON-RPC error answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failure {
    code: ErrorCode,
    message: String,
}

/// What the server tells `claude` the tools are for.
const INSTRUCTIONS: &str = "Reads the terminals the user opened beside you in Yetack, once the user shares them with you. Use it when the user mentions a terminal, a command they ran, or its output.";

/// The text a refusal of an unshared terminal adds, which says how the user shares one.
fn how_to_share() -> String {
    format!("The user shares a terminal from its tab's menu in Yetack, with \"{SHARE_LABEL}\".")
}

/// Serves `agent` on stdin and stdout, reading the terminals from the daemon behind `transport`,
/// until stdin ends.
pub async fn serve(transport: Transport, agent: AgentId) -> io::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut server = Server {
        terminals: Daemon::new(transport, agent),
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if let Some(answer) = server.handle(&line).await {
            stdout.write_all(answer.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

pub(crate) struct Server<T> {
    pub(crate) terminals: T,
}

impl<T: Terminals> Server<T> {
    /// The answer to one line, or `None` for a notification or a blank line.
    pub(crate) async fn handle(&mut self, line: &str) -> Option<String> {
        if line.trim().is_empty() {
            return None;
        }
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                let failure = Failure {
                    code: ErrorCode::Parse,
                    message: error.to_string(),
                };
                return Some(failure_answer(&Value::Null, failure));
            }
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // An answer to a request of ours has a result or an error, but the server sends none.
            if message.get(ID_KEY).is_some()
                && message.get(RESULT_KEY).is_none()
                && message.get(ERROR_KEY).is_none()
            {
                let id = message.get(ID_KEY).cloned().unwrap_or_default();
                let failure = Failure {
                    code: ErrorCode::InvalidRequest,
                    message: "a request needs a method".to_owned(),
                };
                return Some(failure_answer(&id, failure));
            }
            return None;
        };
        // A notification, which needs no answer.
        let id = message.get(ID_KEY)?.clone();
        let params = message.get("params").cloned().unwrap_or_default();
        let answer = match method.parse::<Method>() {
            Ok(Method::Initialize) => Ok(initialize(&params)),
            Ok(Method::Ping) => Ok(json!({})),
            Ok(Method::ToolsList) => Ok(json!({ "tools": tools() })),
            Ok(Method::ToolsCall) => self.call(&params).await,
            Err(_) => Err(Failure {
                code: ErrorCode::MethodNotFound,
                message: format!("no method {method}"),
            }),
        };
        Some(match answer {
            Ok(result) => {
                json!({ JSON_RPC_KEY: JSON_RPC, ID_KEY: id, RESULT_KEY: result }).to_string()
            }
            Err(failure) => failure_answer(&id, failure),
        })
    }

    async fn call(&mut self, params: &Value) -> Result<Value, Failure> {
        let name = params
            .get(NAME_KEY)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = params.get("arguments").cloned().unwrap_or_default();
        let outcome = match name {
            LIST_TERMINALS_TOOL => self.list().await,
            READ_TERMINAL_TOOL => self.read(&arguments).await,
            other => {
                return Err(Failure {
                    code: ErrorCode::InvalidParams,
                    message: format!("no tool {other}"),
                });
            }
        };
        let (text, is_error) = match outcome {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error,
        }))
    }

    async fn list(&mut self) -> Result<String, String> {
        let terminals = self.terminals.list().await?;
        if terminals.is_empty() {
            return Ok(format!(
                "The user shares no terminal with you. {}",
                how_to_share()
            ));
        }
        let lines: Vec<String> = terminals.iter().map(describe).collect();
        Ok(lines.join("\n"))
    }

    async fn read(&mut self, arguments: &Value) -> Result<String, String> {
        let lines = match arguments.get(LINES_ARGUMENT) {
            None | Some(Value::Null) => DEFAULT_LINES,
            Some(value) => value
                .as_u64()
                .filter(|&lines| lines > 0)
                .map(|lines| u32::try_from(lines).unwrap_or(u32::MAX).min(MAX_TEXT_LINES))
                .ok_or_else(|| format!("`{LINES_ARGUMENT}` is a number of lines, at least 1."))?,
        };
        let shared = self.terminals.list().await?;
        let terminal = pick(&shared, arguments.get(TERMINAL_ARGUMENT))?;
        // The daemon refuses a terminal that is not shared, or another agent's, in words for the
        // model.
        let text = self.terminals.read(terminal, lines).await?;
        let named = shared
            .iter()
            .find(|info| info.terminal == terminal)
            .map_or_else(|| format!("Terminal {}", terminal.get()), name);
        let header = if text.lines.is_empty() {
            format!("{named} shows no text yet.")
        } else {
            format!(
                "{named}: the last {} of {} lines.",
                text.lines.len(),
                text.total_lines
            )
        };
        Ok(format!("{header}\n\n{}", text.lines.join("\n")))
    }
}

/// The terminal that `read_terminal`'s `terminal` names: an id, as `3` or the menu's `terminal 3`,
/// or the name the user gave a shared terminal's tab, whatever its case.
fn pick(shared: &[TerminalInfo], wanted: Option<&Value>) -> Result<TerminalId, String> {
    let id = |raw: u64| u32::try_from(raw).ok().map(TerminalId::new);
    let missing = || {
        format!(
            "{READ_TERMINAL_TOOL} needs `{TERMINAL_ARGUMENT}`: a terminal's id or name, from {LIST_TERMINALS_TOOL}."
        )
    };
    let text = match wanted {
        Some(Value::Number(number)) => return number.as_u64().and_then(id).ok_or_else(missing),
        Some(Value::String(text)) => text.trim(),
        _ => return Err(missing()),
    };
    let matching: Vec<&TerminalInfo> = shared
        .iter()
        .filter(|info| {
            info.name
                .as_deref()
                .is_some_and(|name| name.to_lowercase() == text.to_lowercase())
        })
        .collect();
    match matching.as_slice() {
        [one] => Ok(one.terminal),
        [] => id_in(text).and_then(id).ok_or_else(|| {
            format!(
                "No terminal shared with you is named \"{text}\". {LIST_TERMINALS_TOOL} lists the ones you can read. {}",
                how_to_share()
            )
        }),
        several => {
            let ids: Vec<String> = several
                .iter()
                .map(|info| info.terminal.get().to_string())
                .collect();
            Err(format!(
                "{} terminals shared with you are named \"{text}\": {}. Name one by its id.",
                several.len(),
                ids.join(", ")
            ))
        }
    }
}

/// The number in "3" or "terminal 3", whatever the word's case.
fn id_in(text: &str) -> Option<u64> {
    let number = match text.get(..TERMINAL_WORD.len()) {
        Some(word) if word.eq_ignore_ascii_case(TERMINAL_WORD) => text
            .get(TERMINAL_WORD.len()..)
            .unwrap_or_default()
            .trim_start(),
        _ => text,
    };
    number.parse().ok()
}

/// "Terminal 2 \"tests\" (zsh)", or "Terminal 2 (zsh)" for a tab the user did not name.
fn name(info: &TerminalInfo) -> String {
    match &info.name {
        Some(name) => format!(
            "Terminal {} \"{name}\" ({})",
            info.terminal.get(),
            info.title
        ),
        None => format!("Terminal {} ({})", info.terminal.get(), info.title),
    }
}

fn describe(info: &TerminalInfo) -> String {
    let state = match info.exited {
        None => "its shell runs".to_owned(),
        Some(exit) => match exit.code {
            Some(code) => format!("its shell ended with code {code}"),
            None => "its shell ended".to_owned(),
        },
    };
    format!("{}: {state}", name(info))
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get(PROTOCOL_VERSION_KEY)
        .and_then(Value::as_str)
        .filter(|version| MCP_VERSIONS.contains(version))
        .unwrap_or(LATEST_MCP_VERSION);
    json!({
        PROTOCOL_VERSION_KEY: version,
        "capabilities": { "tools": {} },
        "serverInfo": { NAME_KEY: MCP_SERVER_NAME, "version": cockpit_daemon::RELEASE_VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    // They read the terminals, and reach nothing outside the daemon.
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            NAME_KEY: LIST_TERMINALS_TOOL,
            "description": "Lists the terminals the user shares with you in Yetack: each one's id, which the tab's tooltip and menu show, the name the user gave the tab, its shell, and whether the shell still runs. A terminal the user did not share is not listed.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only.clone(),
        },
        {
            NAME_KEY: READ_TERMINAL_TOOL,
            "description": "Reads a terminal the user shares with you: the last lines of its history and screen, as plain text. While a full-screen program such as less or vim runs, it reads that program's screen.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    TERMINAL_ARGUMENT: {
                        "type": ["integer", "string"],
                        "description": format!(
                            "The terminal's id, as 3 or \"terminal 3\", or the name the user gave its tab, from {LIST_TERMINALS_TOOL}. A name matches whatever its case."
                        ),
                    },
                    LINES_ARGUMENT: {
                        "type": "integer",
                        "description": format!(
                            "How many of the last lines to read: {DEFAULT_LINES} when not given, at most {MAX_TEXT_LINES}."
                        ),
                        "minimum": 1,
                        "maximum": MAX_TEXT_LINES,
                    },
                },
                "required": [TERMINAL_ARGUMENT],
            },
            "annotations": read_only,
        },
    ])
}

fn failure_answer(id: &Value, failure: Failure) -> String {
    json!({
        JSON_RPC_KEY: JSON_RPC,
        ID_KEY: id,
        ERROR_KEY: { "code": failure.code.code(), "message": failure.message },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::TerminalExit;

    use super::*;
    use crate::daemon::TerminalText;

    /// Agent 1's daemon: it lists `shared`, and reads terminals 2 and 3. It refuses terminal 4,
    /// unshared or another agent's, and knows no terminal 9, in the daemon's words.
    struct FakeDaemon {
        shared: Vec<TerminalInfo>,
        reads: Vec<(TerminalId, u32)>,
    }

    fn info(terminal: u32, name: Option<&str>, exited: Option<TerminalExit>) -> TerminalInfo {
        TerminalInfo {
            terminal: TerminalId::new(terminal),
            title: "zsh".to_owned(),
            exited,
            shared: true,
            name: name.map(str::to_owned),
        }
    }

    impl Terminals for FakeDaemon {
        async fn list(&mut self) -> Result<Vec<TerminalInfo>, String> {
            Ok(self.shared.clone())
        }

        async fn read(&mut self, terminal: TerminalId, lines: u32) -> Result<TerminalText, String> {
            self.reads.push((terminal, lines));
            match terminal.get() {
                2 => Ok(TerminalText {
                    lines: vec!["$ cargo test".to_owned(), "test result: ok".to_owned()],
                    total_lines: 120,
                }),
                3 => Ok(TerminalText {
                    lines: Vec::new(),
                    total_lines: 0,
                }),
                4 => Err(
                    "Terminal 4 is not shared with you: the user can share it from its tab's \
                     menu, with \"Let the agent read it\"."
                        .to_owned(),
                ),
                other => Err(format!("There is no terminal {other}.")),
            }
        }
    }

    /// Shares 2, named "tests", and 3, whose shell ended.
    fn server() -> Server<FakeDaemon> {
        Server {
            terminals: FakeDaemon {
                shared: vec![
                    info(2, Some("tests"), None),
                    info(3, None, Some(TerminalExit { code: Some(1) })),
                ],
                reads: Vec::new(),
            },
        }
    }

    async fn ask(server: &mut Server<FakeDaemon>, message: Value) -> Value {
        let answer = server
            .handle(&message.to_string())
            .await
            .expect("an answer");
        serde_json::from_str(&answer).unwrap()
    }

    async fn call(server: &mut Server<FakeDaemon>, tool: &str, arguments: Value) -> (String, bool) {
        let answer = ask(
            server,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments },
            }),
        )
        .await;
        let result = &answer["result"];
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap(),
        )
    }

    #[tokio::test]
    async fn initialize_answers_the_clients_version_and_the_tools_capability() {
        let mut server = server();

        let answer = ask(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "id": 0,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "claude-code", "version": "2.1.316" },
                },
            }),
        )
        .await;
        let initialized = server
            .handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await;
        let ping = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" }),
        )
        .await;

        assert_eq!(answer["jsonrpc"], "2.0");
        assert_eq!(answer["id"], 0);
        assert_eq!(answer["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(answer["result"]["capabilities"], json!({ "tools": {} }));
        assert_eq!(answer["result"]["serverInfo"]["name"], "yetack");
        assert_eq!(initialized, None, "a notification gets no answer");
        assert_eq!(ping, json!({ "jsonrpc": "2.0", "id": "p", "result": {} }));
    }

    /// A client of a version the server does not know gets the latest one it knows, which the client
    /// then accepts or refuses.
    #[tokio::test]
    async fn initialize_answers_a_version_it_knows() {
        let mut server = server();
        let initialize = |id: u32, version: &str| {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": { "protocolVersion": version, "capabilities": {} },
            })
        };

        let older = ask(&mut server, initialize(1, "2025-03-26")).await;
        let unknown = ask(&mut server, initialize(2, "2099-01-01")).await;
        let unnamed = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 3, "method": "initialize", "params": {} }),
        )
        .await;

        assert_eq!(older["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(unknown["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(unnamed["result"]["protocolVersion"], "2025-11-25");
    }

    #[tokio::test]
    async fn tools_list_names_the_two_tools_and_their_arguments() {
        let mut server = server();

        let answer = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        )
        .await;

        let tools = answer["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(names, ["list_terminals", "read_terminal"]);
        let read = &tools[1]["inputSchema"];
        assert_eq!(read["required"], json!(["terminal"]));
        assert_eq!(
            read["properties"]["terminal"]["type"],
            json!(["integer", "string"])
        );
        assert_eq!(read["properties"]["lines"]["maximum"], 2000);
        assert!(
            tools
                .iter()
                .all(|tool| tool["annotations"]["readOnlyHint"] == true)
        );
    }

    #[tokio::test]
    async fn list_terminals_lists_the_shared_ones_with_their_ids_and_names() {
        let mut server = server();
        let mut none = Server {
            terminals: FakeDaemon {
                shared: Vec::new(),
                reads: Vec::new(),
            },
        };

        let (text, is_error) = call(&mut server, "list_terminals", json!({})).await;
        let (empty, _) = call(&mut none, "list_terminals", json!({})).await;

        assert!(!is_error);
        assert_eq!(
            text,
            "Terminal 2 \"tests\" (zsh): its shell runs\n\
             Terminal 3 (zsh): its shell ended with code 1"
        );
        assert!(
            empty.starts_with("The user shares no terminal with you."),
            "{empty}"
        );
    }

    #[tokio::test]
    async fn read_terminal_takes_an_id_or_a_name_and_names_the_terminal_in_its_header() {
        let mut server = server();

        let (by_id, by_id_error) =
            call(&mut server, "read_terminal", json!({ "terminal": 2 })).await;
        let (by_name, _) = call(
            &mut server,
            "read_terminal",
            json!({ "terminal": "Tests", "lines": 1 }),
        )
        .await;
        let (by_text_id, _) = call(&mut server, "read_terminal", json!({ "terminal": "2" })).await;
        let (by_copied_id, _) = call(
            &mut server,
            "read_terminal",
            json!({ "terminal": "Terminal 2" }),
        )
        .await;
        let (empty, _) = call(
            &mut server,
            "read_terminal",
            json!({ "terminal": 3, "lines": 50_000 }),
        )
        .await;

        assert!(!by_id_error);
        assert_eq!(
            by_id,
            "Terminal 2 \"tests\" (zsh): the last 2 of 120 lines.\n\n$ cargo test\ntest result: ok"
        );
        assert!(
            by_name.starts_with("Terminal 2 \"tests\" (zsh)"),
            "{by_name}"
        );
        assert_eq!(by_text_id, by_id);
        assert_eq!(by_copied_id, by_id, "the text the tab's menu copies");
        assert!(
            empty.starts_with("Terminal 3 (zsh) shows no text yet."),
            "{empty}"
        );
        assert_eq!(
            server.terminals.reads,
            [
                (TerminalId::new(2), DEFAULT_LINES),
                (TerminalId::new(2), 1),
                (TerminalId::new(2), DEFAULT_LINES),
                (TerminalId::new(2), DEFAULT_LINES),
                (TerminalId::new(3), MAX_TEXT_LINES),
            ]
        );
    }

    /// Terminal 4 is not shared, or another agent's, and 9 does not exist: the daemon's refusal
    /// reaches the model as a tool error. A bad argument is refused before any read.
    #[tokio::test]
    async fn read_terminal_refuses_a_terminal_it_may_not_read() {
        let mut server = server();

        let (unshared, unshared_error) =
            call(&mut server, "read_terminal", json!({ "terminal": 4 })).await;
        let (unknown, unknown_error) =
            call(&mut server, "read_terminal", json!({ "terminal": 9 })).await;
        let reads = server.terminals.reads.len();
        let (no_name, no_name_error) =
            call(&mut server, "read_terminal", json!({ "terminal": "build" })).await;
        let (no_id, no_id_error) = call(&mut server, "read_terminal", json!({})).await;
        let (no_lines, no_lines_error) = call(
            &mut server,
            "read_terminal",
            json!({ "terminal": 2, "lines": 0 }),
        )
        .await;

        assert!(unshared_error && unknown_error && no_name_error && no_id_error && no_lines_error);
        assert!(
            unshared.starts_with("Terminal 4 is not shared with you: the user can share it"),
            "{unshared}"
        );
        assert_eq!(unknown, "There is no terminal 9.");
        assert!(
            no_name.starts_with("No terminal shared with you is named \"build\""),
            "{no_name}"
        );
        assert!(no_id.contains("needs `terminal`"), "{no_id}");
        assert!(no_lines.contains("at least 1"), "{no_lines}");
        assert_eq!(
            server.terminals.reads.len(),
            reads,
            "no read past the bad arguments"
        );
    }

    #[tokio::test]
    async fn a_name_that_several_terminals_have_asks_for_an_id() {
        let mut server = Server {
            terminals: FakeDaemon {
                shared: vec![info(2, Some("tests"), None), info(5, Some("TESTS"), None)],
                reads: Vec::new(),
            },
        };

        let (text, is_error) =
            call(&mut server, "read_terminal", json!({ "terminal": "tests" })).await;

        assert!(is_error);
        assert_eq!(
            text,
            "2 terminals shared with you are named \"tests\": 2, 5. Name one by its id."
        );
        assert!(server.terminals.reads.is_empty());
    }

    #[tokio::test]
    async fn bad_lines_and_unknown_methods_get_json_rpc_errors() {
        let mut server = server();

        let garbled = server.handle("{not json").await.unwrap();
        let unknown = ask(
            &mut server,
            json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" }),
        )
        .await;
        let no_tool = ask(
            &mut server,
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": { "name": "write_terminal" },
            }),
        )
        .await;

        let garbled: Value = serde_json::from_str(&garbled).unwrap();
        assert_eq!(garbled["error"]["code"], -32700);
        assert_eq!(garbled["id"], Value::Null);
        assert_eq!(unknown["error"]["code"], -32601);
        assert_eq!(no_tool["error"]["code"], -32602);
        assert_eq!(server.handle("").await, None);
    }

    #[tokio::test]
    async fn an_answer_without_a_method_is_a_bad_request_only_when_it_holds_no_result() {
        let mut server = server();

        let bare = ask(&mut server, json!({ "jsonrpc": "2.0", "id": 8 })).await;
        let result = server
            .handle(r#"{"jsonrpc":"2.0","id":9,"result":{}}"#)
            .await;

        assert_eq!(bare["error"]["code"], -32600);
        assert_eq!(bare["id"], 8);
        assert_eq!(result, None);
    }

    #[test]
    fn method_names_are_pinned() {
        let words: Vec<&str> = Method::VARIANTS
            .iter()
            .map(|&method| method.into())
            .collect();

        assert_eq!(words, ["initialize", "ping", "tools/list", "tools/call"]);
    }
}

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failure is a failed test"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use claude_protocol::{
    ContentBlock, ContextUsage, ControlRequestId, ControlResponse, HookOutcome, ImageSource,
    IncomingRequest, InitializeResponse, McpScope, McpServerState, McpStatus, McpTransport,
    MessageOrigin, OpenWord, OriginKind, OutputLine, ParseError, PermissionBehavior,
    PermissionDestination, PermissionMode, PermissionUpdate, RateLimitStatus, RateLimitType,
    ResultMessage, ResultSubtype, SessionId, SessionState, SystemEvent, TaskId,
    TaskNotificationText, TaskStatus, TerminalReason, ToolUseId, Usage, UserMessage,
};
use serde_json::{Value, json};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn fixture(name: &str) -> OutputLine {
    let text = std::fs::read_to_string(fixture_path(name)).expect("the fixture exists");
    text.parse()
        .unwrap_or_else(|error| panic!("{name} does not parse: {error}"))
}

fn tool_use_id(raw: &str) -> ToolUseId {
    ToolUseId::new(raw.to_owned())
}

fn system_event(name: &str) -> SystemEvent {
    let OutputLine::System(event) = fixture(name) else {
        panic!("{name} is not a system line");
    };
    event
}

fn success_body(name: &str) -> Value {
    let OutputLine::ControlResponse(ControlResponse::Success(success)) = fixture(name) else {
        panic!("{name} is not a successful control response");
    };
    success.response
}

#[test]
fn system_init() {
    let SystemEvent::Init(init) = system_event("system_init.json") else {
        panic!("not an init event");
    };
    assert_eq!(
        init.session_id,
        SessionId::new("00000000-0000-4000-8000-000000000001".to_owned())
    );
    assert_eq!(init.cwd.as_deref(), Some(Path::new("/home/user/project")));
    assert_eq!(init.model.as_deref(), Some("claude-test-model"));
    assert_eq!(
        init.permission_mode,
        Some(OpenWord::Known(PermissionMode::AcceptEdits))
    );
    assert_eq!(init.tools, ["Bash", "Edit", "Read", "TodoWrite"]);
    assert_eq!(init.slash_commands, ["compact", "review-notes"]);
    assert_eq!(init.skills, ["review-notes"]);
    assert_eq!(init.agents, ["general-purpose"]);
    assert_eq!(init.claude_code_version.as_deref(), Some("9.9.9"));
    let statuses: Vec<_> = init
        .mcp_servers
        .iter()
        .map(|server| server.status.clone())
        .collect();
    assert_eq!(
        statuses,
        [
            Some(OpenWord::Known(McpServerState::Connected)),
            Some(OpenWord::Known(McpServerState::NeedsAuth)),
        ]
    );
}

#[test]
fn system_api_retry() {
    let SystemEvent::ApiRetry(retry) = system_event("system_api_retry.json") else {
        panic!("not an api_retry event");
    };
    assert_eq!(retry.attempt, Some(2));
    assert_eq!(retry.max_retries, Some(10));
    assert_eq!(retry.retry_delay, Some(Duration::from_millis(1500)));
    assert_eq!(retry.error_status, Some(529));
    assert_eq!(retry.error.as_deref(), Some("server_error"));
}

#[test]
fn system_task_started() {
    let SystemEvent::TaskStarted(task) = system_event("system_task_started.json") else {
        panic!("not a task_started event");
    };
    assert_eq!(task.task_id, TaskId::new("task-0001".to_owned()));
    assert_eq!(task.description, "Survey the test folder");
    assert_eq!(task.task_type.as_deref(), Some("local_agent"));
    assert_eq!(
        task.tool_use_id,
        Some(tool_use_id("toolu_00000000000000000000000001"))
    );
}

#[test]
fn system_task_progress() {
    let SystemEvent::TaskProgress(task) = system_event("system_task_progress.json") else {
        panic!("not a task_progress event");
    };
    let usage = task.usage.expect("the progress has a usage");
    assert_eq!(usage.total_tokens, 12000);
    assert_eq!(usage.tool_uses, 3);
    assert_eq!(usage.duration, Some(Duration::from_millis(4200)));
    assert_eq!(task.last_tool_name.as_deref(), Some("Read"));
}

#[test]
fn system_task_notification() {
    let SystemEvent::TaskNotification(task) = system_event("system_task_notification.json") else {
        panic!("not a task_notification event");
    };
    assert_eq!(task.task_id, TaskId::new("task-0001".to_owned()));
    assert_eq!(task.status, Some(OpenWord::Known(TaskStatus::Completed)));
    assert_eq!(task.summary, "Found 12 test files");
    assert_eq!(
        task.output_file.as_deref(),
        Some(Path::new("/home/user/.cache/task-0001.output"))
    );
    assert_eq!(
        task.usage.and_then(|usage| usage.duration),
        Some(Duration::from_secs(9))
    );
}

#[test]
fn system_task_updated() {
    let SystemEvent::TaskUpdated(task) = system_event("system_task_updated.json") else {
        panic!("not a task_updated event");
    };
    assert_eq!(task.task_id, TaskId::new("task-0002".to_owned()));
    assert_eq!(task.patch.status, Some(OpenWord::Known(TaskStatus::Killed)));
}

#[test]
fn task_statuses_tell_which_end_a_task() {
    let terminal: Vec<TaskStatus> = [
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Paused,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Stopped,
        TaskStatus::Killed,
    ]
    .into_iter()
    .filter(|status| status.is_terminal())
    .collect();
    assert_eq!(
        terminal,
        [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Stopped,
            TaskStatus::Killed
        ]
    );
}

#[test]
fn system_session_state_changed() {
    let SystemEvent::SessionStateChanged(change) =
        system_event("system_session_state_changed.json")
    else {
        panic!("not a session_state_changed event");
    };
    assert_eq!(change.state, OpenWord::Known(SessionState::RequiresAction));
}

#[test]
fn system_thinking_tokens() {
    let SystemEvent::ThinkingTokens(thinking) = system_event("system_thinking_tokens.json") else {
        panic!("not a thinking_tokens event");
    };
    assert_eq!(thinking.estimated_tokens, 250);
}

#[test]
fn system_hook_started() {
    let SystemEvent::HookStarted(hook) = system_event("system_hook_started.json") else {
        panic!("not a hook_started event");
    };
    assert_eq!(
        hook.hook_id.as_str(),
        "00000000-0000-4000-8000-000000000301"
    );
    assert_eq!(hook.hook_name, "SessionStart:startup");
    assert_eq!(hook.hook_event, "SessionStart");
    assert_eq!(hook.outcome, None);
}

#[test]
fn system_hook_progress() {
    let SystemEvent::HookProgress(hook) = system_event("system_hook_progress.json") else {
        panic!("not a hook_progress event");
    };
    assert_eq!(hook.stdout, "loading notes\n");
}

#[test]
fn system_hook_response() {
    let SystemEvent::HookResponse(hook) = system_event("system_hook_response.json") else {
        panic!("not a hook_response event");
    };
    assert_eq!(hook.exit_code, Some(1));
    assert_eq!(hook.outcome, Some(OpenWord::Known(HookOutcome::Error)));
    assert_eq!(hook.stderr, "notes folder missing\n");
}

#[test]
fn system_unknown_subtype_keeps_the_raw_json() {
    let SystemEvent::Other(raw) = system_event("system_unknown_subtype.json") else {
        panic!("an unknown subtype is not Other");
    };
    assert_eq!(raw["sky"], "clear");
}

#[test]
fn assistant_text() {
    let OutputLine::Assistant(assistant) = fixture("assistant_text.json") else {
        panic!("not an assistant line");
    };
    assert_eq!(assistant.parent_tool_use_id, None);
    assert_eq!(
        assistant.uuid.as_ref().map(|uuid| uuid.as_str()),
        Some("00000000-0000-4000-8000-000000000201")
    );
    assert_eq!(
        assistant.message.model.as_deref(),
        Some("claude-test-model")
    );
    assert_eq!(assistant.message.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(
        assistant.message.usage,
        Some(Usage {
            input_tokens: 3,
            output_tokens: 12,
            cache_creation_input_tokens: 200,
            cache_read_input_tokens: 30000,
        })
    );
    let [ContentBlock::Text(text)] = assistant.message.content.as_slice() else {
        panic!("not one text block");
    };
    assert_eq!(text.text, "I added the changelog entry.");
}

#[test]
fn assistant_thinking() {
    let OutputLine::Assistant(assistant) = fixture("assistant_thinking.json") else {
        panic!("not an assistant line");
    };
    let [ContentBlock::Thinking(thinking)] = assistant.message.content.as_slice() else {
        panic!("not one thinking block");
    };
    assert_eq!(thinking.thinking, "The changelog needs one line.");
}

#[test]
fn assistant_tool_use_in_a_subagent() {
    let OutputLine::Assistant(assistant) = fixture("assistant_tool_use_in_subagent.json") else {
        panic!("not an assistant line");
    };
    assert_eq!(
        assistant.parent_tool_use_id,
        Some(tool_use_id("toolu_00000000000000000000000001"))
    );
    let [
        ContentBlock::ToolUse(tool_use),
        ContentBlock::Unknown(server_tool_use),
    ] = assistant.message.content.as_slice()
    else {
        panic!("not a tool use then an unknown block");
    };
    assert_eq!(tool_use.id, tool_use_id("toolu_00000000000000000000000002"));
    assert_eq!(tool_use.name, "Edit");
    assert_eq!(
        tool_use.input["file_path"],
        "/home/user/project/src/main.rs"
    );
    assert_eq!(server_tool_use["type"], "server_tool_use");
}

#[test]
fn user_replayed_prompt() {
    let OutputLine::User(user) = fixture("user_replayed_prompt.json") else {
        panic!("not a user line");
    };
    assert!(user.is_replay);
    assert_eq!(user.origin, None);
    let [ContentBlock::Text(text)] = user.message.content.as_slice() else {
        panic!("string content is not one text block");
    };
    assert_eq!(text.text, "Add a changelog entry for the parser");
}

#[test]
fn user_task_notification() {
    let OutputLine::User(user) = fixture("user_task_notification.json") else {
        panic!("not a user line");
    };
    assert!(user.is_replay);
    let origin = user.origin.expect("the line has an origin");
    assert_eq!(origin.kind, OpenWord::Known(OriginKind::TaskNotification));
    assert!(!origin.is_human());
    let [ContentBlock::Text(text)] = user.message.content.as_slice() else {
        panic!("string content is not one text block");
    };
    assert_eq!(
        TaskNotificationText::parse_all(&text.text),
        [TaskNotificationText {
            status: Some(OpenWord::Known(TaskStatus::Killed)),
            summary: Some(
                "Task \"Wait 60 seconds, then print finished\" was stopped by the user or by the app connected to this session"
                    .to_owned()
            ),
        }]
    );
}

fn user_line(origin: &str) -> UserMessage {
    let line = format!(
        r#"{{"type":"user","message":{{"role":"user","content":"Hello"}},"origin":{origin}}}"#
    );
    match line.parse::<OutputLine>() {
        Ok(OutputLine::User(user)) => user,
        other => panic!("{line} gave {other:?}"),
    }
}

#[test]
fn an_origin_that_does_not_read_reads_as_none() {
    let human = user_line(r#"{"kind":"human"}"#);
    let new_kind = user_line(r#"{"kind":"scheduler","producer":"cron"}"#);

    assert!(human.origin.as_ref().is_some_and(MessageOrigin::is_human));
    assert_eq!(
        new_kind.origin.map(|origin| origin.kind),
        Some(OpenWord::Unknown("scheduler".to_owned()))
    );
    for odd in ["null", r#""human""#, "{}", r#"{"kind":3}"#, "[]"] {
        assert_eq!(user_line(odd).origin, None, "{odd}");
    }
}

#[test]
fn user_tool_results() {
    let OutputLine::User(user) = fixture("user_tool_results.json") else {
        panic!("not a user line");
    };
    assert!(!user.is_replay);
    assert_eq!(
        user.tool_use_result,
        Some(json!("Error: Not now, ask again after the review."))
    );
    let [
        ContentBlock::ToolResult(denied),
        ContentBlock::ToolResult(screenshot),
    ] = user.message.content.as_slice()
    else {
        panic!("not two tool results");
    };

    assert_eq!(
        denied.tool_use_id,
        tool_use_id("toolu_00000000000000000000000003")
    );
    assert!(denied.is_error);
    let [ContentBlock::Text(reason)] = denied.content.as_slice() else {
        panic!("string content is not one text block");
    };
    assert_eq!(reason.text, "Not now, ask again after the review.");

    assert!(!screenshot.is_error);
    let [
        ContentBlock::Text(caption),
        ContentBlock::Image(image),
        ContentBlock::Unknown(document),
    ] = screenshot.content.as_slice()
    else {
        panic!("not a text, an image and an unknown block");
    };
    assert_eq!(caption.text, "Screenshot attached");
    let ImageSource::Base64(source) = &image.source else {
        panic!("not a base64 image");
    };
    assert_eq!(source.media_type, "image/png");
    assert_eq!(source.data, "iVBORw0KGgo=");
    assert_eq!(document["title"], "notes.pdf");
}

#[test]
fn result_success() {
    let OutputLine::Result(result) = fixture("result_success.json") else {
        panic!("not a result line");
    };
    assert_eq!(result.subtype, OpenWord::Known(ResultSubtype::Success));
    assert!(!result.is_error);
    assert_eq!(
        result.terminal_reason,
        Some(OpenWord::Known(TerminalReason::Completed))
    );
    assert_eq!(
        result.result.as_deref(),
        Some("I added the changelog entry.")
    );
    assert_eq!(result.duration, Some(Duration::from_millis(2900)));
    assert_eq!(result.api_duration, Some(Duration::from_millis(3300)));
    assert_eq!(result.num_turns, 2);
    assert_eq!(result.total_cost_usd, Some(0.0075));
    assert_eq!(result.api_error_status, None);
    assert_eq!(result.usage.map(|usage| usage.output_tokens), Some(185));
    let model = &result.model_usage["claude-test-model"];
    assert_eq!(model.context_window, Some(200000));
    assert_eq!(model.max_output_tokens, Some(64000));
    assert_eq!(model.cost_usd, 0.0075);
    assert_eq!(model.cache_read_input_tokens, 34000);
    let [denial] = result.permission_denials.as_slice() else {
        panic!("not one denial");
    };
    assert_eq!(denial.tool_name, "Bash");
    assert_eq!(denial.tool_input["command"], "rm -r build");
}

#[test]
fn result_interrupted() {
    let OutputLine::Result(result) = fixture("result_interrupted.json") else {
        panic!("not a result line");
    };
    assert_eq!(
        result.subtype,
        OpenWord::Known(ResultSubtype::ErrorDuringExecution)
    );
    assert!(result.is_error);
    assert_eq!(
        result.terminal_reason,
        Some(OpenWord::Known(TerminalReason::AbortedStreaming))
    );
    assert_eq!(result.errors, ["the turn was interrupted"]);
    assert!(result.model_usage.is_empty());
}

fn result_line(line: &str) -> ResultMessage {
    match line.parse::<OutputLine>() {
        Ok(OutputLine::Result(result)) => result,
        other => panic!("{line} gave {other:?}"),
    }
}

#[test]
fn a_result_line_reads_a_malformed_optional_field_as_absent() {
    let status = result_line(
        r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":"529","num_turns":3,"session_id":"00000000-0000-4000-8000-000000000001"}"#,
    );
    let duration = result_line(
        r#"{"type":"result","subtype":"success","duration_ms":-5,"duration_api_ms":1200,"session_id":"00000000-0000-4000-8000-000000000001"}"#,
    );
    let errors = result_line(
        r#"{"type":"result","subtype":"error_during_execution","errors":null,"terminal_reason":"aborted_tools","session_id":"00000000-0000-4000-8000-000000000001"}"#,
    );

    assert_eq!(status.api_error_status, None);
    assert!(status.is_error);
    assert_eq!(status.num_turns, 3);
    assert_eq!(duration.duration, None);
    assert_eq!(duration.api_duration, Some(Duration::from_millis(1200)));
    assert!(errors.errors.is_empty());
    assert_eq!(
        errors.terminal_reason,
        Some(OpenWord::Known(TerminalReason::AbortedTools))
    );
    assert_eq!(
        errors.session_id,
        Some(SessionId::new(
            "00000000-0000-4000-8000-000000000001".to_owned()
        ))
    );
}

#[test]
fn a_result_line_reads_a_bad_value_inside_a_field_as_no_field() {
    let result = result_line(
        r#"{"type":"result","subtype":"success","usage":{"output_tokens":"many"},"modelUsage":null,"permission_denials":[{"tool_name":"Bash"}],"total_cost_usd":0.5}"#,
    );

    assert_eq!(result.usage, None);
    assert!(result.model_usage.is_empty());
    assert!(result.permission_denials.is_empty());
    assert_eq!(result.total_cost_usd, Some(0.5));
}

#[test]
fn a_result_line_with_a_malformed_subtype_or_session_id_is_an_error() {
    for line in [
        r#"{"type":"result","subtype":7,"session_id":"00000000-0000-4000-8000-000000000001"}"#,
        r#"{"type":"result","subtype":"success","session_id":42}"#,
    ] {
        match line.parse::<OutputLine>() {
            Err(ParseError::MalformedLine { kind, .. }) => assert_eq!(kind, "result", "{line}"),
            other => panic!("{line} gave {other:?}"),
        }
    }
}

#[test]
fn stream_event() {
    let OutputLine::StreamEvent(stream) = fixture("stream_event.json") else {
        panic!("not a stream_event line");
    };
    assert_eq!(stream.event["delta"]["text"], "Hel");
    assert_eq!(stream.parent_tool_use_id, None);
}

#[test]
fn rate_limit_event() {
    let OutputLine::RateLimitEvent(event) = fixture("rate_limit_event.json") else {
        panic!("not a rate_limit_event line");
    };
    let info = event.rate_limit_info;
    let reset = SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000);
    assert_eq!(
        info.status,
        OpenWord::Known(RateLimitStatus::AllowedWarning)
    );
    assert_eq!(
        info.rate_limit_type,
        Some(OpenWord::Known(RateLimitType::SevenDay))
    );
    assert_eq!(info.resets_at, Some(reset));
    assert_eq!(info.utilization, None);
    assert_eq!(
        info.overage_status,
        Some(OpenWord::Known(RateLimitStatus::Rejected))
    );
    assert_eq!(
        info.overage_disabled_reason.as_deref(),
        Some("org_level_disabled")
    );

    let seven_day = &info.unified_windows[&OpenWord::Known(RateLimitType::SevenDay)];
    assert_eq!(seven_day.utilization, Some(0.82));
    assert_eq!(seven_day.resets_at, Some(reset));
    assert_eq!(
        seven_day.status,
        Some(OpenWord::Known(RateLimitStatus::AllowedWarning))
    );
    let five_hour = &info.unified_windows[&OpenWord::Known(RateLimitType::FiveHour)];
    assert_eq!(
        five_hour.resets_at,
        Some(SystemTime::UNIX_EPOCH + Duration::from_millis(1_800_000_000_500))
    );
    let future_window = &info.unified_windows[&OpenWord::Unknown("ten_year".to_owned())];
    assert_eq!(future_window.utilization, Some(0.01));
}

#[test]
fn control_request_can_use_tool() {
    let OutputLine::ControlRequest(request) = fixture("control_request_can_use_tool.json") else {
        panic!("not a control_request line");
    };
    assert_eq!(
        request.request_id,
        ControlRequestId::new("00000000-0000-4000-8000-000000000401".to_owned())
    );
    let IncomingRequest::CanUseTool(can_use_tool) = request.request else {
        panic!("not a can_use_tool request");
    };
    assert_eq!(can_use_tool.tool_name, "Bash");
    assert_eq!(can_use_tool.display_name.as_deref(), Some("Run command"));
    assert_eq!(
        can_use_tool.input["command"],
        "rm /home/user/other/notes.txt"
    );
    assert_eq!(
        can_use_tool.tool_use_id,
        tool_use_id("toolu_00000000000000000000000005")
    );
    assert_eq!(
        can_use_tool.title.as_deref(),
        Some("Claude wants to run rm /home/user/other/notes.txt")
    );
    assert_eq!(
        can_use_tool.description.as_deref(),
        Some("Delete the notes")
    );
    assert_eq!(
        can_use_tool.decision_reason.as_deref(),
        Some("The command writes outside the project")
    );
    assert_eq!(
        can_use_tool.blocked_path.as_deref(),
        Some(Path::new("/home/user/other/notes.txt"))
    );

    let [
        PermissionUpdate::AddRules(rules),
        PermissionUpdate::AddDirectories(directories),
        PermissionUpdate::SetMode(mode),
        PermissionUpdate::Unknown(future),
    ] = can_use_tool.permission_suggestions.as_slice()
    else {
        panic!("not the four suggestions");
    };
    let [rule] = rules.rules.as_slice() else {
        panic!("not one rule");
    };
    assert_eq!(rule.tool_name, "Bash");
    assert_eq!(
        rule.rule_content.as_deref(),
        Some("rm /home/user/other/notes.txt")
    );
    assert_eq!(
        rules.behavior,
        Some(OpenWord::Known(PermissionBehavior::Allow))
    );
    assert_eq!(
        rules.destination,
        Some(OpenWord::Known(PermissionDestination::LocalSettings))
    );
    assert_eq!(directories.directories, [Path::new("/home/user/other")]);
    assert_eq!(
        directories.destination,
        Some(OpenWord::Known(PermissionDestination::Session))
    );
    assert_eq!(
        mode.mode,
        Some(OpenWord::Known(PermissionMode::AcceptEdits))
    );
    assert_eq!(future["type"], "grantEverything");
}

#[test]
fn suggestions_echo_back_as_claude_code_wrote_them() {
    let path = fixture_path("control_request_can_use_tool.json");
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let OutputLine::ControlRequest(request) = fixture("control_request_can_use_tool.json") else {
        panic!("not a control_request line");
    };
    let IncomingRequest::CanUseTool(can_use_tool) = request.request else {
        panic!("not a can_use_tool request");
    };
    let echoed = serde_json::to_value(&can_use_tool.permission_suggestions).unwrap();
    assert_eq!(echoed, raw["request"]["permission_suggestions"]);
}

#[test]
fn control_request_with_null_suggestions() {
    let OutputLine::ControlRequest(request) = fixture("control_request_ask_user_question.json")
    else {
        panic!("not a control_request line");
    };
    let IncomingRequest::CanUseTool(can_use_tool) = request.request else {
        panic!("not a can_use_tool request");
    };
    assert_eq!(can_use_tool.tool_name, "AskUserQuestion");
    assert!(can_use_tool.permission_suggestions.is_empty());
    assert_eq!(can_use_tool.blocked_path, None);
    assert_eq!(can_use_tool.title, None);
}

#[test]
fn control_request_unknown_subtype_keeps_the_raw_request() {
    let OutputLine::ControlRequest(request) = fixture("control_request_unknown_subtype.json")
    else {
        panic!("not a control_request line");
    };
    assert_eq!(
        request.request_id.as_str(),
        "00000000-0000-4000-8000-000000000403"
    );
    let IncomingRequest::Other(raw) = request.request else {
        panic!("an unknown subtype is not Other");
    };
    assert_eq!(raw["callback_id"], "hook_0");
}

#[test]
fn control_response_initialize() {
    let body = success_body("control_response_initialize.json");
    let initialize = InitializeResponse::try_from(&body).unwrap();
    let commands: Vec<(&str, &str, &str)> = initialize
        .commands
        .iter()
        .map(|command| {
            (
                command.name.as_str(),
                command.description.as_str(),
                command.argument_hint.as_str(),
            )
        })
        .collect();
    assert_eq!(
        commands,
        [
            ("compact", "Summarize the conversation", "<instructions>"),
            ("review-notes", "Review the notes folder", ""),
            ("plain", "", ""),
        ]
    );
}

#[test]
fn control_response_context_usage() {
    let body = success_body("control_response_context_usage.json");
    let usage = ContextUsage::try_from(&body).unwrap();
    assert_eq!(usage.total_tokens, 35400);
    assert_eq!(usage.max_tokens, 167000);
    assert_eq!(usage.raw_max_tokens, Some(200000));
    assert_eq!(usage.percentage, Some(17.7));
    assert_eq!(usage.model.as_deref(), Some("claude-test-model"));
    assert!(usage.is_auto_compact_enabled);
    assert_eq!(usage.auto_compact_threshold, Some(160000));
    let categories: Vec<(&str, u32, bool)> = usage
        .categories
        .iter()
        .map(|category| {
            (
                category.name.as_str(),
                category.tokens,
                category.is_deferred,
            )
        })
        .collect();
    assert_eq!(
        categories,
        [
            ("System prompt", 3500, false),
            ("MCP tools", 900, true),
            ("Messages", 31000, false)
        ]
    );
}

#[test]
fn control_response_mcp_status() {
    let body = success_body("control_response_mcp_status.json");
    let status = McpStatus::try_from(&body).unwrap();
    let [notes, tickets, legacy] = status.mcp_servers.as_slice() else {
        panic!("not three servers");
    };

    assert_eq!(notes.name, "notes");
    assert_eq!(notes.status, OpenWord::Known(McpServerState::Connected));
    assert_eq!(notes.scope, Some(OpenWord::Known(McpScope::User)));
    let info = notes
        .server_info
        .as_ref()
        .expect("a connected server has its info");
    assert_eq!(
        (info.name.as_str(), info.version.as_str()),
        ("notes-server", "1.2.0")
    );
    let config = notes.config.as_ref().expect("the server has a config");
    assert_eq!(config.transport, Some(OpenWord::Known(McpTransport::Stdio)));
    assert_eq!(config.command.as_deref(), Some("notes-mcp"));
    assert_eq!(config.args, ["--root", "/home/user/notes"]);
    let [read_note, delete_note, list_notes] = notes.tools.as_slice() else {
        panic!("not three tools");
    };
    assert_eq!(read_note.description.as_deref(), Some("Reads a note"));
    assert_eq!(read_note.annotations.read_only, Some(true));
    assert_eq!(read_note.annotations.open_world, Some(false));
    assert_eq!(read_note.annotations.destructive, None);
    assert_eq!(delete_note.annotations.destructive, Some(true));
    assert_eq!(list_notes.annotations.read_only, None);

    assert_eq!(tickets.status, OpenWord::Known(McpServerState::Failed));
    assert_eq!(tickets.error.as_deref(), Some("connection refused"));
    assert_eq!(tickets.scope, Some(OpenWord::Known(McpScope::Project)));
    let config = tickets.config.as_ref().expect("the server has a config");
    assert_eq!(config.transport, Some(OpenWord::Known(McpTransport::Http)));
    assert_eq!(
        config.url.as_deref(),
        Some("https://tickets.example.com/mcp")
    );
    assert!(tickets.tools.is_empty());

    assert_eq!(legacy.status, OpenWord::Unknown("hibernating".to_owned()));
    assert_eq!(legacy.scope, Some(OpenWord::Unknown("galaxy".to_owned())));
    assert_eq!(
        legacy
            .config
            .as_ref()
            .and_then(|config| config.transport.clone()),
        None
    );
}

#[test]
fn control_response_interrupt() {
    let OutputLine::ControlResponse(response) = fixture("control_response_interrupt.json") else {
        panic!("not a control_response line");
    };
    assert_eq!(
        response.request_id().map(ControlRequestId::as_str),
        Some("cockpit-5")
    );
    let ControlResponse::Success(success) = response else {
        panic!("not a success");
    };
    assert_eq!(success.response, json!({ "still_queued": [] }));
}

#[test]
fn control_response_error() {
    let OutputLine::ControlResponse(ControlResponse::Error(error)) =
        fixture("control_response_error.json")
    else {
        panic!("not an error control response");
    };
    assert_eq!(error.request_id.as_str(), "cockpit-4");
    assert_eq!(error.error, "Server 'tickets' not found");
}

#[test]
fn control_response_unknown_subtype_keeps_the_raw_response() {
    let line =
        r#"{"type":"control_response","response":{"subtype":"partial","request_id":"cockpit-6"}}"#;
    let Ok(OutputLine::ControlResponse(ControlResponse::Unknown(raw))) = line.parse() else {
        panic!("an unknown subtype is not Unknown");
    };
    assert_eq!(raw["request_id"], "cockpit-6");
}

#[test]
fn control_cancel_request() {
    let OutputLine::ControlCancelRequest(cancel) = fixture("control_cancel_request.json") else {
        panic!("not a control_cancel_request line");
    };
    assert_eq!(
        cancel.request_id.as_str(),
        "00000000-0000-4000-8000-000000000401"
    );
}

#[test]
fn unknown_type_keeps_the_raw_json() {
    let OutputLine::Unknown(raw) = fixture("unknown_type.json") else {
        panic!("an unknown type is not Unknown");
    };
    assert_eq!(raw["type"], "keep_alive");
}

#[test]
fn json_without_a_type_is_unknown() {
    for line in ["[1, 2]", "42", r#"{"subtype":"init"}"#, r#"{"type":7}"#] {
        let parsed: OutputLine = line.parse().unwrap();
        assert!(
            matches!(parsed, OutputLine::Unknown(_)),
            "{line} is not Unknown"
        );
    }
}

#[test]
fn a_line_that_is_not_json_is_an_error() {
    let error = "exit 1".parse::<OutputLine>().unwrap_err();
    assert!(matches!(error, ParseError::NotJson(_)), "{error}");
}

#[test]
fn a_known_type_with_a_malformed_required_field_is_an_error() {
    let cases = [
        (r#"{"type":"assistant"}"#, "assistant"),
        (
            r#"{"type":"assistant","message":{"content":"text"}}"#,
            "assistant",
        ),
        (
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
            "user",
        ),
        (
            r#"{"type":"system","subtype":"init","model":"claude-test-model"}"#,
            "init",
        ),
        (
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":"many"}"#,
            "thinking_tokens",
        ),
        (r#"{"type":"result","is_error":false}"#, "result"),
        (
            r#"{"type":"rate_limit_event","rate_limit_info":{}}"#,
            "rate_limit_event",
        ),
        (
            r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#,
            "control_request",
        ),
        (
            r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash"}}"#,
            "can_use_tool",
        ),
        (
            r#"{"type":"control_response","response":{"subtype":"success"}}"#,
            "control_response",
        ),
    ];
    for (line, expected_kind) in cases {
        match line.parse::<OutputLine>() {
            Err(ParseError::MalformedLine { kind, .. }) => {
                assert_eq!(kind, expected_kind, "{line}")
            }
            other => panic!("{line} gave {other:?}"),
        }
    }
}

#[test]
fn a_malformed_control_request_keeps_its_id() {
    // Claude Code waits for an answer, so the caller answers this id with an error.
    let line = r#"{"type":"control_request","request_id":"r7","request":{"subtype":"can_use_tool","tool_name":"Bash"}}"#;

    let error = line.parse::<OutputLine>().unwrap_err();

    let ParseError::MalformedLine {
        kind, request_id, ..
    } = error
    else {
        panic!("{line} gave {error:?}");
    };
    assert_eq!(kind, "can_use_tool");
    assert_eq!(request_id, Some(ControlRequestId::new("r7".to_owned())));
}

#[test]
fn another_malformed_line_has_no_request_id() {
    let error = r#"{"type":"assistant"}"#.parse::<OutputLine>().unwrap_err();

    assert!(
        matches!(
            error,
            ParseError::MalformedLine {
                request_id: None,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_parse_error_says_what_is_wrong() {
    let error = r#"{"type":"assistant"}"#.parse::<OutputLine>().unwrap_err();
    assert_eq!(
        error.to_string(),
        "malformed `assistant` line: missing field `message`"
    );
}

#[test]
fn a_malformed_response_body_is_an_error() {
    let error = ContextUsage::try_from(&json!({ "totalTokens": "a lot" })).unwrap_err();
    assert!(
        matches!(
            error,
            ParseError::MalformedResponse {
                body: "context usage",
                ..
            }
        ),
        "{error}"
    );
}

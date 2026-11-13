#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failure is a failed test"
)]

use claude_protocol::{
    HookOutcome, InterruptNotice, McpScope, McpServerState, McpTransport, OpenWord, OriginKind,
    PermissionBehavior, PermissionDestination, PermissionMode, RateLimitStatus, RateLimitType,
    ResetTrigger, ResultSubtype, SessionState, TaskStatus, TerminalReason, TodoStatus, ToolName,
};
use strum::VariantArray;

fn words<W: VariantArray + Copy + Into<&'static str>>() -> Vec<&'static str> {
    W::VARIANTS.iter().map(|&word| word.into()).collect()
}

#[test]
fn permission_modes_are_pinned() {
    assert_eq!(
        words::<PermissionMode>(),
        [
            "default",
            "acceptEdits",
            "plan",
            "bypassPermissions",
            "dontAsk",
            "auto"
        ]
    );
}

#[test]
fn permission_modes_serialize_as_their_words() {
    for &mode in PermissionMode::VARIANTS {
        let word: &str = mode.into();
        assert_eq!(serde_json::to_value(mode).unwrap(), word);
    }
}

#[test]
fn permission_update_words_are_pinned() {
    assert_eq!(words::<PermissionBehavior>(), ["allow", "deny", "ask"]);
    assert_eq!(
        words::<PermissionDestination>(),
        [
            "userSettings",
            "projectSettings",
            "localSettings",
            "session"
        ]
    );
}

#[test]
fn rate_limit_words_are_pinned() {
    assert_eq!(
        words::<RateLimitStatus>(),
        ["allowed", "allowed_warning", "rejected"]
    );
    assert_eq!(
        words::<RateLimitType>(),
        [
            "five_hour",
            "seven_day",
            "seven_day_opus",
            "seven_day_sonnet",
            "overage"
        ]
    );
}

#[test]
fn mcp_words_are_pinned() {
    assert_eq!(
        words::<McpServerState>(),
        ["connected", "failed", "needs-auth", "pending", "disabled"]
    );
    assert_eq!(
        words::<McpTransport>(),
        ["stdio", "sse", "http", "sdk", "claudeai-proxy"]
    );
    assert_eq!(
        words::<McpScope>(),
        ["local", "user", "project", "claudeai", "managed"]
    );
}

#[test]
fn task_and_session_words_are_pinned() {
    assert_eq!(
        words::<TaskStatus>(),
        [
            "pending",
            "running",
            "paused",
            "completed",
            "failed",
            "stopped",
            "killed"
        ]
    );
    assert_eq!(
        words::<SessionState>(),
        ["idle", "running", "requires_action"]
    );
    assert_eq!(words::<HookOutcome>(), ["success", "error", "cancelled"]);
}

#[test]
fn origin_kinds_are_pinned() {
    assert_eq!(
        words::<OriginKind>(),
        ["human", "task-notification", "peer"]
    );
}

#[test]
fn interrupt_notices_are_pinned() {
    assert_eq!(
        words::<InterruptNotice>(),
        [
            "[Request interrupted by user]",
            "[Request interrupted by user for tool use]"
        ]
    );
}

#[test]
fn reset_triggers_are_pinned() {
    assert_eq!(words::<ResetTrigger>(), ["clear"]);
}

#[test]
fn result_words_are_pinned() {
    assert_eq!(
        words::<ResultSubtype>(),
        [
            "success",
            "error_max_turns",
            "error_during_execution",
            "error_max_budget_usd",
            "error_max_structured_output_retries",
        ]
    );
    assert_eq!(
        words::<TerminalReason>(),
        [
            "completed",
            "max_turns",
            "aborted_streaming",
            "aborted_tools"
        ]
    );
}

#[test]
fn tool_words_are_pinned() {
    assert_eq!(
        words::<TodoStatus>(),
        ["pending", "in_progress", "completed"]
    );
    assert_eq!(
        words::<ToolName>(),
        [
            "TodoWrite",
            "AskUserQuestion",
            "ExitPlanMode",
            "Edit",
            "Write",
            "Bash",
            "Read",
            "Skill",
            "Agent",
            "Task"
        ]
    );
}

#[test]
fn an_open_word_keeps_an_unknown_word() {
    let known: OpenWord<PermissionMode> = serde_json::from_str("\"plan\"").unwrap();
    assert_eq!(known, OpenWord::Known(PermissionMode::Plan));
    let unknown: OpenWord<PermissionMode> = serde_json::from_str("\"yolo\"").unwrap();
    assert_eq!(unknown, OpenWord::Unknown("yolo".to_owned()));
    assert_eq!(unknown.as_str(), "yolo");
    assert_eq!(serde_json::to_string(&unknown).unwrap(), "\"yolo\"");
    assert_eq!(serde_json::to_string(&known).unwrap(), "\"plan\"");
}

#[test]
fn an_open_word_is_still_a_string() {
    let error = serde_json::from_str::<OpenWord<PermissionMode>>("3").unwrap_err();
    assert!(error.is_data(), "{error}");
}

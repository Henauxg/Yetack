#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failure is a failed test"
)]

use claude_protocol::{
    Base64Image, ContentBlock, ControlRequest, ControlRequestId, ImageBlock, ImageSource,
    InputLine, OpenWord, PermissionBehavior, PermissionDecision, PermissionDestination,
    PermissionMode, PermissionRule, PermissionUpdate, QuestionAnswer, RulesUpdate, TaskId,
    TextBlock,
};
use serde_json::{Map, Value, json};

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(object) = value else {
        panic!("not an object");
    };
    object
}

fn request_id(raw: &str) -> ControlRequestId {
    ControlRequestId::new(raw.to_owned())
}

/// The written text without its final `\n`, which must be its only line break.
fn line(input: &InputLine) -> String {
    let text = input.to_line().unwrap();
    let Some(json) = text.strip_suffix('\n') else {
        panic!("{text} does not end with a line break");
    };
    assert!(!json.contains('\n'), "{text} holds more than one line");
    json.to_owned()
}

#[test]
fn a_line_is_one_line_of_json() {
    let text = InputLine::prompt("first\nsecond".to_owned())
        .to_line()
        .unwrap();
    assert_eq!(text.matches('\n').count(), 1);
    assert!(text.ends_with('\n'));
}

#[test]
fn text_prompt() {
    assert_eq!(
        line(&InputLine::prompt("Fix the failing test".to_owned())),
        r#"{"type":"user","message":{"role":"user","content":"Fix the failing test"}}"#
    );
}

#[test]
fn prompt_with_an_image() {
    let image = ContentBlock::Image(ImageBlock {
        source: ImageSource::Base64(Base64Image {
            media_type: "image/png".to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
        }),
    });
    let text = ContentBlock::Text(TextBlock {
        text: "What is on this screen?".to_owned(),
    });
    assert_eq!(
        line(&InputLine::prompt_blocks(vec![image, text])),
        r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}},{"type":"text","text":"What is on this screen?"}]}}"#
    );
}

#[test]
fn control_requests() {
    let cases = [
        (ControlRequest::Initialize, r#"{"subtype":"initialize"}"#),
        (ControlRequest::Interrupt, r#"{"subtype":"interrupt"}"#),
        (
            ControlRequest::SetPermissionMode {
                mode: PermissionMode::AcceptEdits,
            },
            r#"{"subtype":"set_permission_mode","mode":"acceptEdits"}"#,
        ),
        (
            ControlRequest::SetModel {
                model: Some("claude-test-model".to_owned()),
            },
            r#"{"subtype":"set_model","model":"claude-test-model"}"#,
        ),
        (
            ControlRequest::SetModel { model: None },
            r#"{"subtype":"set_model","model":null}"#,
        ),
        (
            ControlRequest::GetContextUsage,
            r#"{"subtype":"get_context_usage"}"#,
        ),
        (ControlRequest::GetSettings, r#"{"subtype":"get_settings"}"#),
        (ControlRequest::McpStatus, r#"{"subtype":"mcp_status"}"#),
        (
            ControlRequest::McpReconnect {
                server_name: "notes".to_owned(),
            },
            r#"{"subtype":"mcp_reconnect","serverName":"notes"}"#,
        ),
        (
            ControlRequest::McpToggle {
                server_name: "notes".to_owned(),
                enabled: false,
            },
            r#"{"subtype":"mcp_toggle","serverName":"notes","enabled":false}"#,
        ),
        (
            ControlRequest::McpAuthenticate {
                server_name: "notes".to_owned(),
            },
            r#"{"subtype":"mcp_authenticate","serverName":"notes"}"#,
        ),
        (
            ControlRequest::McpOauthCallbackUrl {
                server_name: "notes".to_owned(),
                callback_url: "http://localhost:50123/callback?code=c1&state=s1".to_owned(),
            },
            r#"{"subtype":"mcp_oauth_callback_url","serverName":"notes","callbackUrl":"http://localhost:50123/callback?code=c1&state=s1"}"#,
        ),
        (
            ControlRequest::StopTask {
                task_id: TaskId::new("task-0001".to_owned()),
            },
            r#"{"subtype":"stop_task","task_id":"task-0001"}"#,
        ),
        (
            ControlRequest::ReloadSkills,
            r#"{"subtype":"reload_skills"}"#,
        ),
        (
            ControlRequest::ReloadPlugins,
            r#"{"subtype":"reload_plugins"}"#,
        ),
    ];
    for (request, expected_request) in cases {
        let expected = format!(
            r#"{{"type":"control_request","request_id":"cockpit-7","request":{expected_request}}}"#
        );
        assert_eq!(
            line(&InputLine::control_request(
                request_id("cockpit-7"),
                request
            )),
            expected
        );
    }
}

#[test]
fn allow() {
    let decision = PermissionDecision::allow(object(json!({ "command": "cargo test" })));
    assert_eq!(
        line(&InputLine::permission(request_id("req-1"), decision)),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"allow","updatedInput":{"command":"cargo test"}}}}"#
    );
}

#[test]
fn allow_and_remember() {
    let always = PermissionUpdate::AddRules(RulesUpdate {
        rules: vec![PermissionRule {
            tool_name: "Bash".to_owned(),
            rule_content: Some("cargo test".to_owned()),
        }],
        behavior: Some(OpenWord::Known(PermissionBehavior::Allow)),
        destination: Some(OpenWord::Known(PermissionDestination::LocalSettings)),
    });
    let decision = PermissionDecision::Allow {
        updated_input: object(json!({ "command": "cargo test" })),
        updated_permissions: vec![always],
    };
    assert_eq!(
        line(&InputLine::permission(request_id("req-2"), decision)),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-2","response":{"behavior":"allow","updatedInput":{"command":"cargo test"},"updatedPermissions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"cargo test"}],"behavior":"allow","destination":"localSettings"}]}}}"#
    );
}

#[test]
fn deny() {
    let decision = PermissionDecision::deny("Not now, ask again after the review.".to_owned());
    assert_eq!(
        line(&InputLine::permission(request_id("req-3"), decision)),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-3","response":{"behavior":"deny","message":"Not now, ask again after the review."}}}"#
    );
}

#[test]
fn deny_and_interrupt() {
    let decision = PermissionDecision::Deny {
        message: "Stop here".to_owned(),
        interrupt: true,
    };
    assert_eq!(
        line(&InputLine::permission(request_id("req-4"), decision)),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-4","response":{"behavior":"deny","message":"Stop here","interrupt":true}}}"#
    );
}

#[test]
fn control_error() {
    assert_eq!(
        line(&InputLine::control_error(
            request_id("req-5"),
            "unsupported request".to_owned()
        )),
        r#"{"type":"control_response","response":{"subtype":"error","request_id":"req-5","error":"unsupported request"}}"#
    );
}

#[test]
fn answer_questions_adds_the_answers_to_the_input() {
    let input = json!({
        "questions": [
            {
                "question": "Which format should the report use?",
                "header": "Format",
                "options": [{ "label": "Summary", "description": "A short overview" }],
                "multiSelect": false
            },
            {
                "question": "Which sections should it include?",
                "header": "Sections",
                "options": [{ "label": "Intro", "description": "The context" }],
                "multiSelect": true
            },
            {
                "question": "Who reads it?",
                "header": "Readers",
                "options": [{ "label": "The team", "description": "Engineers" }],
                "multiSelect": false
            }
        ]
    });
    let answers = vec![
        QuestionAnswer {
            question: "Which format should the report use?".to_owned(),
            choices: vec!["Summary".to_owned()],
        },
        QuestionAnswer {
            question: "Which sections should it include?".to_owned(),
            choices: vec!["Intro".to_owned(), "Risks".to_owned()],
        },
        QuestionAnswer {
            question: "Who reads it?".to_owned(),
            choices: vec!["The board, on Monday".to_owned()],
        },
    ];
    let decision = PermissionDecision::answer_questions(object(input.clone()), answers);
    let text = line(&InputLine::permission(request_id("req-6"), decision));

    let mut expected_input = input;
    expected_input["answers"] = json!({
        "Which format should the report use?": "Summary",
        "Which sections should it include?": "Intro, Risks",
        "Who reads it?": "The board, on Monday"
    });
    let expected = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": "req-6",
            "response": { "behavior": "allow", "updatedInput": expected_input }
        }
    });
    let written: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written, expected);
}

#[test]
fn approve_a_plan() {
    let input = object(json!({ "plan": "1. Fix the bug" }));
    assert_eq!(
        line(&InputLine::permission(
            request_id("req-7"),
            PermissionDecision::allow(input)
        )),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-7","response":{"behavior":"allow","updatedInput":{"plan":"1. Fix the bug"}}}}"#
    );
}

#[test]
fn keep_planning() {
    let decision = PermissionDecision::deny("Keep planning: cover the migration too.".to_owned());
    assert_eq!(
        line(&InputLine::permission(request_id("req-8"), decision)),
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-8","response":{"behavior":"deny","message":"Keep planning: cover the migration too."}}}"#
    );
}

#[test]
fn a_redacted_sign_in_callback_loses_its_authorization_code() {
    let callback = InputLine::control_request(
        request_id("cockpit-9"),
        ControlRequest::McpOauthCallbackUrl {
            server_name: "notes".to_owned(),
            callback_url: "http://localhost:50123/callback?code=c1&state=s1".to_owned(),
        },
    );

    let redacted = callback.redacted().map(|redacted| line(&redacted));

    assert_eq!(
        redacted.as_deref(),
        Some(
            r#"{"type":"control_request","request_id":"cockpit-9","request":{"subtype":"mcp_oauth_callback_url","serverName":"notes","callbackUrl":"http://localhost:50123/callback?(left out)"}}"#
        )
    );
}

#[test]
fn a_line_without_a_secret_has_no_redacted_form() {
    let prompt = InputLine::prompt("Fix the login form".to_owned());

    assert_eq!(prompt.redacted(), None);
}

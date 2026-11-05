#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code: a failure is a failed test"
)]

use std::path::Path;

use claude_protocol::{
    BashInput, EditInput, ExitPlanModeInput, OpenWord, ParseError, ReadInput, SkillInput,
    TodoStatus, ToolInput, ToolName, WriteInput,
};
use serde_json::{Map, Value, json};

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(object) = value else {
        panic!("not an object");
    };
    object
}

fn parse(tool_name: &str, input: Value) -> ToolInput {
    ToolInput::parse(tool_name, &object(input)).unwrap()
}

#[test]
fn todo_write() {
    let input = json!({
        "todos": [
            { "content": "Write the parser", "status": "completed", "activeForm": "Writing the parser" },
            { "content": "Test the parser", "status": "in_progress", "activeForm": "Testing the parser" },
            { "content": "Ship it", "status": "blocked" }
        ]
    });
    let ToolInput::TodoWrite(todo_write) = parse("TodoWrite", input) else {
        panic!("not a TodoWrite input");
    };
    let todos: Vec<(&str, OpenWord<TodoStatus>, Option<&str>)> = todo_write
        .todos
        .iter()
        .map(|todo| {
            (
                todo.content.as_str(),
                todo.status.clone(),
                todo.active_form.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        todos,
        [
            (
                "Write the parser",
                OpenWord::Known(TodoStatus::Completed),
                Some("Writing the parser")
            ),
            (
                "Test the parser",
                OpenWord::Known(TodoStatus::InProgress),
                Some("Testing the parser")
            ),
            ("Ship it", OpenWord::Unknown("blocked".to_owned()), None),
        ]
    );
}

#[test]
fn ask_user_question() {
    let input = json!({
        "questions": [
            {
                "question": "Which format should the report use?",
                "header": "Format",
                "options": [
                    { "label": "Summary", "description": "A short overview", "preview": "<div></div>" },
                    { "label": "Detailed", "description": "Every finding" }
                ],
                "multiSelect": false
            },
            {
                "question": "Which sections should it include?",
                "header": "Sections",
                "options": [{ "label": "Intro", "description": "The context" }],
                "multiSelect": true
            }
        ]
    });
    let ToolInput::AskUserQuestion(ask) = parse("AskUserQuestion", input) else {
        panic!("not an AskUserQuestion input");
    };
    let [format, sections] = ask.questions.as_slice() else {
        panic!("not two questions");
    };
    assert_eq!(format.question, "Which format should the report use?");
    assert_eq!(format.header, "Format");
    assert!(!format.multi_select);
    let options: Vec<(&str, &str)> = format
        .options
        .iter()
        .map(|option| (option.label.as_str(), option.description.as_str()))
        .collect();
    assert_eq!(
        options,
        [
            ("Summary", "A short overview"),
            ("Detailed", "Every finding")
        ]
    );
    assert!(sections.multi_select);
}

#[test]
fn exit_plan_mode() {
    let input = json!({ "plan": "1. Read the code\n2. Fix the bug" });
    assert_eq!(
        parse("ExitPlanMode", input),
        ToolInput::ExitPlanMode(ExitPlanModeInput {
            plan: Some("1. Read the code\n2. Fix the bug".to_owned()),
        })
    );
}

#[test]
fn edit() {
    let input = json!({
        "file_path": "/home/user/project/src/main.rs",
        "old_string": "version = 1",
        "new_string": "version = 2",
        "replace_all": true
    });
    assert_eq!(
        parse("Edit", input),
        ToolInput::Edit(EditInput {
            file_path: Path::new("/home/user/project/src/main.rs").to_path_buf(),
            old_string: "version = 1".to_owned(),
            new_string: "version = 2".to_owned(),
            replace_all: true,
        })
    );
}

#[test]
fn edit_replaces_one_occurrence_by_default() {
    let input = json!({ "file_path": "/home/user/a.txt", "old_string": "a", "new_string": "b" });
    let ToolInput::Edit(edit) = parse("Edit", input) else {
        panic!("not an Edit input");
    };
    assert!(!edit.replace_all);
}

#[test]
fn write() {
    let input = json!({ "file_path": "/home/user/project/CHANGELOG.md", "content": "## 0.2.0\n" });
    assert_eq!(
        parse("Write", input),
        ToolInput::Write(WriteInput {
            file_path: Path::new("/home/user/project/CHANGELOG.md").to_path_buf(),
            content: "## 0.2.0\n".to_owned(),
        })
    );
}

#[test]
fn bash() {
    let input =
        json!({ "command": "cargo test", "description": "Run the tests", "timeout": 60000 });
    assert_eq!(
        parse("Bash", input),
        ToolInput::Bash(BashInput {
            command: "cargo test".to_owned(),
            description: Some("Run the tests".to_owned()),
        })
    );
}

#[test]
fn read() {
    let input = json!({ "file_path": "/home/user/project/src/lib.rs", "offset": 40, "limit": 20 });
    assert_eq!(
        parse("Read", input),
        ToolInput::Read(ReadInput {
            file_path: Path::new("/home/user/project/src/lib.rs").to_path_buf(),
            offset: Some(40),
            limit: Some(20),
        })
    );
}

#[test]
fn skill() {
    let with_args = parse("Skill", json!({"skill": "greet", "args": "world"}));
    let without = parse("Skill", json!({"skill": "review"}));

    assert_eq!(
        [with_args, without],
        [
            ToolInput::Skill(SkillInput {
                skill: "greet".to_owned(),
                args: Some("world".to_owned()),
            }),
            ToolInput::Skill(SkillInput {
                skill: "review".to_owned(),
                args: None,
            }),
        ]
    );
}

#[test]
fn another_tool_is_unknown() {
    for tool_name in ["Grep", "mcp__notes__read_note", "bash"] {
        assert_eq!(
            parse(tool_name, json!({ "pattern": "fn main" })),
            ToolInput::Unknown
        );
    }
}

#[test]
fn a_malformed_known_input_is_an_error() {
    let error =
        ToolInput::parse("Bash", &object(json!({ "description": "no command" }))).unwrap_err();
    let ParseError::MalformedToolInput { tool, .. } = &error else {
        panic!("not a tool input error: {error}");
    };
    assert_eq!(*tool, ToolName::Bash);
    assert_eq!(
        error.to_string(),
        "malformed `Bash` input: missing field `command`"
    );
}

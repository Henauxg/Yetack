//! What the transcript shows of a tool call: its one-line summary, its edit, its output.

use std::path::Path;

use claude_protocol::{ContentBlock, ToolInput};
use cockpit_protocol::FileEdit;
use serde_json::{Map, Value};
use strum::EnumString;

/// The tools that run a subagent. Claude Code has called it both names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
enum SubagentTool {
    Task,
    Agent,
}

/// The input fields a summary shows: the first one present wins.
const SUMMARY_FIELDS: [&str; 9] = [
    "command",
    FILE_PATH_FIELD,
    NOTEBOOK_PATH_FIELD,
    "pattern",
    "url",
    "query",
    "description",
    "prompt",
    "skill",
];
const FILE_PATH_FIELD: &str = "file_path";
const NOTEBOOK_PATH_FIELD: &str = "notebook_path";
const SUMMARY_MAX_CHARS: usize = 200;

/// Claude Code's record of a `Write` call holds the file as it was before.
const ORIGINAL_FILE_FIELD: &str = "originalFile";
/// How Claude Code starts the output of a command that failed.
const EXIT_CODE_PREFIX: &str = "Exit code ";
const IMAGE_PLACEHOLDER: &str = "[image]";
const TRUNCATED_MARK: &str = "\n[truncated]";
const ELLIPSIS: char = '…';

/// The most of a tool's output a transcript item keeps, so that a history page stays small.
pub(crate) const MAX_OUTPUT_BYTES: usize = 64 * 1024;

pub(crate) fn is_subagent(tool: &str) -> bool {
    tool.parse::<SubagentTool>().is_ok()
}

/// One line for the card's title: the command, the file path (relative to the agent's folder
/// when inside it), the pattern, the URL.
pub(crate) fn summary(input: &Map<String, Value>, folder: &Path) -> String {
    for field in SUMMARY_FIELDS {
        let Some(Value::String(text)) = input.get(field) else {
            continue;
        };
        let shown = if field == FILE_PATH_FIELD || field == NOTEBOOK_PATH_FIELD {
            Path::new(text)
                .strip_prefix(folder)
                .map_or_else(|_| text.clone(), |relative| relative.display().to_string())
        } else {
            text.clone()
        };
        return one_line(&shown);
    }
    String::new()
}

/// For `Edit` and `Write`. A `Write` starts with no old text: the result may tell it later.
pub(crate) fn file_edit(input: &ToolInput) -> Option<FileEdit> {
    match input {
        ToolInput::Edit(edit) => Some(FileEdit {
            path: edit.file_path.clone(),
            old_text: edit.old_string.clone(),
            new_text: edit.new_string.clone(),
        }),
        ToolInput::Write(write) => Some(FileEdit {
            path: write.file_path.clone(),
            old_text: String::new(),
            new_text: write.content.clone(),
        }),
        ToolInput::TodoWrite(_)
        | ToolInput::AskUserQuestion(_)
        | ToolInput::ExitPlanMode(_)
        | ToolInput::Bash(_)
        | ToolInput::Read(_)
        | ToolInput::Unknown => None,
    }
}

/// The file before a `Write`, from Claude Code's record of the call.
pub(crate) fn original_file(tool_use_result: Option<&Value>) -> Option<String> {
    tool_use_result?
        .get(ORIGINAL_FILE_FIELD)?
        .as_str()
        .map(str::to_owned)
}

/// The text of a tool result, cut at [`MAX_OUTPUT_BYTES`].
pub(crate) fn result_text(blocks: &[ContentBlock]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => parts.push(&text.text),
            ContentBlock::Image(_) => parts.push(IMAGE_PLACEHOLDER),
            ContentBlock::Thinking(_)
            | ContentBlock::ToolUse(_)
            | ContentBlock::ToolResult(_)
            | ContentBlock::Unknown(_) => {}
        }
    }
    truncate(parts.join("\n"), MAX_OUTPUT_BYTES)
}

/// Cuts `text` to at most `max_bytes` at a character boundary, and says so.
pub(crate) fn truncate(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes.saturating_sub(TRUNCATED_MARK.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATED_MARK);
    text
}

/// The exit code of a failed command: Claude Code starts its output with "Exit code 2".
pub(crate) fn exit_code(output: &str) -> Option<i32> {
    let first_line = output.lines().next()?;
    first_line.strip_prefix(EXIT_CODE_PREFIX)?.trim().parse().ok()
}

/// Lines added and removed from `old` to `new`, after their common first and last lines.
pub(crate) fn changed_lines(old: &str, new: &str) -> (u32, u32) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let common_start = old_lines
        .iter()
        .zip(&new_lines)
        .take_while(|(old, new)| old == new)
        .count();
    let old_rest = old_lines.get(common_start..).unwrap_or_default();
    let new_rest = new_lines.get(common_start..).unwrap_or_default();
    let common_end = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let added = new_rest.len() - common_end;
    let removed = old_rest.len() - common_end;
    (
        u32::try_from(added).unwrap_or(u32::MAX),
        u32::try_from(removed).unwrap_or(u32::MAX),
    )
}

fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.chars().count() <= SUMMARY_MAX_CHARS {
        return line.to_owned();
    }
    let mut shortened: String = line.chars().take(SUMMARY_MAX_CHARS - 1).collect();
    shortened.push(ELLIPSIS);
    shortened
}

#[cfg(test)]
mod tests {
    use claude_protocol::TextBlock;
    use serde_json::json;

    use super::*;

    fn input(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object");
        };
        map
    }

    #[test]
    fn summaries_pick_the_telling_field() {
        let folder = Path::new("/srv/work/app");

        let bash = summary(
            &input(json!({"command": "cargo test\n--quiet", "description": "Run the tests"})),
            folder,
        );
        let read = summary(
            &input(json!({"file_path": "/srv/work/app/src/main.rs"})),
            folder,
        );
        let outside = summary(&input(json!({"file_path": "/etc/hosts"})), folder);
        let grep = summary(&input(json!({"pattern": "fn main", "path": "src"})), folder);
        let nothing = summary(&input(json!({"count": 3})), folder);

        assert_eq!(bash, "cargo test");
        assert_eq!(read, "src/main.rs");
        assert_eq!(outside, "/etc/hosts");
        assert_eq!(grep, "fn main");
        assert_eq!(nothing, "");
    }

    #[test]
    fn a_long_summary_is_shortened() {
        let long = "x".repeat(500);

        let shown = summary(&input(json!({ "command": long })), Path::new("/"));

        assert_eq!(shown.chars().count(), SUMMARY_MAX_CHARS);
        assert!(shown.ends_with(ELLIPSIS));
    }

    #[test]
    fn subagent_tools_are_recognized() {
        assert!(is_subagent("Task"));
        assert!(is_subagent("Agent"));
        assert!(!is_subagent("Bash"));
    }

    #[test]
    fn exit_codes_come_from_the_first_line() {
        assert_eq!(exit_code("Exit code 2\nerror: no such file"), Some(2));
        assert_eq!(exit_code("all good"), None);
    }

    #[test]
    fn changed_lines_skip_the_common_lines() {
        assert_eq!(changed_lines("a\nb\nc\n", "a\nB\nc\n"), (1, 1));
        assert_eq!(changed_lines("", "one\ntwo\n"), (2, 0));
        assert_eq!(changed_lines("a\nb\n", "a\nb\nc\n"), (1, 0));
        assert_eq!(changed_lines("same", "same"), (0, 0));
    }

    #[test]
    fn long_output_is_cut_at_a_character_boundary() {
        let text = "é".repeat(MAX_OUTPUT_BYTES);
        let blocks = vec![ContentBlock::Text(TextBlock { text })];

        let output = result_text(&blocks);

        assert!(output.len() <= MAX_OUTPUT_BYTES);
        assert!(output.ends_with(TRUNCATED_MARK));
    }

    #[test]
    fn the_original_file_comes_from_the_record() {
        let record = json!({"type": "update", "originalFile": "old text"});

        assert_eq!(original_file(Some(&record)).as_deref(), Some("old text"));
        assert_eq!(original_file(Some(&json!("Done"))), None);
    }
}

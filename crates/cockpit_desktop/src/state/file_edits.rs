//! The file view's diff: the edits an agent made to a file, and the file's text before them.

use std::path::{Path, PathBuf};

use cockpit_protocol::{FileEdit, ItemBody, ToolCallStatus};

use super::transcript::Transcript;
use crate::path_style::PathStyle;

/// `path` relative to `folder` when it is inside it, so that a link, a changed file and an edit
/// name the same file the same way. On Windows, the path has `\` between its names.
pub fn relative_path(folder: &Path, path: &Path) -> PathBuf {
    let style = PathStyle::of(folder);
    let text = path.to_string_lossy();
    let kept = match style.strip_prefix(&text, &folder.to_string_lossy()) {
        Some(relative) if !relative.is_empty() => relative,
        Some(_) | None => &text,
    };
    PathBuf::from(style.shown(kept).as_ref())
}

/// `path` as a line names it: relative to `folder` when it is inside it.
pub fn shown_path(folder: &Path, path: &Path) -> String {
    let relative = relative_path(folder, path);
    PathStyle::of(folder)
        .shown(&relative.to_string_lossy())
        .into_owned()
}

/// The edits that ran on `path`, in transcript order, subagents' included.
pub fn edits_for<'transcript>(
    transcript: &'transcript Transcript,
    folder: &Path,
    path: &Path,
) -> Vec<&'transcript FileEdit> {
    let style = PathStyle::of(folder);
    let path = path.to_string_lossy();
    let mut edits = Vec::new();
    for item in transcript.items() {
        let (ItemBody::ToolCall(call) | ItemBody::Subagent(call)) = &item.body else {
            continue;
        };
        let Some(edit) = &call.edit else {
            continue;
        };
        let edited = relative_path(folder, &edit.path);
        if call.status == ToolCallStatus::Done && style.same_path(&edited.to_string_lossy(), &path)
        {
            edits.push(edit);
        }
    }
    edits
}

/// The file's text before `edits`, undoing them from the last to the first on `current`. `None`
/// when an edit's new text is no longer in the file, as after a change the transcript does not
/// show.
pub fn original_text(current: &str, edits: &[&FileEdit]) -> Option<String> {
    let mut text = current.to_owned();
    for edit in edits.iter().rev() {
        if edit.old_text.is_empty() {
            // A `Write` created the file.
            return Some(String::new());
        }
        if edit.new_text.is_empty() {
            return None;
        }
        let start = text.find(&edit.new_text)?;
        let end = start + edit.new_text.len();
        text.replace_range(start..end, &edit.old_text);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{ItemId, ToolCall, TranscriptItem};
    use serde_json::Value;

    use super::*;

    fn edit(path: &str, old_text: &str, new_text: &str) -> FileEdit {
        FileEdit {
            path: PathBuf::from(path),
            old_text: old_text.to_owned(),
            new_text: new_text.to_owned(),
        }
    }

    fn call(id: u32, status: ToolCallStatus, edit: FileEdit) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: "Edit".to_owned(),
                summary: String::new(),
                input: Value::Null,
                status,
                output: None,
                exit_code: None,
                edit: Some(edit),
            }),
        }
    }

    #[test]
    fn paths_inside_the_folder_become_relative() {
        let folder = Path::new("/work/app");

        assert_eq!(
            relative_path(folder, Path::new("/work/app/src/main.rs")),
            PathBuf::from("src/main.rs")
        );
        assert_eq!(
            relative_path(folder, Path::new("/etc/hosts")),
            PathBuf::from("/etc/hosts")
        );
        assert_eq!(
            relative_path(folder, Path::new("src/main.rs")),
            PathBuf::from("src/main.rs")
        );
    }

    #[test]
    fn paths_inside_a_windows_folder_become_relative_whatever_their_case_or_separators() {
        let folder = Path::new(r"C:\Users\me\app");

        assert_eq!(
            relative_path(folder, Path::new(r"c:\users\me\app\src\main.rs")),
            PathBuf::from(r"src\main.rs")
        );
        assert_eq!(
            relative_path(folder, Path::new("C:/Users/me/app/src/main.rs")),
            PathBuf::from(r"src\main.rs")
        );
        assert_eq!(
            shown_path(folder, Path::new("D:/notes.txt")),
            r"D:\notes.txt"
        );
        assert_eq!(
            relative_path(folder, Path::new("src/main.rs")),
            PathBuf::from(r"src\main.rs")
        );
    }

    #[test]
    fn edits_for_keeps_the_finished_edits_of_one_file() {
        let mut transcript = Transcript::default();
        transcript.append(call(
            1,
            ToolCallStatus::Done,
            edit("/work/app/a.rs", "1", "2"),
        ));
        transcript.append(call(
            2,
            ToolCallStatus::Failed,
            edit("/work/app/a.rs", "2", "3"),
        ));
        transcript.append(call(
            3,
            ToolCallStatus::Done,
            edit("/work/app/b.rs", "x", "y"),
        ));
        transcript.append(call(4, ToolCallStatus::Done, edit("a.rs", "2", "4")));

        let edits = edits_for(&transcript, Path::new("/work/app"), Path::new("a.rs"));

        let new_texts: Vec<&str> = edits.iter().map(|edit| edit.new_text.as_str()).collect();
        assert_eq!(new_texts, ["2", "4"]);
    }

    #[test]
    fn original_text_undoes_the_edits_in_reverse() {
        let first = edit("a.rs", "let total = 1;", "let total = 2;");
        let second = edit("a.rs", "fn main() {", "pub fn main() {");
        let current = "pub fn main() {\n    let total = 2;\n}\n";

        let original = original_text(current, &[&first, &second]);

        assert_eq!(
            original.as_deref(),
            Some("fn main() {\n    let total = 1;\n}\n")
        );
    }

    #[test]
    fn a_created_file_was_empty_and_a_lost_edit_gives_up() {
        let created = edit("a.rs", "", "fn main() {}\n");
        let lost = edit("a.rs", "old", "text the file no longer has");

        assert_eq!(
            original_text("fn main() {}\n", &[&created]).as_deref(),
            Some("")
        );
        assert_eq!(original_text("fn main() {}\n", &[&lost]), None);
    }
}

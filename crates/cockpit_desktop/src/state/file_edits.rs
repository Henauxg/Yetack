//! The file view's diff: the edits an agent made to a file, and the file's text before them.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use cockpit_protocol::{EditTexts, FileEdit, ItemBody, ToolCallStatus};

use super::transcript::Transcript;
use crate::path_style::{PARENT, PathStyle};

/// Claude Code's shell tool. Without its input, its summary is the command's first line.
pub const BASH_TOOL: &str = "Bash";

/// Claude Code's tool that reads a file.
pub const READ_TOOL: &str = "Read";

/// The input fields that name a file, as Claude Code's `Read`, `Write`, `Edit` and
/// `NotebookEdit` send them.
pub const FILE_FIELDS: [&str; 2] = ["file_path", "notebook_path"];

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

/// How a call's line names a file: its name, then where it is, short enough for one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPath {
    pub name: String,
    /// Empty for a file at the top of the agent's folder.
    pub folder: String,
}

/// The longest folder name a call's line shows whole: a longer one keeps its start and its end.
const MAX_SHOWN_NAME_CHARS: usize = 28;
/// The most folders a call's line goes up with `..` from the agent's folder. Past that, it shows
/// the path from its root.
const MAX_LEVELS_UP: usize = 2;
/// The folders at the end of a call's folder that always show, after the first one.
const KEPT_LAST_FOLDERS: usize = 2;
/// What stands for the names a shortened path leaves out.
const LEFT_OUT: &str = "…";

/// `path` as a call's line shows it: its name, then its folder, from the folder it shares with
/// the agent's folder. The folder keeps its first name and its last two, so a sibling worktree's
/// file reads `../shop_4PWLC-LetThe…Refund/…/claims/delivery`.
///
/// - Inside the agent's folder, the folder is relative, as elsewhere in the app.
/// - Up to [`MAX_LEVELS_UP`] folders above it, the folder starts with `..`.
/// - Further, it starts from the root.
pub fn call_path(agent_folder: &Path, path: &Path) -> CallPath {
    let style = PathStyle::of(agent_folder);
    let text = path.to_string_lossy();
    // `/work/shop/src/../../etc/hosts` shows where it leads, `../etc`, not `src/…/../etc`.
    let text = if style.is_absolute(&text) && style.goes_up(&text) {
        Cow::Owned(style.resolve(&text, ""))
    } else {
        text
    };
    let base = agent_folder.to_string_lossy();
    let name = style.file_name(&text).unwrap_or(&text).to_owned();
    let Some(parent) = style.parent(&text) else {
        return CallPath {
            name,
            folder: String::new(),
        };
    };
    let (start, names): (String, Vec<&str>) = match style.strip_prefix(parent, &base) {
        Some(inside) => (String::new(), style.names(inside).collect()),
        None => {
            let names: Vec<&str> = style.names(parent).collect();
            let base_names: Vec<&str> = style.names(&base).collect();
            let shared = if style.same_root(parent, &base) {
                names
                    .iter()
                    .zip(&base_names)
                    .take_while(|(name, base_name)| style.same_name(name, base_name))
                    .count()
            } else {
                0
            };
            let levels = base_names.len() - shared;
            if shared > 0 && levels <= MAX_LEVELS_UP {
                (
                    vec![PARENT; levels].join(&style.separator().to_string()),
                    names.get(shared..).unwrap_or_default().to_vec(),
                )
            } else {
                let root = style.root_len(parent).unwrap_or(0);
                let root = style
                    .shown(parent.get(..root).unwrap_or_default())
                    .into_owned();
                (root, names)
            }
        }
    };
    let rest = shortened(&names, style.separator());
    // A root alone keeps its separator: `/`, `C:\`.
    let folder = if rest.is_empty() {
        start
    } else {
        style.join(&start, &rest)
    };
    CallPath { name, folder }
}

/// The folder names joined, the first one cut in its middle when long, and the ones between it
/// and the last [`KEPT_LAST_FOLDERS`] left out.
fn shortened(names: &[&str], separator: char) -> String {
    let Some((first, rest)) = names.split_first() else {
        return String::new();
    };
    let mut shown = vec![cut_in_the_middle(first)];
    if rest.len() > KEPT_LAST_FOLDERS {
        shown.push(LEFT_OUT.to_owned());
    }
    let kept = rest.len().saturating_sub(KEPT_LAST_FOLDERS);
    shown.extend(rest.iter().skip(kept).map(|name| (*name).to_owned()));
    shown.join(&separator.to_string())
}

/// `name` whole, or its start and its end around [`LEFT_OUT`] when it is longer than
/// [`MAX_SHOWN_NAME_CHARS`].
fn cut_in_the_middle(name: &str) -> String {
    let count = name.chars().count();
    if count <= MAX_SHOWN_NAME_CHARS {
        return name.to_owned();
    }
    let start = MAX_SHOWN_NAME_CHARS / 2;
    let end = MAX_SHOWN_NAME_CHARS - start - 1;
    let head: String = name.chars().take(start).collect();
    let tail: String = name.chars().skip(count - end).collect();
    format!("{head}{LEFT_OUT}{tail}")
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

/// The edits after the last one too large to keep its texts. The file's text before that edit is
/// unknown, so a diff can only start after it. All of `edits` when none is too large.
pub fn after_too_large(edits: &[FileEdit]) -> &[FileEdit] {
    let start = edits
        .iter()
        .rposition(|edit| edit.texts == EditTexts::TooLarge)
        .map_or(0, |last| last + 1);
    edits.get(start..).unwrap_or_default()
}

/// The file's text before `edits`, undoing them from the last to the first on `current`. `None`
/// when an edit's new text is no longer in the file, as after a change the transcript does not
/// show, and when an edit is too large to keep its texts.
pub fn original_text(current: &str, edits: &[&FileEdit]) -> Option<String> {
    let mut text = current.to_owned();
    for edit in edits.iter().rev() {
        if edit.texts == EditTexts::TooLarge {
            return None;
        }
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

    fn shown(folder: &str, path: &str) -> (String, String) {
        let CallPath { name, folder } = call_path(Path::new(folder), Path::new(path));
        (name, folder)
    }

    fn pair(name: &str, folder: &str) -> (String, String) {
        (name.to_owned(), folder.to_owned())
    }

    #[test]
    fn a_call_names_its_file_then_a_short_folder_from_the_agents_folder() {
        let agent = "/home/dev/projects/shop";
        let worktree = "/home/dev/projects/shop_4PWLC-LetTheBackofficeRegisterTheGestureOfferedAfterAnEarlierRefund";

        assert_eq!(
            shown(agent, "/home/dev/projects/shop/README.md"),
            pair("README.md", "")
        );
        assert_eq!(
            shown(agent, "/home/dev/projects/shop/src/checkout/cart.rs"),
            pair("cart.rs", "src/checkout")
        );
        assert_eq!(
            shown(
                agent,
                &format!("{worktree}/backoffice/src/views/claims/delivery/claim.util.ts")
            ),
            pair(
                "claim.util.ts",
                "../shop_4PWLC-Let…EarlierRefund/…/claims/delivery"
            )
        );
        assert_eq!(
            shown(agent, &format!("{worktree}/notes.md")),
            pair("notes.md", "../shop_4PWLC-Let…EarlierRefund")
        );
        assert_eq!(
            shown(agent, "/home/dev/.cargo/config.toml"),
            pair("config.toml", "../../.cargo")
        );
        assert_eq!(
            shown(agent, "/etc/xdg/tools/editor/settings.json"),
            pair("settings.json", "/etc/…/tools/editor")
        );
        assert_eq!(shown(agent, "/notes.txt"), pair("notes.txt", "/"));
    }

    #[test]
    fn a_call_on_windows_shows_its_folder_with_backslashes() {
        let agent = r"C:\work\shop";

        assert_eq!(
            shown(agent, r"C:\work\shop\src\cart.rs"),
            pair("cart.rs", "src")
        );
        assert_eq!(
            shown(agent, r"C:\work\shop-tests\unit\cart\totals.rs"),
            pair("totals.rs", r"..\shop-tests\unit\cart")
        );
        assert_eq!(
            shown(agent, r"D:\data\a\b\c\export.csv"),
            pair("export.csv", r"D:\data\…\b\c")
        );
    }

    #[test]
    fn a_call_shows_where_a_path_with_dots_leads() {
        let agent = "/home/dev/projects/shop";

        assert_eq!(
            shown(agent, "/home/dev/projects/shop/src/../../../etc/hosts"),
            pair("hosts", "../../etc")
        );
        assert_eq!(
            shown(agent, "/home/dev/projects/shop/./src/./cart.rs"),
            pair("cart.rs", "src")
        );
        assert_eq!(
            shown(agent, "/home/dev/projects/shop/../billing/main.rs"),
            pair("main.rs", "../billing")
        );
        assert_eq!(
            shown(agent, "/home/dev/projects/shop/src/.."),
            pair("shop", "..")
        );
    }

    #[test]
    fn a_file_whose_folder_is_a_root_shows_the_root_whole() {
        assert_eq!(
            shown(r"C:\work\shop", r"D:\export.csv"),
            pair("export.csv", r"D:\")
        );
        assert_eq!(
            shown(r"C:\work\shop", "c:/export.csv"),
            pair("export.csv", r"c:\")
        );
        assert_eq!(
            shown(r"C:\work\shop", r"\\files\team\notes.txt"),
            pair("notes.txt", r"\\files\team\")
        );
        assert_eq!(
            shown(r"C:\work\shop", r"\\files\team\a\b\c\d\notes.txt"),
            pair("notes.txt", r"\\files\team\a\…\c\d")
        );
    }

    #[test]
    fn a_call_path_compares_windows_names_whatever_their_case_and_separators() {
        let agent = r"C:\work\shop";

        assert_eq!(
            shown(agent, r"c:\WORK\Shop\src\cart.rs"),
            pair("cart.rs", "src")
        );
        assert_eq!(
            shown(agent, "c:/work/Billing/main.rs"),
            pair("main.rs", r"..\Billing")
        );
        assert_eq!(
            shown(agent, r"C:/work/shop/src\a/b\c/cart.rs"),
            pair("cart.rs", r"src\…\b\c")
        );
    }

    #[test]
    fn a_call_path_from_a_root_folder_or_a_relative_path() {
        assert_eq!(shown("/", "/etc/hosts"), pair("hosts", "etc"));
        assert_eq!(shown("/", "/hosts"), pair("hosts", ""));
        assert_eq!(shown(r"C:\", r"C:\data\a.csv"), pair("a.csv", "data"));
        assert_eq!(
            shown("/home/dev/projects/shop", "src/cart.rs"),
            pair("cart.rs", "src")
        );
        assert_eq!(
            shown("/home/dev/projects/shop", "/home/dev/projects/shop"),
            pair("shop", "..")
        );
        assert_eq!(
            shown("/home/dev/projects/shop/", "/home/dev/projects/shop/src/"),
            pair("src", "")
        );
    }

    #[test]
    fn a_long_folder_name_is_cut_between_characters() {
        let name = format!("{}{}", "é".repeat(20), "日".repeat(20));

        assert_eq!(
            shown(
                "/home/dev/projects/shop",
                &format!("/home/dev/projects/{name}/a.rs")
            ),
            pair(
                "a.rs",
                &format!("../{}…{}", "é".repeat(14), "日".repeat(13))
            )
        );
    }

    fn edit(path: &str, old_text: &str, new_text: &str) -> FileEdit {
        FileEdit {
            path: PathBuf::from(path),
            old_text: old_text.to_owned(),
            new_text: new_text.to_owned(),
            texts: EditTexts::Kept,
        }
    }

    fn too_large(path: &str) -> FileEdit {
        FileEdit {
            path: PathBuf::from(path),
            old_text: String::new(),
            new_text: String::new(),
            texts: EditTexts::TooLarge,
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

    #[test]
    fn an_edit_too_large_to_keep_is_neither_a_creation_nor_undone() {
        let large = too_large("a.rs");
        let later = edit("a.rs", "let total = 1;", "let total = 2;");

        assert_eq!(original_text("let total = 1;\n", &[&large]), None);
        assert_eq!(original_text("let total = 2;\n", &[&large, &later]), None);
    }

    #[test]
    fn a_diff_starts_after_the_last_edit_too_large_to_keep() {
        let first = edit("a.rs", "1", "2");
        let large = too_large("a.rs");
        let last = edit("a.rs", "2", "3");

        assert_eq!(
            after_too_large(&[first.clone(), last.clone()]),
            [first.clone(), last.clone()]
        );
        assert_eq!(
            after_too_large(&[first.clone(), large.clone(), last.clone()]),
            [last]
        );
        assert!(after_too_large(&[first, large]).is_empty());
    }
}

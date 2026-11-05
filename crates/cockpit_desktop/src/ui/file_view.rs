//! The read-only file view on the right: tabs of the changed files, the diff or the whole file,
//! and the line a `path:line` link marks.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf};

use cockpit_protocol::{AgentId, FileEdit};
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::render::{self, Diff, Language, MAX_RENDERED_LINES, diff_lines};
use crate::state::{FileRead, Loadable, ViewMode, actions, file_edits};
use crate::ui::changes_list::{outside_root, root_label};
use crate::ui::code::{LineNumbers, diff_line, diff_row, lines_not_shown};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::splitter::FILE_VIEW_ID;

/// With Ctrl, closes the shown tab.
const CLOSE_TAB_KEY: &str = "w";

/// Unchanged lines kept around each change of the file view's diff.
const FILE_DIFF_CONTEXT: usize = 3;

/// The widths of the placeholder lines shown while the file loads, in percent of the text's width.
const LOADING_LINE_WIDTHS: [u8; 8] = [46, 72, 64, 30, 81, 58, 69, 38];

#[component]
pub fn FileViewPanel() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent = entry.id();
    let Some(view) = state.ui.file_view(agent).cloned() else {
        return rsx! {};
    };
    let folder = &entry.summary.folder;
    let tabs = view.tabs(&entry.changed_paths());
    let text = entry.files.get(&view.path).cloned().unwrap_or_default();
    let edits: Vec<FileEdit> = file_edits::edits_for(&entry.transcript, folder, &view.path)
        .into_iter()
        .cloned()
        .collect();
    let short = ShortPath::new(folder, &view.path);
    let full = folder.join(&view.path).display().to_string();
    let labels = tab_labels(&tabs);
    let note = if edits.is_empty() {
        "This agent did not change the file.".to_owned()
    } else {
        format!(
            "Changed by {} of this agent.",
            format::count(edits.len(), "edit", "edits")
        )
    };
    let diff = view.mode == ViewMode::Diff;
    let shown = view.path.clone();
    let loading = matches!(text, Loadable::NotRequested | Loadable::Loading);
    let wrap = state.prefs.file_view.wrap;
    rsx! {
        section {
            id: FILE_VIEW_ID,
            class: "file-view",
            aria_label: "File view",
            // Focusable, so that a click in the view lets Ctrl+W close its tab.
            tabindex: -1,
            onkeydown: move |event| {
                let command = event.modifiers().ctrl() || event.modifiers().meta();
                if command && event.key() == Key::Character(CLOSE_TAB_KEY.to_owned()) {
                    event.prevent_default();
                    let shown = shown.clone();
                    cockpit.update(|state| actions::close_tab(state, agent, &shown));
                }
            },
            div { class: "file-tabs",
                div { class: "tab-list scroll-x", role: "tablist", aria_label: "Files changed by this agent",
                    for (tab, label) in tabs.into_iter().zip(labels) {
                        FileTab {
                            key: "{tab.display()}",
                            agent,
                            selected: tab == view.path,
                            full: folder.join(&tab).display().to_string(),
                            path: tab,
                            label,
                        }
                    }
                }
                button {
                    class: "icon-button",
                    aria_label: "Close the file view",
                    title: "Close the file view",
                    onclick: move |_| cockpit.update(|state| actions::close_file_view(state, agent)),
                    Icon { kind: IconKind::Close, size: 16 }
                }
            }
            div { class: "file-bar",
                span { class: "file-label mono",
                    span { class: "file-path", title: "{full}", {short_path(&short)} }
                    if let Some(line) = view.line {
                        span { class: "file-line", ", line {line}" }
                    }
                    if loading {
                        span { class: "spinner", role: "status", aria_label: "Loading the file" }
                    }
                }
                span { class: "read-only-chip", "Read-only" }
                div { class: "segmented", role: "group", aria_label: "View mode",
                    button {
                        class: if diff { "segment chosen" } else { "segment" },
                        aria_pressed: diff,
                        onclick: move |_| cockpit.update(|state| actions::set_view_mode(state, agent, ViewMode::Diff)),
                        "Diff"
                    }
                    button {
                        class: if diff { "segment" } else { "segment chosen" },
                        aria_pressed: !diff,
                        onclick: move |_| cockpit.update(|state| actions::set_view_mode(state, agent, ViewMode::WholeFile)),
                        "Whole file"
                    }
                }
                div { class: "segmented",
                    button {
                        class: if wrap { "segment chosen" } else { "segment" },
                        aria_pressed: wrap,
                        title: "Wrap long lines instead of scrolling sideways",
                        onclick: move |_| cockpit.update(actions::toggle_wrap),
                        "Wrap"
                    }
                }
            }
            div { class: if wrap { "file-body scroll mono wrap" } else { "file-body scroll mono" },
                match text {
                    Loadable::NotRequested | Loadable::Loading => rsx! {
                        div { class: "file-loading", aria_hidden: true,
                            for (index, width) in LOADING_LINE_WIDTHS.iter().enumerate() {
                                div { key: "{index}", class: "skeleton skeleton-line", style: "width: {width}%" }
                            }
                        }
                    },
                    Loadable::Failed(error) => rsx! {
                        p { class: "file-note", "Cannot read the file: {error}" }
                    },
                    Loadable::Loaded(FileRead::Missing(sentence)) => rsx! {
                        p { class: "file-note", "{sentence}." }
                    },
                    Loadable::Loaded(FileRead::Choices(matches)) => rsx! {
                        FileChoices { agent, folder: folder.clone(), name: view.path.clone(), matches, line: view.line }
                    },
                    Loadable::Loaded(FileRead::Text(text)) => rsx! {
                        FileContent { path: view.path.clone(), text, mode: view.mode, line: view.line, edits }
                    },
                }
            }
            div { class: "file-foot", "{note} To change it, ask the agent." }
        }
    }
}

/// `label` is the file's name, with the folders that tell it apart from another tab; `full`, its
/// absolute path.
#[component]
fn FileTab(agent: AgentId, path: PathBuf, label: String, full: String, selected: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let close = {
        let path = path.clone();
        move || {
            let path = path.clone();
            cockpit.update(|state| actions::close_tab(state, agent, &path));
        }
    };
    let close_on_middle = close.clone();
    rsx! {
        div {
            class: if selected { "file-tab selected" } else { "file-tab" },
            role: "tab",
            aria_selected: selected,
            title: "{full}",
            onauxclick: move |event| {
                if event.trigger_button() == Some(MouseButton::Auxiliary) {
                    event.prevent_default();
                    close_on_middle();
                }
            },
            button {
                class: "file-tab-name",
                onclick: move |_| {
                    let path = path.clone();
                    cockpit.update(|state| actions::open_file(state, agent, path, None));
                },
                "{label}"
            }
            button {
                class: "file-tab-close",
                aria_label: "Close {label}",
                title: "Close (middle-click, or Ctrl+W in the file view)",
                onclick: move |_| close(),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

/// The files that end with `name`, for the user to pick one.
#[component]
fn FileChoices(
    agent: AgentId,
    folder: PathBuf,
    name: PathBuf,
    matches: Vec<PathBuf>,
    line: Option<u32>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let count = matches.len();
    rsx! {
        div { class: "file-choices",
            p { class: "file-note", "{count} files end with {name.display()}. Pick one:" }
            for choice in matches {
                {
                    let short = ShortPath::new(&folder, &choice);
                    let name = name.clone();
                    rsx! {
                        button {
                            key: "{choice.display()}",
                            class: "file-choice mono",
                            title: "{choice.display()}",
                            onclick: move |_| {
                                let choice = choice.clone();
                                cockpit.update(|state| actions::pick_file(state, agent, &name, choice, line));
                            },
                            span { class: "file-path", {short_path(&short)} }
                        }
                    }
                }
            }
        }
    }
}

/// A file's path as the file view shows it: relative to the agent's folder, or, for a file outside
/// it, relative to the folder beside it that holds it, which `root` names.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShortPath {
    /// The outside folder's name, as "app_feature/".
    root: Option<String>,
    rest: String,
}

impl ShortPath {
    fn new(folder: &Path, path: &Path) -> Self {
        let path = file_edits::relative_path(folder, path);
        if path.is_relative() {
            return Self {
                root: None,
                rest: path.display().to_string(),
            };
        }
        let root = outside_root(folder, &path);
        let rest = path.strip_prefix(&root).map_or_else(
            |_| path.display().to_string(),
            |rest| rest.display().to_string(),
        );
        Self {
            root: Some(root_label(&root)),
            rest,
        }
    }
}

/// On one line cut at its start, the outside folder dim.
fn short_path(short: &ShortPath) -> Element {
    rsx! {
        span { class: "file-path-text",
            if let Some(root) = &short.root {
                span { class: "file-path-root", "{root}" }
            }
            "{short.rest}"
        }
    }
}

/// Each tab's file name, with as many of its folders as tell it apart from another tab of the same
/// name: "lib.rs", or "src/lib.rs" beside "tests/lib.rs".
fn tab_labels(tabs: &[PathBuf]) -> Vec<String> {
    let names: Vec<Vec<&OsStr>> = tabs.iter().map(|tab| names_from_the_end(tab)).collect();
    let mut labels = Vec::with_capacity(tabs.len());
    for (index, own) in names.iter().enumerate() {
        let others = names
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, names)| names);
        let mut shown = 1;
        for other in others {
            // The fewest names that differ from this other tab's, as far as this path goes.
            let shared = own.iter().zip(other).take_while(|(a, b)| a == b).count();
            shown = shown.max((shared + 1).min(own.len()));
        }
        let mut label: Vec<Cow<'_, str>> = own
            .iter()
            .take(shown)
            .map(|name| name.to_string_lossy())
            .collect();
        label.reverse();
        labels.push(label.join(MAIN_SEPARATOR_STR));
    }
    labels
}

/// A path's names, the file's first.
fn names_from_the_end(path: &Path) -> Vec<&OsStr> {
    let names = path
        .components()
        .rev()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        });
    names.collect()
}

/// The rows are rebuilt only when the text, the mode, the line or the edits change.
#[component]
fn FileContent(
    path: PathBuf,
    text: String,
    mode: ViewMode,
    line: Option<u32>,
    edits: Vec<FileEdit>,
) -> Element {
    let language = Language::from_path(&path);
    if mode == ViewMode::WholeFile || edits.is_empty() {
        let lines = render::plain_lines(&text, language, 1, MAX_RENDERED_LINES);
        return rsx! {
            if mode == ViewMode::Diff {
                p { class: "file-note", "This agent did not change the file. Here is its text." }
            }
            div { class: "diff-rows",
                for row in lines.rows.iter() {
                    {diff_line(row, LineNumbers::Shown, line)}
                }
            }
            {lines_not_shown(lines.more)}
        };
    }
    let edit_refs: Vec<&FileEdit> = edits.iter().collect();
    if let Some(original) = file_edits::original_text(&text, &edit_refs) {
        let diff = diff_lines(
            &original,
            &text,
            language,
            FILE_DIFF_CONTEXT,
            1,
            MAX_RENDERED_LINES,
        );
        if diff.rows.is_empty() {
            return rsx! {
                p { class: "file-note", "The file is back to its text before the edits." }
            };
        }
        return rsx! {
            div { class: "diff-rows",
                for row in diff.rows.iter() {
                    {diff_row(row, LineNumbers::Shown, line)}
                }
            }
            {lines_not_shown(diff.more)}
        };
    }
    // Another change hides where the edits were: each edit shows on its own, without line
    // numbers. The edits share the cap.
    let count = edits.len();
    let mut left = MAX_RENDERED_LINES;
    let mut diffs: Vec<Diff> = Vec::with_capacity(count);
    for edit in &edits {
        let diff = diff_lines(
            &edit.old_text,
            &edit.new_text,
            language,
            FILE_DIFF_CONTEXT,
            1,
            left,
        );
        left = left.saturating_sub(diff.rows.len());
        diffs.push(diff);
    }
    rsx! {
        p { class: "file-note",
            "The file changed since these edits, so each shows on its own, without line numbers."
        }
        for (index, diff) in diffs.iter().enumerate() {
            p { class: "edit-title", "Edit {index + 1} of {count}" }
            div { class: "diff-rows",
                for row in diff.rows.iter() {
                    {diff_row(row, LineNumbers::Hidden, None)}
                }
            }
            {lines_not_shown(diff.more)}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::MAIN_SEPARATOR;

    use super::*;

    fn labels(tabs: &[&str]) -> Vec<String> {
        let tabs: Vec<PathBuf> = tabs.iter().map(PathBuf::from).collect();
        tab_labels(&tabs)
    }

    #[test]
    fn a_tab_shows_its_name_and_only_the_folders_that_tell_it_apart() {
        assert_eq!(
            labels(&[
                "src/error.rs",
                "/work/app_feature/src/tokens/unexpected_token.rs"
            ]),
            ["error.rs", "unexpected_token.rs"]
        );
        assert_eq!(
            labels(&["src/lib.rs", "tests/lib.rs", "src/main.rs"]),
            ["src/lib.rs", "tests/lib.rs", "main.rs"]
        );
        // The relative path has no more folders: the other tab shows the one beyond.
        assert_eq!(
            labels(&["src/lib.rs", "/work/app_feature/src/lib.rs"]),
            ["src/lib.rs", "app_feature/src/lib.rs"]
        );
    }

    #[test]
    fn a_path_outside_the_folder_shows_under_the_folder_beside_it() {
        let folder = Path::new("/work/app");

        let inside = ShortPath::new(folder, Path::new("/work/app/src/main.rs"));
        let beside = ShortPath::new(
            folder,
            Path::new("/work/app_GH42Q-a-long-branch-name/crates/lexer/src/position.rs"),
        );

        assert_eq!(
            inside,
            ShortPath {
                root: None,
                rest: "src/main.rs".to_owned()
            }
        );
        assert_eq!(
            beside,
            ShortPath {
                root: Some(format!("app_GH42Q-a-long-branch-name{MAIN_SEPARATOR}")),
                rest: "crates/lexer/src/position.rs".to_owned()
            }
        );
    }
}

//! The read-only file view on the right: tabs of the changed files, the diff, the whole file or a
//! markdown file rendered, and the line a `path:line` link marks.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cockpit_protocol::{AgentId, FileEdit};
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::path_style::PathStyle;
use crate::render::{self, Diff, Language, MAX_RENDERED_LINES, diff_lines};
use crate::state::{
    FileFreshness, FileRead, FileUpdate, Loadable, Popover, Reread, ViewMode, actions, file_edits,
    file_reread, is_markdown,
};
use crate::ui::changes_list;
use crate::ui::code::{
    CopyButton, CopyLook, LineMarks, LineNumbers, diff_line, diff_row, lines_not_shown,
};
use crate::ui::file_list::FileListButton;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::markdown::MarkdownView;
use crate::ui::splitter::FILE_VIEW_ID;
use crate::{bridge, format};

/// The shown tab's id, which keeps it in view in a long row of tabs.
const SHOWN_TAB_ID: &str = "file-tab-shown";

/// With Ctrl, closes the shown tab, or the highlighted file in the list of open files.
pub const CLOSE_TAB_KEY: &str = "w";

/// Unchanged lines kept around each change of the file view's diff.
const FILE_DIFF_CONTEXT: usize = 3;

/// How long "Updated just now" and the changed lines' highlight last after a read again.
const UPDATE_SHOWN_FOR: Duration = Duration::from_secs(10);

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
    let tab_count = tabs.len();
    let list_open = state.ui.popover == Some(Popover::FileList);
    let text = entry.files.get(&view.path).cloned().unwrap_or_default();
    let freshness = entry.file_freshness.clone();
    let update = freshness.update.clone();
    let edits: Vec<FileEdit> = file_edits::edits_for(&entry.transcript, folder, &view.path)
        .into_iter()
        .cloned()
        .collect();
    let short = ShortPath::new(folder, &view.path);
    let full = full_path(folder, &view.path);
    let labels = tab_labels(PathStyle::of(folder), &tabs);
    let note = if edits.is_empty() {
        "This agent did not change the file.".to_owned()
    } else {
        format!(
            "Changed by {} of this agent.",
            format::count(edits.len(), "edit", "edits")
        )
    };
    let markdown = is_markdown(&view.path);
    // A view left in Preview on another kind of file shows the whole file.
    let mode = if view.mode == ViewMode::Preview && !markdown {
        ViewMode::WholeFile
    } else {
        view.mode
    };
    let preview = mode == ViewMode::Preview;
    let mut modes = vec![
        (ViewMode::Diff, "Diff"),
        (ViewMode::WholeFile, "Whole file"),
    ];
    if markdown {
        modes.push((ViewMode::Preview, "Preview"));
    }
    let shown = view.path.clone();
    let loading = matches!(text, Loadable::NotRequested | Loadable::Loading);
    let image = matches!(text, Loadable::Loaded(FileRead::Image(_)));
    let copied_text = match &text {
        Loadable::Loaded(FileRead::Text(text)) => Some(text.clone()),
        _ => None,
    };
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
                            full: full_path(folder, &tab),
                            path: tab,
                            label,
                        }
                    }
                }
                FileListButton { agent, count: tab_count, open: list_open }
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
                if !image {
                    div { class: "segmented", role: "group", aria_label: "View mode",
                        for (choice, name) in modes {
                            button {
                                key: "{name}",
                                class: if choice == mode { "segment chosen" } else { "segment" },
                                aria_pressed: choice == mode,
                                onclick: move |_| cockpit.update(|state| actions::set_view_mode(state, agent, choice)),
                                "{name}"
                            }
                        }
                    }
                    button {
                        class: if wrap && !preview { "icon-button framed toggled" } else { "icon-button framed" },
                        aria_label: "Wrap long lines",
                        aria_pressed: wrap && !preview,
                        disabled: preview,
                        title: if preview { "The preview always wraps" } else { "Wrap long lines instead of scrolling sideways" },
                        onclick: move |_| cockpit.update(actions::toggle_wrap),
                        Icon { kind: IconKind::Wrap, size: 15 }
                    }
                    // The file's whole text, in the diff and the preview too.
                    CopyButton { text: copied_text, label: "Copy the file's text", look: CopyLook::IconOnly }
                }
            }
            div {
                class: match (preview, wrap) {
                    (true, _) => "file-body scroll file-preview",
                    (false, true) => "file-body scroll mono wrap",
                    (false, false) => "file-body scroll mono",
                },
                // Focusable, so that after a click in the text End and Home scroll it.
                tabindex: -1,
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
                    Loadable::Loaded(FileRead::Text(text)) if preview => rsx! {
                        FilePreview { agent, folder: folder.clone(), path: view.path.clone(), text }
                    },
                    Loadable::Loaded(FileRead::Text(text)) => rsx! {
                        FileContent { path: view.path.clone(), text, mode, line: view.line, edits, update }
                    },
                    Loadable::Loaded(FileRead::Image(image)) => rsx! {
                        div { class: "file-image",
                            img { src: bridge::image_url(&image), alt: "{full}" }
                        }
                    },
                }
            }
            FileFoot { agent, note, freshness }
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
    use_effect(use_reactive!(|selected| {
        if selected {
            bridge::reveal(SHOWN_TAB_ID);
        }
    }));
    rsx! {
        div {
            id: selected.then_some(SHOWN_TAB_ID),
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
    let style = PathStyle::of(&folder);
    let shown_name = style.shown(&name.to_string_lossy()).into_owned();
    rsx! {
        div { class: "file-choices",
            p { class: "file-note", "{count} files end with {shown_name}. Pick one:" }
            for choice in matches {
                {
                    let short = ShortPath::new(&folder, &choice);
                    let name = name.clone();
                    let full = style.shown(&choice.to_string_lossy()).into_owned();
                    rsx! {
                        button {
                            key: "{full}",
                            class: "file-choice mono",
                            title: "{full}",
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
pub struct ShortPath {
    /// The outside folder's name, as "app_feature/".
    root: Option<String>,
    rest: String,
}

impl ShortPath {
    pub fn new(folder: &Path, path: &Path) -> Self {
        let style = PathStyle::of(folder);
        let path = file_edits::relative_path(folder, path);
        let path = path.to_string_lossy();
        if !style.is_absolute(&path) {
            return Self {
                root: None,
                rest: style.shown(&path).into_owned(),
            };
        }
        let root = changes_list::outside_root(folder, Path::new(path.as_ref()));
        let rest = style
            .strip_prefix(&path, &root.to_string_lossy())
            .unwrap_or(&path)
            .to_owned();
        Self {
            root: Some(changes_list::root_label(&root)),
            rest: style.shown(&rest).into_owned(),
        }
    }

    /// The file's name, and the folders before it, the outside folder first, without the last
    /// separator.
    pub fn name_and_folders(&self) -> (&str, String) {
        let cut = self
            .rest
            .rfind(['/', '\\'])
            .map_or(0, |separator| separator + 1);
        let (folders, name) = self.rest.split_at(cut);
        let folders = format!("{}{folders}", self.root.as_deref().unwrap_or_default());
        (name, folders.trim_end_matches(['/', '\\']).to_owned())
    }
}

/// A tab's path in full, as its tooltip shows it.
pub fn full_path(folder: &Path, tab: &Path) -> String {
    let style = PathStyle::of(folder);
    let tab = tab.to_string_lossy();
    let full = if style.is_absolute(&tab) {
        tab.into_owned()
    } else {
        style.join(&folder.to_string_lossy(), &tab)
    };
    style.shown(&full).into_owned()
}

/// On one line cut at its start, the outside folder dim.
pub fn short_path(short: &ShortPath) -> Element {
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
fn tab_labels(style: PathStyle, tabs: &[PathBuf]) -> Vec<String> {
    let texts: Vec<String> = tabs
        .iter()
        .map(|tab| tab.to_string_lossy().into_owned())
        .collect();
    let names: Vec<Vec<&str>> = texts
        .iter()
        .map(|tab| style.names_from_the_end(tab))
        .collect();
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
            let shared = own
                .iter()
                .zip(other)
                .take_while(|(left, right)| style.same_name(left, right))
                .count();
            shown = shown.max((shared + 1).min(own.len()));
        }
        let mut label: Vec<&str> = own.iter().take(shown).copied().collect();
        label.reverse();
        labels.push(label.join(&style.separator().to_string()));
    }
    labels
}

/// The markdown file at `path` rendered, up to the cap on rendered lines. `folder` is the agent's,
/// where the files its links open must be.
#[component]
fn FilePreview(agent: AgentId, folder: PathBuf, path: PathBuf, text: String) -> Element {
    let (shown, more) = render::split_lines(&text, MAX_RENDERED_LINES);
    let style = PathStyle::of(&folder);
    let full = style.resolve(&folder.to_string_lossy(), &path.to_string_lossy());
    let file_folder = style.parent(&full).map(PathBuf::from);
    rsx! {
        MarkdownView { agent, folder, markdown: shown.to_owned(), file_folder }
        {lines_not_shown(more)}
    }
}

/// The rows are rebuilt only when the text, the mode, the line, the edits or the update change.
#[component]
fn FileContent(
    path: PathBuf,
    text: String,
    mode: ViewMode,
    line: Option<u32>,
    edits: Vec<FileEdit>,
    update: Option<FileUpdate>,
) -> Element {
    let language = Language::from_path(&path);
    let marks = line_marks(line, update.as_ref());
    if mode == ViewMode::WholeFile || edits.is_empty() {
        let lines = render::plain_lines(&text, language, 1, MAX_RENDERED_LINES);
        return rsx! {
            if mode == ViewMode::Diff {
                p { class: "file-note", "This agent did not change the file. Here is its text." }
            }
            div { class: "diff-rows",
                for row in lines.rows.iter() {
                    {diff_line(row, LineNumbers::Shown, marks)}
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
                    {diff_row(row, LineNumbers::Shown, marks)}
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
                    {diff_row(row, LineNumbers::Hidden, LineMarks::marked(None))}
                }
            }
            {lines_not_shown(diff.more)}
        }
    }
}

/// The line a link marks, and the lines a recent read again changed. An older update marks
/// none: a view drawn again later, as after a switch back to the agent, does not play it again.
fn line_marks(line: Option<u32>, update: Option<&FileUpdate>) -> LineMarks<'_> {
    match update.filter(|update| is_recent(update, SystemTime::now())) {
        Some(update) => LineMarks {
            marked: line,
            updated: &update.lines,
            updated_class: if update.serial % 2 == 0 {
                "updated-even"
            } else {
                "updated-odd"
            },
        },
        None => LineMarks::marked(line),
    }
}

/// Whether `update` came less than `UPDATE_SHOWN_FOR` before `now`.
fn is_recent(update: &FileUpdate, now: SystemTime) -> bool {
    update
        .at
        .to_system_time()
        .and_then(|at| now.duration_since(at).ok())
        .is_some_and(|age| age < UPDATE_SHOWN_FOR)
}

/// The foot: what the agent did to the file, when the shown text was read, and Reload.
#[component]
fn FileFoot(agent: AgentId, note: String, freshness: FileFreshness) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context();
    let now = now();
    let reading = freshness.reread != Reread::Idle;
    let updated = freshness
        .update
        .as_ref()
        .is_some_and(|update| is_recent(update, now));
    let read = freshness.read_at.and_then(|read_at| {
        let time = format::time_of_day(read_at)?;
        let ago = format::ago(read_at, now);
        Some(if ago == format::NOW {
            format!("Read at {time}")
        } else {
            format!("Read at {time}, {ago}")
        })
    });
    rsx! {
        div { class: "file-foot",
            span { class: "file-foot-note", "{note} To change it, ask the agent." }
            if reading {
                span { class: "file-freshness",
                    span { class: "spinner", aria_hidden: true }
                    "Reading…"
                }
            } else if updated {
                span { class: "file-freshness updated", role: "status", "Updated just now" }
            } else if let Some(read) = read {
                span { class: "file-freshness", "{read}" }
            }
            button {
                class: "file-reload",
                title: "Read the file again (F5)",
                disabled: reading,
                onclick: move |_| cockpit.update(|state| file_reread::reread(state, agent)),
                Icon { kind: IconKind::Reconnect, size: 13 }
                "Reload"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(tabs: &[&str]) -> Vec<String> {
        let tabs: Vec<PathBuf> = tabs.iter().map(PathBuf::from).collect();
        tab_labels(PathStyle::Unix, &tabs)
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
        let tabs: Vec<PathBuf> = [
            r"src\lib.rs",
            r"Tests\lib.rs",
            r"C:\work\feature\SRC\lib.rs",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        assert_eq!(
            tab_labels(PathStyle::Windows, &tabs),
            [r"src\lib.rs", r"Tests\lib.rs", r"feature\SRC\lib.rs"]
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
                root: Some("app_GH42Q-a-long-branch-name/".to_owned()),
                rest: "crates/lexer/src/position.rs".to_owned()
            }
        );

        let folder = Path::new(r"C:\work\app");
        assert_eq!(
            ShortPath::new(folder, Path::new("c:/work/app/src/main.rs")),
            ShortPath {
                root: None,
                rest: r"src\main.rs".to_owned()
            }
        );
        assert_eq!(
            ShortPath::new(folder, Path::new("C:/work/app_feature/src/lib.rs")),
            ShortPath {
                root: Some(r"app_feature\".to_owned()),
                rest: r"src\lib.rs".to_owned()
            }
        );
        assert_eq!(
            full_path(folder, Path::new("src/main.rs")),
            r"C:\work\app\src\main.rs"
        );
    }
}

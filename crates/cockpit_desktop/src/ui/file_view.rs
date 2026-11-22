//! The read-only file view on the right: tabs of the changed files, the diff, the whole file or a
//! markdown file rendered, and the line a `path:line` link marks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use cockpit_protocol::{AgentId, FileEdit, GitBlame};
use dioxus::dioxus_core::spawn_forever;
use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::daemon_label::DaemonPlace;
use crate::keymap::Command;
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::TerminalPlace;
use crate::render::{self, Diff, Language, MAX_RENDERED_LINES, diff_lines};
use crate::state::git::{self, DiffBase, GitHubLinks};
use crate::state::{
    FileFreshness, FileRead, FileUpdate, Loadable, Popover, Reread, TabMenuOrigin, ViewMode,
    actions, chat_images, file_edits, file_reread, file_tree, is_markdown, page_copy, tab_menu,
    terminals,
};
use crate::ui::code::{
    CopyButton, CopyLook, LineMarks, LineNumbers, diff_line, diff_row, lines_not_shown,
};
use crate::ui::file_git::{
    self, BlameCard, BlameCell, BlameHover, ChangeCount, CommitChange, HistoryButton, LineBlame,
};
use crate::ui::file_list::FileListButton;
use crate::ui::find::FileFindBox;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::markdown::MarkdownView;
use crate::ui::presence::use_agent_presence;
use crate::ui::splitter::FILE_VIEW_ID;
use crate::ui::terminal_pane::TerminalPane;
use crate::ui::zoom::ZoomImage;
use crate::{bridge, browser, format, page_files};

/// The shown tab's id, which keeps it in view in a long row of tabs.
const SHOWN_TAB_ID: &str = "file-tab-shown";

/// With Ctrl, closes the shown tab, or the highlighted file in the list of open files.
pub const CLOSE_TAB_KEY: &str = "w";

/// Unchanged lines kept around each change of the file view's diff.
pub(super) const FILE_DIFF_CONTEXT: usize = 3;

/// Above a diff of the edits after one too large to show.
const AFTER_TOO_LARGE_NOTE: &str =
    "An earlier edit of the file is too large to show, so this diff starts after it.";

/// Above the diff of a file in no git checkout.
const OUTSIDE_GIT_NOTE: &str = "Not in a git checkout: the diff shows only this agent's edits.";

/// How long "Updated just now" and the changed lines' highlight last after a read again.
const UPDATE_SHOWN_FOR: Duration = Duration::from_secs(10);

/// The file's text, which the find searches.
pub(super) const FILE_BODY_CLASS: &str = "file-body";

/// The placeholder lines shown while the file loads: the find waits for the text.
pub(super) const FILE_LOADING_CLASS: &str = "file-loading";

/// The widths of the placeholder lines shown while the file loads, in percent of the text's width.
const LOADING_LINE_WIDTHS: [u8; 8] = [46, 72, 64, 30, 81, 58, 69, 38];

/// The panel: the tabs of the files and of the terminal, when the terminal is one, and the shown
/// tab's content.
#[component]
pub fn FileViewPanel() -> Element {
    let cockpit: Cockpit = use_context();
    let (selected, terminal_open) = {
        let state = cockpit.state.read();
        let selected = state.ui.selected;
        let terminal_open =
            selected.filter(|agent| terminals::file_view_shows_terminal(&state, *agent));
        (selected, terminal_open)
    };
    // The panel stays as the window switches agents: it tells whether the terminal came with its
    // agent, and then draws at once.
    let terminal_instant =
        use_agent_presence(selected, terminal_open).is_some_and(|shown| shown.instant);
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent = entry.id();
    let view = state.ui.file_view(agent);
    let terminal_tab = terminals::in_file_view(&state, agent);
    if view.is_none() && !terminal_tab {
        return rsx! {};
    }
    let terminal_shown = terminals::file_view_shows_terminal(&state, agent);
    let folder = entry.summary.folder.clone();
    let changed_paths = entry.changed_paths();
    let tabs = view
        .map(|view| view.tabs(&changed_paths))
        .unwrap_or_default();
    let tab_count = tabs.len();
    let has_files = view.is_some();
    // The file whose tab is selected, which Ctrl+W closes. In the terminal, Ctrl+W is the shell's.
    let shown = view
        .filter(|_| !terminal_shown)
        .map(|view| view.path.clone());
    let closed_by_key = shown.clone();
    let list_menu = matches!(
        state.ui.popover,
        Some(Popover::TabMenu {
            origin: TabMenuOrigin::List,
            ..
        })
    );
    let list_open = state.ui.popover == Some(Popover::FileList) || list_menu;
    let labels = tab_labels(PathStyle::of(&folder), &tabs);
    drop(state);
    rsx! {
        section {
            id: FILE_VIEW_ID,
            class: "file-view",
            aria_label: "File view",
            // Focusable, so that a click in the view lets Ctrl+W close its tab.
            tabindex: -1,
            onkeydown: move |event| {
                let Some(shown) = closed_by_key.clone() else {
                    return;
                };
                let command = event.modifiers().ctrl() || event.modifiers().meta();
                if command && event.key() == Key::Character(CLOSE_TAB_KEY.to_owned()) {
                    event.prevent_default();
                    cockpit.update(|state| actions::close_tab(state, agent, &shown));
                }
            },
            div { class: "file-tabs",
                div { class: "tab-list scroll-x", role: "tablist", aria_label: "Files changed by this agent",
                    if terminal_tab {
                        TerminalTab { agent, selected: terminal_shown }
                    }
                    for (tab, label) in tabs.into_iter().zip(labels) {
                        FileTab {
                            key: "{tab.display()}",
                            agent,
                            selected: shown.as_ref() == Some(&tab),
                            full: full_path(&folder, &tab),
                            path: tab,
                            label,
                        }
                    }
                }
                if has_files {
                    FileListButton { agent, count: tab_count, open: list_open, covered: list_menu }
                }
                button {
                    class: "icon-button",
                    aria_label: "Close the file view",
                    title: "Close the file view",
                    onclick: move |_| cockpit.update(|state| actions::close_file_panel(state, agent)),
                    Icon { kind: IconKind::Close, size: 16 }
                }
            }
            if terminal_shown {
                TerminalPane { place: TerminalPlace::FileView, leaving: false, instant: terminal_instant }
            } else {
                FileShown { agent }
            }
        }
    }
}

/// The terminal's tab, before the files'. Its "×" hides the terminal, and its shells keep running.
#[component]
fn TerminalTab(agent: AgentId, selected: bool) -> Element {
    let cockpit: Cockpit = use_context();
    use_effect(use_reactive!(|selected| {
        if selected {
            bridge::reveal(SHOWN_TAB_ID);
        }
    }));
    rsx! {
        div {
            id: selected.then_some(SHOWN_TAB_ID),
            class: if selected { "file-tab terminal-tab-in-view selected" } else { "file-tab terminal-tab-in-view" },
            role: "tab",
            aria_selected: selected,
            onauxclick: move |event| {
                if event.trigger_button() == Some(MouseButton::Auxiliary) {
                    event.prevent_default();
                    cockpit.update(|state| terminals::hide(state, agent));
                }
            },
            button {
                class: "file-tab-name",
                onclick: move |_| cockpit.update(|state| terminals::show_tab(state, agent)),
                Icon { kind: IconKind::Terminal, size: 12 }
                "Terminal"
            }
            button {
                class: "file-tab-close",
                aria_label: "Hide the terminal",
                title: "Hide the terminal: its shells keep running",
                onclick: move |_| cockpit.update(|state| terminals::hide(state, agent)),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

/// The shown file: its bar, its text and its foot.
#[component]
fn FileShown(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let (Some(entry), Some(view)) = (state.agents.get(&agent), state.ui.file_view(agent).cloned())
    else {
        return rsx! {};
    };
    let folder = &entry.summary.folder;
    let text = entry.files.get(&view.path).cloned().unwrap_or_default();
    let absolute = git::absolute(entry, &view.path);
    let file_git = entry.git.files.get(&absolute);
    let base = DiffBase::of(file_git);
    // A blame of another commit than the text the view has, as while both are read again, waits.
    let blame = file_git
        .and_then(|file| file.blame.value.loaded().cloned())
        .filter(|blame| {
            matches!(&base, DiffBase::Git { head: Some(head), .. } if head.hash == blame.head)
        });
    let checkout = entry.git.checkout_of(&absolute);
    let github = checkout.and_then(GitHubLinks::of);
    let blame_github = checkout
        .zip(blame.as_ref())
        .and_then(|(checkout, blame)| GitHubLinks::of_blame(checkout, blame));
    let shown_commit = view
        .commit
        .as_ref()
        .filter(|shown| shown.path == view.path)
        .map(|shown| shown.hash.clone());
    let commit_change = shown_commit.as_ref().map(|hash| {
        file_git
            .and_then(|file| file.commits.get(hash).cloned())
            .unwrap_or(Loadable::Loading)
    });
    let history_open = matches!(
        state.ui.popover,
        Some(Popover::FileHistory { agent: shown, .. }) if shown == agent
    );
    let history_button = file_git::history_button(
        &base,
        matches!(text, Loadable::Loaded(FileRead::Text { .. })),
    );
    let freshness = entry.file_freshness.clone();
    let update = freshness.update.clone();
    let short = ShortPath::new(folder, &view.path);
    let full = full_path(folder, &view.path);
    let changed = entry.has_changed(&view.path);
    let edit_count = match &text {
        Loadable::Loaded(FileRead::Text { edits, .. }) => edits.len(),
        _ => 0,
    };
    let agent_note = if edit_count > 0 {
        format!(
            "Changed by {} of this agent.",
            format::count(edit_count, "edit", "edits")
        )
    } else if changed {
        "Changed by this agent.".to_owned()
    } else {
        "This agent did not change the file.".to_owned()
    };
    let note = match (&base, &text) {
        (DiffBase::Git { new: true, .. }, _) => {
            FootNote::Plain("New, not committed yet.".to_owned())
        }
        (
            DiffBase::Git {
                text: committed, ..
            },
            Loadable::Loaded(FileRead::Text { text, .. }),
        ) => FootNote::Changes {
            base: committed.clone(),
            text: text.clone(),
        },
        _ => FootNote::Plain(agent_note),
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
    let loading = matches!(text, Loadable::NotRequested | Loadable::Loading);
    let image = matches!(text, Loadable::Loaded(FileRead::Image(_)));
    let copied_text = match &text {
        Loadable::Loaded(FileRead::Text { text, .. }) => Some(text.clone()),
        _ => None,
    };
    let page_text = copied_text
        .as_ref()
        .filter(|_| page_copy::is_page(&full))
        .cloned();
    let on_this_computer = state
        .machines
        .get(MachineId::of(agent))
        .is_some_and(|machine| machine.daemon.place != DaemonPlace::Ssh);
    let reading_page = state.page_copies.is_reading(agent, &full);
    let wrap = state.prefs.file_view.wrap;
    let find_open = view.find;
    let body_layout = match (preview, wrap) {
        (true, _) => "scroll file-preview",
        (false, true) => "scroll mono wrap",
        (false, false) => "scroll mono",
    };
    rsx! {
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
                            class: if file_git::mode_chosen(mode, choice, shown_commit.is_some()) { "segment chosen" } else { "segment" },
                            aria_pressed: file_git::mode_chosen(mode, choice, shown_commit.is_some()),
                            onclick: move |_| cockpit.update(|state| actions::set_view_mode(state, agent, choice)),
                            "{name}"
                        }
                    }
                }
                if let Some(enabled) = history_button {
                    HistoryButton { agent, open: history_open, enabled }
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
                if let Some(text) = page_text {
                    OpenInBrowserButton { agent, path: full.clone(), text, on_this_computer, reading: reading_page }
                }
            }
        }
        div { class: "file-find-anchor",
            FileFindBox { agent, open: find_open }
        }
        div {
            class: "{FILE_BODY_CLASS} {body_layout}",
            // Focusable, so that after a click in the text End and Home scroll it.
            tabindex: -1,
            match text {
                Loadable::NotRequested | Loadable::Loading => rsx! {
                    div { class: FILE_LOADING_CLASS, aria_hidden: true,
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
                Loadable::Loaded(FileRead::Text { .. }) if commit_change.is_some() => rsx! {
                    CommitChange {
                        agent,
                        shown: commit_change.clone().unwrap_or_default(),
                        language_path: view.path.clone(),
                        github: github.clone(),
                    }
                },
                Loadable::Loaded(FileRead::Text { text, .. }) if preview => rsx! {
                    FilePreview { agent, folder: folder.clone(), path: view.path.clone(), text }
                },
                Loadable::Loaded(FileRead::Text { text, edits }) => rsx! {
                    FileContent {
                        agent,
                        path: view.path.clone(),
                        text,
                        mode,
                        line: view.line,
                        edits,
                        changed,
                        update,
                        base: base.clone(),
                        blame: blame.clone(),
                        github: blame_github.clone(),
                    }
                },
                Loadable::Loaded(FileRead::Image(image)) => {
                    let image = Arc::new(image);
                    let path = view.path.clone();
                    let open_full_view = {
                        let image = Arc::clone(&image);
                        move |()| {
                            let (path, image) = (path.clone(), Arc::clone(&image));
                            cockpit.update(|state| chat_images::open_full_view(state, agent, path, image));
                        }
                    };
                    rsx! {
                        div { class: "file-image",
                            // Keyed by the path, so that another file does not show for a
                            // frame at the last one's zoom.
                            for shown in [view.path.clone()] {
                                ZoomImage {
                                    key: "{shown.display()}",
                                    image: Arc::clone(&image),
                                    alt: full.clone(),
                                    on_click: open_full_view.clone(),
                                }
                            }
                        }
                    }
                }
            }
        }
        FileFoot { agent, note, freshness }
    }
}

/// Opens the web page the view shows, at `path`, in the browser. A page on this computer opens
/// as it is, with every file it loads. The browser cannot reach a server's: the daemon reads the
/// files the page names, and the browser opens a copy ([`use_page_copies`]).
#[component]
fn OpenInBrowserButton(
    agent: AgentId,
    path: String,
    text: String,
    on_this_computer: bool,
    reading: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let title = match (on_this_computer, reading) {
        (true, _) => "Open the page in the browser",
        (false, false) => "Open a copy of the page and of the files it names in the browser",
        (false, true) => "Reading the files the page names…",
    };
    rsx! {
        button {
            class: "icon-button framed",
            aria_label: "Open in the browser",
            aria_busy: reading,
            disabled: reading,
            title,
            onclick: move |_| {
                if on_this_computer {
                    let page = PathBuf::from(&path);
                    // Not tied to the button, which may leave the screen before the opener is done.
                    spawn_forever(async move {
                        if let Err(error) = browser::open_page(page).await {
                            cockpit.update(|state| state.ui.report(format!("Cannot open the page: {error}")));
                        }
                    });
                } else {
                    let text = text.clone();
                    cockpit.change(|state| page_copy::start(state, agent, &path, text));
                }
            },
            if reading {
                span { class: "spinner", role: "status", aria_label: "Reading the files the page names" }
            } else {
                Icon { kind: IconKind::External, size: 15 }
            }
        }
    }
}

/// Writes each copy of a server's page once the daemon answered for all its files, then opens it
/// in the browser.
pub fn use_page_copies(cockpit: Cockpit) {
    let any_ready = use_memo(move || cockpit.state.read().page_copies.any_ready());
    use_effect(move || {
        if !any_ready() {
            return;
        }
        let mut state = cockpit.state;
        let copies = page_copy::take_ready(&mut state.write());
        for mut copy in copies {
            spawn_forever(async move {
                let mut missing = std::mem::take(&mut copy.missing);
                match page_files::open_copy(copy).await {
                    Ok(refused) => {
                        missing.extend(refused);
                        if let Some(note) = page_copy::missing_note(&missing) {
                            cockpit.update(|state| state.ui.inform(note));
                        }
                    }
                    Err(error) => {
                        cockpit.update(|state| {
                            state.ui.report(format!("Cannot open the page: {error}"))
                        });
                    }
                }
            });
        }
    });
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
    let menu_path = path.clone();
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
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                let path = menu_path.clone();
                cockpit.update(|state| tab_menu::open(state, agent, &path, x, y, TabMenuOrigin::Tabs));
            },
            button {
                class: "file-tab-name",
                onclick: move |_| {
                    let path = path.clone();
                    cockpit.update(|state| actions::show_file(state, agent, path));
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
        let root = file_tree::outside_root(folder, Path::new(path.as_ref()));
        let rest = style
            .strip_prefix(&path, &root.to_string_lossy())
            .unwrap_or(&path)
            .to_owned();
        Self {
            root: Some(file_tree::root_label(&root)),
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

/// The rows are rebuilt only when a prop changes.
/// `changed` says whether the agent changed the file, even with no edits to show. `base` is what
/// the diff compares the file with, `blame` who last changed its committed lines, and `github`
/// the links to the blame's commits.
#[component]
fn FileContent(
    agent: AgentId,
    path: PathBuf,
    text: String,
    mode: ViewMode,
    line: Option<u32>,
    edits: Vec<FileEdit>,
    changed: bool,
    update: Option<FileUpdate>,
    base: DiffBase,
    blame: Option<GitBlame>,
    github: Option<GitHubLinks>,
) -> Element {
    let hover = use_signal(|| None::<BlameHover>);
    let on_card = use_signal(|| false);
    let entered = use_signal(|| None::<u64>);
    let language = Language::from_path(&path);
    let marks = line_marks(line, update.as_ref());
    if mode == ViewMode::Diff {
        match &base {
            DiffBase::Waiting => {
                return rsx! {
                    div { class: FILE_LOADING_CLASS, aria_hidden: true,
                        for (index, width) in LOADING_LINE_WIDTHS.iter().enumerate() {
                            div { key: "{index}", class: "skeleton skeleton-line", style: "width: {width}%" }
                        }
                    }
                };
            }
            DiffBase::Git {
                head,
                text: committed,
                new,
            } => {
                let note = file_git::since_note(head.as_ref(), *new);
                return file_git::git_diff(committed, &text, note, language, marks);
            }
            DiffBase::Edits { .. } => {}
        }
    }
    let outside = base == DiffBase::Edits { outside: true };
    let shown = file_edits::after_too_large(&edits);
    let too_large = shown.len() < edits.len();
    if mode == ViewMode::WholeFile || shown.is_empty() {
        let lines = render::plain_lines(&text, language, 1, MAX_RENDERED_LINES);
        let blames: Vec<BlameCell> = if mode == ViewMode::WholeFile {
            file_git::line_blames(&base, blame.as_ref(), &text, SystemTime::now())
                .into_iter()
                .map(|LineBlame { text, commit }| BlameCell {
                    text,
                    commit,
                    hover,
                    on_card,
                    entered,
                })
                .collect()
        } else {
            Vec::new()
        };
        let commits = blame.map(|blame| blame.commits).unwrap_or_default();
        return rsx! {
            if mode == ViewMode::Diff && outside {
                p { class: "file-note", "{OUTSIDE_GIT_NOTE}" }
            }
            if mode == ViewMode::Diff && too_large {
                p { class: "file-note", "This agent's last edit of the file is too large to show. Here is its text." }
            } else if mode == ViewMode::Diff && changed {
                p { class: "file-note", "This agent changed the file, but its edits are not available. Here is its text." }
            } else if mode == ViewMode::Diff {
                p { class: "file-note", "This agent did not change the file. Here is its text." }
            }
            div { class: "diff-rows",
                for (index, row) in lines.rows.iter().enumerate() {
                    {diff_line(row, LineNumbers::Shown, marks, blames.get(index).cloned())}
                }
            }
            {lines_not_shown(lines.more)}
            if !commits.is_empty() || !blames.is_empty() {
                BlameCard { agent, hover, on_card, commits, github }
            }
        };
    }
    let edit_refs: Vec<&FileEdit> = shown.iter().collect();
    let outside_note = rsx! {
        if outside {
            p { class: "file-note", "{OUTSIDE_GIT_NOTE}" }
        }
    };
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
                if too_large {
                    p { class: "file-note", "{AFTER_TOO_LARGE_NOTE}" }
                }
                p { class: "file-note", "The file is back to its text before the edits." }
            };
        }
        return rsx! {
            {outside_note}
            if too_large {
                p { class: "file-note", "{AFTER_TOO_LARGE_NOTE}" }
            }
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
    let count = shown.len();
    let mut left = MAX_RENDERED_LINES;
    let mut diffs: Vec<Diff> = Vec::with_capacity(count);
    for edit in shown {
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
        if too_large {
            p { class: "file-note", "{AFTER_TOO_LARGE_NOTE}" }
        }
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

/// What the foot says of the file.
#[derive(Debug, Clone, PartialEq)]
enum FootNote {
    Plain(String),
    /// Its uncommitted changes, counted against the last commit's text.
    Changes {
        base: String,
        text: String,
    },
}

/// The foot: what changed in the file, when the shown text was read, and Reload.
#[component]
fn FileFoot(agent: AgentId, note: FootNote, freshness: FileFreshness) -> Element {
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
    let reload_keys = cockpit.state.read().prefs.keys.shown(Command::ReloadFile);
    rsx! {
        div { class: "file-foot",
            span { class: "file-foot-note",
                match note {
                    FootNote::Plain(text) => rsx! { "{text}" },
                    FootNote::Changes { base, text } => rsx! {
                        ChangeCount { base, text }
                    },
                }
            }
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
                title: "Read the file again ({reload_keys})",
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

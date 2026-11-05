//! The read-only file view on the right: tabs of the changed files, the diff or the whole file,
//! and the line a `path:line` link marks.

use std::path::{Path, PathBuf};

use cockpit_protocol::{AgentId, FileEdit};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::render::{self, Diff, Language, MAX_RENDERED_LINES, diff_lines};
use crate::state::{Loadable, ViewMode, actions, file_edits};
use crate::ui::code::{LineNumbers, diff_line, diff_row, lines_not_shown};
use crate::ui::icons::{Icon, IconKind};

/// Unchanged lines kept around each change of the file view's diff.
const FILE_DIFF_CONTEXT: usize = 3;

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
    let changed = entry
        .details
        .loaded()
        .into_iter()
        .flat_map(|details| details.changed_files.iter())
        .map(|file| file_edits::relative_path(folder, &file.path));
    // Each path once: the daemon's list does not promise it, and the paths key the tabs.
    let mut tabs: Vec<PathBuf> = Vec::new();
    for path in changed.chain([view.path.clone()]) {
        if !tabs.contains(&path) {
            tabs.push(path);
        }
    }
    let text = entry.files.get(&view.path).cloned().unwrap_or_default();
    let edits: Vec<FileEdit> = file_edits::edits_for(&entry.transcript, folder, &view.path)
        .into_iter()
        .cloned()
        .collect();
    let label = match view.line {
        Some(line) => format!("{}, line {line}", view.path.display()),
        None => view.path.display().to_string(),
    };
    let note = if edits.is_empty() {
        "This agent did not change the file.".to_owned()
    } else {
        format!(
            "Changed by {} of this agent.",
            format::count(edits.len(), "edit", "edits")
        )
    };
    let diff = view.mode == ViewMode::Diff;
    rsx! {
        section { class: "file-view", aria_label: "File view",
            div { class: "file-tabs",
                div { class: "tab-list scroll-x", role: "tablist", aria_label: "Files changed by this agent",
                    for tab in tabs {
                        FileTab { key: "{tab.display()}", agent, selected: tab == view.path, path: tab }
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
                span { class: "file-label mono", "{label}" }
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
            }
            div { class: "file-body scroll mono",
                match text {
                    Loadable::NotRequested | Loadable::Loading => rsx! {
                        p { class: "file-note", "Loading the file" }
                    },
                    Loadable::Failed(error) => rsx! {
                        p { class: "file-note", "Cannot read the file: {error}" }
                    },
                    Loadable::Loaded(text) => rsx! {
                        FileContent { path: view.path.clone(), text, mode: view.mode, line: view.line, edits }
                    },
                }
            }
            div { class: "file-foot", "{note} To change it, ask the agent." }
        }
    }
}

#[component]
fn FileTab(agent: AgentId, path: PathBuf, selected: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let name = file_name(&path);
    let full = path.display().to_string();
    rsx! {
        button {
            class: if selected { "file-tab selected" } else { "file-tab" },
            role: "tab",
            aria_selected: selected,
            title: "{full}",
            onclick: move |_| {
                let path = path.clone();
                cockpit.update(|state| actions::open_file(state, agent, path, None));
            },
            "{name}"
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
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
            for row in lines.rows.iter() {
                {diff_line(row, LineNumbers::Shown, line)}
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
            for row in diff.rows.iter() {
                {diff_row(row, LineNumbers::Shown, line)}
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
            for row in diff.rows.iter() {
                {diff_row(row, LineNumbers::Hidden, None)}
            }
            {lines_not_shown(diff.more)}
        }
    }
}

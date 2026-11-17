//! The button at the end of the file view's tabs, with the count of open files, and the list it
//! opens: every open file, filtered by a field of its own, to show one or close one.

use std::path::{Path, PathBuf};

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::bridge::{self, HIGHLIGHTED_CLASS, HIGHLIGHTED_ENTRY_ID};
use crate::connection::Cockpit;
use crate::state::actions::Direction;
use crate::state::{file_list, terminals};
use crate::ui::dialogs::{PointerMoves, use_pointer_moves};
use crate::ui::file_view::{self, CLOSE_TAB_KEY, ShortPath};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::Presence;

/// The list's filter, which takes the focus when the list opens and gets it back after a close.
pub const FILTER_ID: &str = "file-list-filter";

#[component]
pub fn FileListButton(agent: AgentId, count: usize, open: bool, covered: bool) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div { class: "file-list-anchor",
            button {
                class: if open { "file-list-button open" } else { "file-list-button" },
                aria_expanded: open,
                aria_label: "All open files: {count}",
                title: "All open files",
                onclick: move |_| cockpit.update_with_drafts(|state, drafts| file_list::toggle(state, drafts, agent)),
                span { class: "file-list-count", "{count}" }
                Icon { kind: IconKind::ChevronDown, size: 12 }
            }
            Presence { open, FileList { agent, open, covered } }
        }
    }
}

/// The pointer's part in the highlight, which the rows share.
#[derive(Clone, Copy)]
struct ListPointer {
    moves: PointerMoves,
    /// The row the pointer highlighted last, until the scroll effect sees it. It is in view
    /// already: scrolling to it would move the list under the pointer.
    pointed: CopyValue<Option<usize>>,
}

impl ListPointer {
    fn point(mut self, cockpit: Cockpit, agent: AgentId, index: usize, event: &MouseEvent) {
        // Writing the drafts renders their readers: only a new row does.
        if self.moves.moved(event) && cockpit.drafts.peek().file_list.highlighted != index {
            let took = cockpit.with_drafts(|state, drafts| {
                file_list::point(state, drafts, agent, index);
                drafts.file_list.highlighted == index
            });
            if took {
                self.pointed.set(Some(index));
            }
        }
    }
}

/// One row: the file's name and its folders, a check on the shown file.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    path: PathBuf,
    name: String,
    folders: String,
    full: String,
    shown: bool,
}

fn rows(folder: &Path, shown: Option<&Path>, paths: Vec<PathBuf>) -> Vec<Row> {
    paths
        .into_iter()
        .map(|path| {
            let short = ShortPath::new(folder, &path);
            let (name, folders) = short.name_and_folders();
            Row {
                name: name.to_owned(),
                folders,
                full: file_view::full_path(folder, &path),
                shown: Some(path.as_path()) == shown,
                path,
            }
        })
        .collect()
}

#[component]
fn FileList(agent: AgentId, open: bool, covered: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let highlighted = use_memo(move || cockpit.drafts.read().file_list.highlighted);
    let moves = use_pointer_moves();
    let mut pointed = use_hook(|| CopyValue::new(None));
    use_context_provider(|| ListPointer { moves, pointed });
    // A row's menu took the focus: the keys go back to the filter once it closes to the list.
    use_effect(use_reactive!(|open, covered| {
        if open && !covered {
            bridge::focus_field(FILTER_ID);
        }
    }));
    use_effect(move || {
        let index = highlighted();
        if pointed.replace(None) != Some(index) {
            bridge::reveal(HIGHLIGHTED_ENTRY_ID);
        }
    });
    let rows = {
        let state = cockpit.state.read();
        let drafts = cockpit.drafts.read();
        let (Some(entry), Some(_)) = (state.agents.get(&agent), state.ui.file_view(agent)) else {
            return rsx! {};
        };
        let paths = file_list::rows(&state, &drafts, agent);
        rows(
            &entry.summary.folder,
            terminals::shown_file(&state, agent),
            paths,
        )
    };
    let highlighted = highlighted();
    rsx! {
        div {
            class: if covered { "popover file-list covered" } else { "popover file-list" },
            role: "dialog",
            aria_label: "Open files",
            div { class: "palette-head",
                Icon { kind: IconKind::Search, size: 14 }
                Filter { agent }
            }
            div { class: "file-list-rows scroll", role: "listbox", aria_label: "Open files",
                if rows.is_empty() {
                    span { class: "palette-empty", "No open file matches." }
                }
                for (index, row) in rows.into_iter().enumerate() {
                    FileRow { key: "{row.path.display()}", agent, index, row, highlighted: index == highlighted }
                }
            }
            span { class: "palette-foot",
                "Up and Down to move, Enter to show, Ctrl+W to close the file, Esc to close the list"
            }
        }
    }
}

/// The list's filter. It follows the keyboard alone: a render that lags behind typing would put
/// an older filter back.
#[component]
fn Filter(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        input {
            id: FILTER_ID,
            class: "palette-input",
            r#type: "text",
            aria_label: "Filter the open files",
            placeholder: "Filter the open files",
            initial_value: "",
            onmounted: move |event| async move {
                // The list may have closed already: nothing to focus then.
                let _ = event.data().set_focus(true).await;
            },
            oninput: move |event: FormEvent| {
                cockpit.with_drafts(|state, drafts| file_list::filter(state, drafts, agent, event.value()));
            },
            onkeydown: move |event: KeyboardEvent| {
                let command = event.modifiers().ctrl() || event.modifiers().meta();
                match event.key() {
                    // The keys move the highlight, and the filter's caret stays.
                    Key::ArrowDown => {
                        event.prevent_default();
                        cockpit.with_drafts(|state, drafts| file_list::move_highlight(state, drafts, agent, Direction::Down));
                    }
                    Key::ArrowUp => {
                        event.prevent_default();
                        cockpit.with_drafts(|state, drafts| file_list::move_highlight(state, drafts, agent, Direction::Up));
                    }
                    Key::Enter => {
                        event.prevent_default();
                        cockpit.update_with_drafts(|state, drafts| file_list::pick(state, drafts, agent, None));
                    }
                    // The file view closes its shown file on Ctrl+W: here the list does.
                    Key::Character(letter) if command && letter == CLOSE_TAB_KEY => {
                        event.prevent_default();
                        event.stop_propagation();
                        cockpit.update_with_drafts(|state, drafts| file_list::close(state, drafts, agent, None));
                    }
                    _ => {}
                }
            },
        }
    }
}

#[component]
fn FileRow(agent: AgentId, index: usize, row: Row, highlighted: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let pointer: ListPointer = use_context();
    let mut class = vec!["file-list-row"];
    if highlighted {
        class.push(HIGHLIGHTED_CLASS);
    }
    if row.shown {
        class.push("shown");
    }
    let class = class.join(" ");
    let name = row.name;
    rsx! {
        div {
            id: highlighted.then_some(HIGHLIGHTED_ENTRY_ID),
            class,
            role: "option",
            aria_selected: row.shown,
            title: "{row.full}",
            onmousemove: move |event: MouseEvent| pointer.point(cockpit, agent, index, &event),
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                cockpit.update_with_drafts(|state, drafts| file_list::open_menu(state, drafts, agent, index, x, y));
            },
            button {
                class: "file-list-pick",
                onclick: move |_| cockpit.update_with_drafts(|state, drafts| file_list::pick(state, drafts, agent, Some(index))),
                span { class: "file-list-check",
                    if row.shown {
                        Icon { kind: IconKind::Check, size: 14 }
                    }
                }
                span { class: "file-list-name mono", "{name}" }
                span { class: "file-list-folders mono",
                    span { class: "file-path-text", "{row.folders}" }
                }
            }
            button {
                class: "file-list-close",
                aria_label: "Close {name}",
                title: "Close (Ctrl+W)",
                onclick: move |_| {
                    cockpit.update_with_drafts(|state, drafts| file_list::close(state, drafts, agent, Some(index)));
                    // The button took the focus: the keys go back to the filter.
                    bridge::focus_field(FILTER_ID);
                },
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_shows_the_name_apart_from_its_folders() {
        let folder = Path::new("/work/app");
        let paths = vec![
            PathBuf::from("src/state/apply.rs"),
            PathBuf::from("AGENTS.md"),
            PathBuf::from("/work/app_feature/src/lib.rs"),
        ];

        let rows = rows(folder, Some(Path::new("AGENTS.md")), paths);

        let parts: Vec<(&str, &str, bool)> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.folders.as_str(), row.shown))
            .collect();
        assert_eq!(
            parts,
            [
                ("apply.rs", "src/state", false),
                ("AGENTS.md", "", true),
                ("lib.rs", "app_feature/src", false),
            ]
        );
        assert_eq!(rows[0].full, "/work/app/src/state/apply.rs");
    }
}

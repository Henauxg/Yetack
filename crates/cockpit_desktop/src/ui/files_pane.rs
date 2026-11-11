//! The files pane at the window's right edge: the selected agent's folder as a tree, with what the
//! agent changed and read. Ctrl+B or the header's files chip opens and closes it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::bridge::{self, COMPOSER_ID, HIGHLIGHTED_CLASS, HighlightedList};
use crate::connection::Cockpit;
use crate::keymap::Command;
use crate::path_style::{self, PathStyle};
use crate::state::actions::Direction;
use crate::state::file_tree::{self, RowKind, Sideways, TreeFilter, TreeMenuItem, TreeRow};
use crate::state::{AppState, Popover, file_edits};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;
use crate::ui::splitter::FILES_PANE_ID;

/// The search field, which keeps its text per agent.
const SEARCH_ID: &str = "files-search";

/// How long typing pauses before the search runs.
const SEARCH_PAUSE: Duration = Duration::from_millis(300);

/// How far each level of the tree is indented, in pixels.
const INDENT: usize = 14;

/// How many characters end a name whatever the pane's width: enough for "component.ts".
const TAIL_CHARS: usize = 12;

const ZERO_WIDTH_JOINER: char = '\u{200D}';

#[component]
pub fn FilesPane() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent = entry.id();
    let folder = entry.summary.folder.clone();
    let footprint = file_tree::footprint(entry);
    let rows = file_tree::rows(&entry.tree, &folder, &footprint);
    let filter = entry.tree.filter;
    let highlighted = entry.tree.highlighted.clone();
    let search = entry
        .tree
        .search
        .as_ref()
        .map(|search| search.name.clone())
        .unwrap_or_default();
    let style = PathStyle::of(&folder);
    let shown_file = state.ui.file_view(agent).map(|view| {
        PathBuf::from(style.resolve(&folder.to_string_lossy(), &view.path.to_string_lossy()))
    });
    let changed = footprint.changed.len();
    let read = footprint.read.len();
    let full_folder = path_style::shown(&folder);
    let name = style
        .file_name(&folder.to_string_lossy())
        .map_or_else(|| full_folder.clone(), str::to_owned);
    let any_open = file_tree::can_collapse(&entry.tree, &folder, &footprint);
    let key_rows = rows.clone();
    let can_reveal = state
        .ui
        .file_view(agent)
        .is_some_and(|view| file_tree::can_show(&state, agent, &view.path));
    let reveal_due = file_tree::reveal_due(&entry.tree, &rows);
    let pane_keys = state.prefs.keys.shown(Command::FilesPane);
    rsx! {
        aside { id: FILES_PANE_ID, class: "files-pane", aria_label: "Files",
            div { class: "files-head",
                span { class: "files-root", title: "{full_folder}", "{name}" }
                button {
                    class: "icon-button tiny",
                    aria_label: "Show the file view's file",
                    title: if can_reveal || shown_file.is_none() { "Show the file view's file" } else { "The tree does not hold the file view's file" },
                    disabled: !can_reveal,
                    onclick: move |_| cockpit.update(|state| file_tree::reveal(state, agent)),
                    Icon { kind: IconKind::Target, size: 14 }
                }
                button {
                    class: "icon-button tiny",
                    aria_label: "Refresh",
                    title: "Refresh",
                    onclick: move |_| cockpit.update(|state| {
                        if let Some(entry) = state.agents.get_mut(&agent) {
                            file_tree::refresh(&mut entry.tree);
                        }
                    }),
                    Icon { kind: IconKind::Reconnect, size: 14 }
                }
                button {
                    class: "icon-button tiny",
                    aria_label: "Close every folder",
                    title: "Close every folder",
                    disabled: !any_open,
                    onclick: move |_| cockpit.update(|state| file_tree::collapse_all(state, agent)),
                    Icon { kind: IconKind::Collapse, size: 14 }
                }
                button {
                    class: "icon-button tiny",
                    aria_label: "Close the files pane",
                    title: "Close ({pane_keys})",
                    onclick: move |_| cockpit.update(file_tree::toggle_pane),
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            SearchField { key: "{agent.get()}", agent, searched: search }
            div { class: "files-filters", role: "group", aria_label: "Show",
                FilterChip { agent, filter: TreeFilter::All, chosen: filter, label: "All".to_owned() }
                FilterChip { agent, filter: TreeFilter::Changed, chosen: filter, label: format!("Changed {changed}") }
                FilterChip { agent, filter: TreeFilter::Read, chosen: filter, label: format!("Read {read}") }
            }
            div {
                class: "files-tree scroll",
                role: "tree",
                "data-own-keys": true,
                aria_label: "Files of {name}",
                tabindex: 0,
                onkeydown: move |event| tree_key(cockpit, agent, &key_rows, &event),
                for row in rows {
                    TreeRowView {
                        key: "{row.kind:?}{row.path.display()}{row.name}",
                        agent,
                        highlighted: highlighted.as_ref() == Some(&row.path) && row.kind.is_entry(),
                        shown: shown_file.as_ref() == Some(&row.path),
                        row,
                    }
                }
            }
            div { class: "files-foot",
                span { "{changed} changed, {read} read" }
                span { "{pane_keys}" }
            }
            RevealScroll { agent, due: reveal_due }
        }
    }
}

/// Scrolls the tree to the row `file_tree::reveal` highlighted, once the row shows: its folder
/// may need a listing first, and the pane may open in the same change.
#[component]
fn RevealScroll(agent: AgentId, due: bool) -> Element {
    let cockpit: Cockpit = use_context();
    use_effect(use_reactive!(|agent, due| {
        if due {
            bridge::scroll_to_highlighted_now(HighlightedList::FilesPane);
            cockpit.update(|state| file_tree::revealed(state, agent));
        }
    }));
    rsx! {}
}

fn tree_key(cockpit: Cockpit, agent: AgentId, rows: &[TreeRow], event: &KeyboardEvent) {
    let modifiers = event.modifiers();
    let plain = !modifiers.ctrl() && !modifiers.meta() && !modifiers.shift();
    if !plain {
        return;
    }
    let alt = modifiers.alt();
    match event.key() {
        Key::ArrowDown if !alt => {
            event.prevent_default();
            cockpit.update(|state| file_tree::move_highlight(state, agent, Direction::Down));
            bridge::scroll_to_highlighted(HighlightedList::FilesPane);
        }
        Key::ArrowUp if !alt => {
            event.prevent_default();
            cockpit.update(|state| file_tree::move_highlight(state, agent, Direction::Up));
            bridge::scroll_to_highlighted(HighlightedList::FilesPane);
        }
        Key::ArrowRight if !alt => {
            event.prevent_default();
            cockpit.update(|state| file_tree::step_sideways(state, agent, Sideways::Right));
            bridge::scroll_to_highlighted(HighlightedList::FilesPane);
        }
        Key::ArrowLeft if !alt => {
            event.prevent_default();
            cockpit.update(|state| file_tree::step_sideways(state, agent, Sideways::Left));
            bridge::scroll_to_highlighted(HighlightedList::FilesPane);
        }
        Key::Enter => {
            let highlighted = cockpit
                .state
                .peek()
                .agents
                .get(&agent)
                .and_then(|entry| entry.tree.highlighted.clone());
            let Some(row) = rows
                .iter()
                .find(|row| Some(&row.path) == highlighted.as_ref() && row.kind.is_entry())
                .cloned()
            else {
                return;
            };
            event.prevent_default();
            if alt {
                mention(cockpit, agent, &row.path);
            } else {
                cockpit.update(|state| file_tree::activate(state, agent, &row));
            }
        }
        _ => {}
    }
}

/// Puts the path in the agent's message, and the focus in the composer.
fn mention(cockpit: Cockpit, agent: AgentId, path: &Path) {
    let path = path.to_owned();
    cockpit.update_with_drafts(|state, drafts| file_tree::mention(state, drafts, agent, &path));
    bridge::focus_field_at_end(COMPOSER_ID);
}

#[component]
fn FilterChip(agent: AgentId, filter: TreeFilter, chosen: TreeFilter, label: String) -> Element {
    let cockpit: Cockpit = use_context();
    let is_chosen = filter == chosen;
    rsx! {
        button {
            class: if is_chosen { "filter-chip chosen" } else { "filter-chip" },
            aria_pressed: is_chosen,
            onclick: move |_| cockpit.update(|state| file_tree::set_filter(state, agent, filter)),
            "{label}"
        }
    }
}

/// The search field. Its text stays here while typing, and reaches the tree once typing pauses.
/// `searched` is the tree's search: when a filter or "Show the file" clears it, the field empties.
#[component]
fn SearchField(agent: AgentId, searched: String) -> Element {
    let cockpit: Cockpit = use_context();
    let mut text = use_signal(|| searched.clone());
    // Spaces typed at the ends are not in the search: they do not count as a change.
    use_effect(use_reactive!(|searched| {
        if text.peek().trim() != searched {
            text.set(searched);
        }
    }));
    rsx! {
        div { class: "files-search",
            Icon { kind: IconKind::Search, size: 13 }
            input {
                id: SEARCH_ID,
                class: "files-search-field",
                r#type: "search",
                placeholder: "Find a file",
                aria_label: "Find a file by name",
                spellcheck: false,
                autocomplete: "off",
                value: "{text}",
                oninput: move |event| {
                    let typed = event.value();
                    text.set(typed.clone());
                    spawn(async move {
                        tokio::time::sleep(SEARCH_PAUSE).await;
                        // A filter chosen meanwhile emptied the field, and wins.
                        if *text.peek() == typed {
                            cockpit.update(|state| file_tree::set_search(state, agent, &typed));
                        }
                    });
                },
                onkeydown: move |event| {
                    if event.key() == Key::Escape && !text.peek().is_empty() {
                        event.stop_propagation();
                        text.set(String::new());
                        cockpit.update(|state| file_tree::set_search(state, agent, ""));
                    }
                },
            }
        }
    }
}

#[component]
fn TreeRowView(agent: AgentId, row: TreeRow, highlighted: bool, shown: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let indent = row.depth * INDENT;
    match row.kind {
        RowKind::Section => {
            return rsx! {
                div { class: "tree-section", role: "presentation", "{row.name}" }
            };
        }
        RowKind::Loading => {
            return rsx! {
                div { class: "tree-loading", style: "--indent: {indent}px", aria_hidden: true,
                    div { class: "skeleton skeleton-line" }
                }
            };
        }
        RowKind::Note => {
            return rsx! {
                div { class: "tree-note", style: "--indent: {indent}px", "{row.name}" }
            };
        }
        RowKind::Folder { .. } | RowKind::File => {}
    }
    let mut classes = vec!["tree-row"];
    if highlighted {
        classes.push(HIGHLIGHTED_CLASS);
    }
    if shown {
        classes.push("shown");
    }
    if row.marks.ignored {
        classes.push("ignored");
    }
    if row.marks.change.is_some() {
        classes.push("changed");
    }
    let open = matches!(row.kind, RowKind::Folder { open: true, .. });
    let is_folder = matches!(row.kind, RowKind::Folder { .. });
    let full = path_style::shown(&row.path);
    let (head, tail) = split_tail(&row.name);
    let clicked = row.clone();
    let pointed = row.path.clone();
    let menu_path = row.path.clone();
    rsx! {
        div {
            class: classes.join(" "),
            role: "treeitem",
            aria_selected: highlighted,
            aria_expanded: is_folder.then_some(open),
            title: "{full}",
            style: "--indent: {indent}px",
            onclick: move |_| cockpit.update(|state| file_tree::activate(state, agent, &clicked)),
            onmousemove: move |_| {
                let pointed = pointed.clone();
                let already = cockpit
                    .state
                    .peek()
                    .agents
                    .get(&agent)
                    .is_some_and(|entry| entry.tree.highlighted.as_ref() == Some(&pointed));
                if !already {
                    cockpit.update(|state| file_tree::highlight(state, agent, &pointed));
                }
            },
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                let path = menu_path.clone();
                cockpit.update(|state| file_tree::open_menu(state, agent, &path, x, y));
            },
            span { class: if open { "tree-chevron open" } else { "tree-chevron" }, aria_hidden: true,
                if is_folder {
                    Icon { kind: IconKind::ChevronRight, size: 12 }
                }
            }
            span { class: "tree-icon", aria_hidden: true,
                Icon { kind: if is_folder { IconKind::Folder } else { IconKind::File }, size: 14 }
            }
            span { class: "tree-name",
                span { class: "tree-name-head", "{head}" }
                span { class: "tree-name-tail", "{tail}" }
            }
            if let Some(change) = row.marks.change {
                span { class: "tree-change mono",
                    span { class: "added", "+{change.added}" }
                    span { class: "removed", "-{change.removed}" }
                }
            }
            if row.marks.read && row.marks.change.is_none() {
                span { class: "tree-read", title: "Read by the agent" }
            }
            if row.marks.holds_change {
                span { class: "tree-holds", title: "Holds a file the agent changed" }
            }
        }
    }
}

/// A row's menu: open it, open its diff, put its path in the message, or copy its path.
#[component]
pub fn TreeMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = {
        let state = cockpit.state.read();
        match state.ui.popover {
            Some(Popover::TreeMenu { agent, x, y }) => menu_target(&state, agent, x, y),
            _ => None,
        }
    };
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let target = shown.value.clone();
    // A menu opened on another row starts with nothing highlighted.
    let key = format!(
        "{}-{}-{}-{}",
        shown.opening,
        target.agent.get(),
        target.x,
        target.y
    );
    let folder_open = matches!(target.row.kind, RowKind::Folder { open: true, .. });
    let rows: Vec<FlatRow> = target
        .items
        .iter()
        .map(|&item| {
            let (label, title) = match item {
                TreeMenuItem::Open if folder_open => ("Close", "Enter in the tree does this"),
                TreeMenuItem::Open => ("Open", "Enter in the tree does this"),
                TreeMenuItem::OpenDiff => ("Open on its diff", ""),
                TreeMenuItem::Mention => {
                    ("Mention in the message", "Alt+Enter in the tree does this")
                }
                TreeMenuItem::CopyPath => ("Copy the path", ""),
                TreeMenuItem::CopyRelativePath => ("Copy the relative path", ""),
            };
            FlatRow {
                label,
                key: item.key(),
                enabled: true,
                title: title.to_owned(),
                divided: item == TreeMenuItem::CopyPath,
            }
        })
        .collect();
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                FlatMenu {
                    key: "{key}",
                    x: target.x,
                    y: target.y,
                    rows: rows.clone(),
                    focus_back: None,
                    on_pick: {
                        let target = target.clone();
                        move |index: usize| {
                            if let Some(&item) = target.items.get(index) {
                                run_menu_item(cockpit, &target, item);
                            }
                        }
                    },
                }
            }
        }
    }
}

fn run_menu_item(cockpit: Cockpit, target: &MenuTarget, item: TreeMenuItem) {
    let agent = target.agent;
    let row = &target.row;
    let close = |state: &mut AppState| state.ui.popover = None;
    match item {
        TreeMenuItem::Open => cockpit.update(|state| {
            close(state);
            file_tree::activate(state, agent, row);
        }),
        TreeMenuItem::OpenDiff => cockpit.update(|state| {
            close(state);
            file_tree::open_diff(state, agent, &row.path);
        }),
        TreeMenuItem::Mention => mention(cockpit, agent, &row.path),
        TreeMenuItem::CopyPath => {
            bridge::copy_to_clipboard(path_style::shown(&row.path));
            cockpit.update(close);
        }
        TreeMenuItem::CopyRelativePath => {
            if let Some(relative) = &target.relative {
                bridge::copy_to_clipboard(relative.clone());
            }
            cockpit.update(close);
        }
    }
}

/// What the open menu is about.
#[derive(Debug, Clone, PartialEq)]
struct MenuTarget {
    agent: AgentId,
    x: i32,
    y: i32,
    row: TreeRow,
    /// The path from the agent's folder, for an entry inside it.
    relative: Option<String>,
    items: Vec<TreeMenuItem>,
}

fn menu_target(state: &AppState, agent: AgentId, x: i32, y: i32) -> Option<MenuTarget> {
    let entry = state.agents.get(&agent)?;
    let path = entry.tree.menu.as_ref()?;
    let row = file_tree::shown_rows(state, agent)
        .into_iter()
        .find(|row| &row.path == path && row.kind.is_entry())?;
    let folder = &entry.summary.folder;
    let relative = file_edits::shown_path(folder, path);
    let relative = (!PathStyle::of(folder).is_absolute(&relative)).then_some(relative);
    Some(MenuTarget {
        agent,
        x,
        y,
        items: file_tree::menu_items(&row, relative.is_some()),
        row,
        relative,
    })
}

/// `name` cut before its last `TAIL_CHARS` characters, so that a narrow row hides its middle.
/// The cut moves left past a character that attaches to the one before it: "e" and its accent
/// stay in the same part.
fn split_tail(name: &str) -> (&str, &str) {
    let mut at = name
        .char_indices()
        .rev()
        .nth(TAIL_CHARS - 1)
        .map_or(0, |(at, _)| at);
    while let Some((head, tail)) = name.split_at_checked(at)
        && let (Some(before), Some(after)) = (head.chars().next_back(), tail.chars().next())
        && (attaches(after) || before == ZERO_WIDTH_JOINER)
    {
        at = at.saturating_sub(before.len_utf8());
    }
    name.split_at_checked(at).unwrap_or(("", name))
}

/// Whether `character` shows on the character before it rather than on its own: a combining
/// accent, a variation selector, a skin tone, or the joiner of an emoji sequence.
fn attaches(character: char) -> bool {
    matches!(
        character,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{1F3FB}'..='\u{1F3FF}'
            | ZERO_WIDTH_JOINER
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_name_keeps_its_end_apart() {
        assert_eq!(
            split_tail("settings-panel.component.ts"),
            ("settings-panel.", "component.ts")
        );
    }

    #[test]
    fn a_short_name_is_all_tail() {
        assert_eq!(split_tail("lib.rs"), ("", "lib.rs"));
        assert_eq!(split_tail("component.ts"), ("", "component.ts"));
    }

    #[test]
    fn the_cut_keeps_an_accent_on_its_letter() {
        assert_eq!(
            split_tail("abe\u{301}0123456789a"),
            ("ab", "e\u{301}0123456789a")
        );
    }

    #[test]
    fn the_cut_keeps_an_emoji_sequence_whole() {
        assert_eq!(
            split_tail("ab\u{1F469}\u{200D}\u{1F4BB}01234567890"),
            ("ab", "\u{1F469}\u{200D}\u{1F4BB}01234567890")
        );
    }

    #[test]
    fn the_cut_falls_between_characters() {
        assert_eq!(
            split_tail("été/ünïcödé_fïlé.rs"),
            ("été/ünï", "cödé_fïlé.rs")
        );
    }
}

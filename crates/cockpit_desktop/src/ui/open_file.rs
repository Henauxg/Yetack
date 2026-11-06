//! The "Open file" dialog (Ctrl+O): a path field over the entries of the folder it names, and, for
//! a bare name, the project's files that hold it.

use std::path::Path;
use std::time::Duration;

use cockpit_protocol::{DirEntry, EntryKind};
use dioxus::prelude::*;

use crate::bridge::{self, OPEN_FILE_FIELD_ID};
use crate::connection::Cockpit;
use crate::format;
use crate::state::actions::Direction;
use crate::state::open_file::{self, Row, Take};
use crate::state::{Loadable, OpenFileView};
use crate::ui::file_view::{ShortPath, short_path};
use crate::ui::icons::{Icon, IconKind};

/// The row of the folder above, and its key.
pub const UP_ROW: &str = "..";

/// How long typing pauses before a bare name is searched for in the project.
const SEARCH_PAUSE: Duration = Duration::from_millis(300);

/// The widths of the rows that stand for entries on their way.
const LOADING_ROWS: [&str; 5] = ["38%", "24%", "52%", "30%", "44%"];

/// The widths of the rows that stand for the project's files on their way.
const SEARCHING_ROWS: [&str; 2] = ["58%", "46%"];

#[component]
pub fn OpenFileDialog(view: OpenFileView) -> Element {
    let cockpit: Cockpit = use_context();
    let (text, highlighted) = {
        let drafts = cockpit.drafts.read();
        (drafts.open_file.path.clone(), drafts.open_file.highlighted)
    };
    // Follows the keyboard alone: a render that lags behind typing would put an older text back.
    let seed = use_hook(|| text.clone());
    let agent_folder = cockpit
        .state
        .peek()
        .agents
        .get(&view.agent)
        .map(|entry| entry.summary.folder.clone())
        .unwrap_or_default();
    let rows = open_file::rows(&view, &text);
    let segment = open_file::split(&text).1.to_owned();
    let first_match = rows.iter().position(|row| matches!(row, Row::Match(_)));
    let entry_rows = first_match.unwrap_or(rows.len());
    let listed = view
        .listing
        .loaded()
        .map_or(0, |listing| listing.entries.len());
    rsx! {
        div {
            class: "dialog open-file",
            role: "dialog",
            aria_label: "Open file",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Open file" }
                span { class: "key-hint", "Ctrl O" }
                button {
                    class: "icon-button small",
                    aria_label: "Close",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "open-file-path",
                div { class: "folder-field",
                    Icon { kind: IconKind::File, size: 16 }
                    input {
                        id: OPEN_FILE_FIELD_ID,
                        class: "folder-input mono",
                        r#type: "text",
                        spellcheck: false,
                        aria_label: "Path of the file to open",
                        placeholder: "Type a path, or a file's name to find it in the project",
                        initial_value: "{seed}",
                        onmounted: move |_| bridge::focus_field_at_end(OPEN_FILE_FIELD_ID),
                        oninput: move |event: FormEvent| type_path(cockpit, event.value()),
                        onkeydown: move |event: KeyboardEvent| field_key(cockpit, &event),
                    }
                }
                button {
                    class: "button primary",
                    onclick: move |_| take(cockpit, None, Take::Open),
                    "OK"
                }
            }
            div { class: "dialog-list open-file-list scroll",
                for (index, row) in rows.iter().take(entry_rows).enumerate() {
                    {row_view(cockpit, &agent_folder, *row, index, highlighted == Some(index))}
                }
                match &view.listing {
                    Loadable::NotRequested | Loadable::Loading => rsx! {
                        for width in LOADING_ROWS {
                            div { class: "open-file-row placeholder", aria_hidden: true,
                                span { class: "skeleton", style: "width: 14px; height: 14px" }
                                span { class: "skeleton skeleton-line", style: "width: {width}" }
                            }
                        }
                    },
                    Loadable::Failed(error) => rsx! {
                        span { class: "dialog-empty", "{sentence(error)}" }
                    },
                    Loadable::Loaded(listing) => rsx! {
                        if entry_rows == 0 || (entry_rows == 1 && rows.first() == Some(&Row::Up)) {
                            span { class: "dialog-empty",
                                if segment.is_empty() {
                                    "This folder is empty."
                                } else {
                                    "No entry of this folder holds \"{segment}\"."
                                }
                            }
                        }
                        if listing.truncated {
                            span { class: "dialog-empty",
                                "The folder holds more entries than the {listed} listed. Type a file's full name and press Enter to open it."
                            }
                        }
                    },
                }
                if let Some((name, search)) = &view.search {
                    div { class: "open-file-section", "Matches in the project" }
                    match search {
                        Loadable::NotRequested | Loadable::Loading => rsx! {
                            for width in SEARCHING_ROWS {
                                div { class: "open-file-row placeholder", aria_hidden: true,
                                    span { class: "skeleton", style: "width: 14px; height: 14px" }
                                    span { class: "skeleton skeleton-line", style: "width: {width}" }
                                }
                            }
                        },
                        Loadable::Failed(error) => rsx! {
                            span { class: "dialog-empty", "{sentence(error)}" }
                        },
                        Loadable::Loaded(matches) => rsx! {
                            if matches.is_empty() {
                                span { class: "dialog-empty",
                                    "No file in the project has \"{name}\" in its name."
                                }
                            }
                        },
                    }
                    for (index, row) in rows.iter().enumerate().skip(entry_rows) {
                        {row_view(cockpit, &agent_folder, *row, index, highlighted == Some(index))}
                    }
                }
            }
            span { class: "dialog-foot",
                "Up and Down to move, Tab to complete, Enter to open, Backspace to go up, Esc to close"
            }
        }
    }
}

fn row_view(
    cockpit: Cockpit,
    agent_folder: &Path,
    row: Row<'_>,
    index: usize,
    highlighted: bool,
) -> Element {
    let class = if highlighted {
        "switcher-row open-file-row highlighted"
    } else {
        "switcher-row open-file-row"
    };
    let click = move |_| take(cockpit, Some(index), Take::Open);
    match row {
        Row::Up => rsx! {
            button { key: "{UP_ROW}", class, onclick: click,
                Icon { kind: IconKind::Folder, size: 14 }
                span { class: "open-file-name mono", "{UP_ROW}" }
            }
        },
        Row::Entry(entry) => entry_view(entry, class, click),
        Row::Match(path) => {
            let short = ShortPath::new(agent_folder, path);
            let full = path.display().to_string();
            rsx! {
                button { key: "{full}", class, title: "{full}", onclick: click,
                    Icon { kind: IconKind::File, size: 14 }
                    span { class: "open-file-name mono file-path", {short_path(&short)} }
                }
            }
        }
    }
}

pub fn entry_view(
    entry: &DirEntry,
    class: &'static str,
    click: impl FnMut(MouseEvent) + 'static,
) -> Element {
    let icon = match entry.kind {
        EntryKind::Folder => IconKind::Folder,
        EntryKind::File | EntryKind::Other => IconKind::File,
    };
    let size = entry.size.map(format::bytes).unwrap_or_default();
    let name = &entry.name;
    rsx! {
        button { key: "{name}", class, onclick: click,
            Icon { kind: icon, size: 14 }
            span { class: "open-file-name mono", "{name}" }
            if entry.symlink {
                span { class: "open-file-link", title: "A symbolic link", "\u{2192}" }
            }
            span { class: "spacer" }
            span { class: "open-file-size mono", "{size}" }
        }
    }
}

/// The daemon's message as a sentence.
pub fn sentence(message: &str) -> String {
    let mut characters = message.chars();
    let first: String = characters
        .next()
        .map(char::to_uppercase)
        .into_iter()
        .flatten()
        .collect();
    format!("{first}{}.", characters.as_str())
}

/// Keeps the typed text, lists the folder it names at once, and searches for a bare name once
/// typing pauses.
fn type_path(cockpit: Cockpit, text: String) {
    let typed =
        cockpit.with_drafts(|state, drafts| open_file::type_path(state, drafts, text.clone()));
    if typed.follow {
        cockpit.update_with_drafts(|state, drafts| open_file::follow(state, drafts, false));
    }
    if typed.search {
        spawn(async move {
            tokio::time::sleep(SEARCH_PAUSE).await;
            let still = cockpit.with_drafts(|state, drafts| {
                drafts.open_file.path == text && open_file::needs_search(state, drafts)
            });
            if still {
                cockpit.update_with_drafts(|state, drafts| open_file::follow(state, drafts, true));
            }
        });
    }
}

fn field_key(cockpit: Cockpit, event: &KeyboardEvent) {
    match event.key() {
        // The keys move the highlight, and the field's caret stays.
        Key::ArrowDown => {
            event.prevent_default();
            cockpit.with_drafts(|state, drafts| {
                open_file::move_highlight(state, drafts, Direction::Down)
            });
            bridge::scroll_to_highlighted();
        }
        Key::ArrowUp => {
            event.prevent_default();
            cockpit.with_drafts(|state, drafts| {
                open_file::move_highlight(state, drafts, Direction::Up)
            });
            bridge::scroll_to_highlighted();
        }
        Key::Tab => {
            event.prevent_default();
            take(cockpit, None, Take::Complete);
        }
        Key::Enter => {
            event.prevent_default();
            take(cockpit, None, Take::Open);
        }
        // The page's script passes Backspace on only with the caret at the end and nothing
        // selected.
        Key::Backspace => {
            if let Some(text) =
                cockpit.with_drafts(|state, drafts| open_file::text_above(state, drafts))
            {
                event.prevent_default();
                cockpit.update_with_drafts(|state, drafts| {
                    open_file::set_text(state, drafts, text.clone())
                });
                bridge::set_field_text(OPEN_FILE_FIELD_ID, text);
            }
        }
        _ => {}
    }
}

/// Takes a row, or the highlighted one, and puts the field's new text in it. A click took the
/// focus from the field: it goes back there.
fn take(cockpit: Cockpit, index: Option<usize>, how: Take) {
    let mut new_text = None;
    cockpit
        .update_with_drafts(|state, drafts| new_text = open_file::take(state, drafts, index, how));
    if let Some(text) = new_text {
        bridge::set_field_text(OPEN_FILE_FIELD_ID, text);
        bridge::focus_field_at_end(OPEN_FILE_FIELD_ID);
    }
}

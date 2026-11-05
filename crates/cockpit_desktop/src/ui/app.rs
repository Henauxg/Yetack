//! The window: sidebar, center pane, file view and dialogs.

use dioxus::prelude::*;

use crate::bridge::use_bridge;
use crate::connection::{Cockpit, Startup, use_clock, use_cockpit};
use crate::prefs::use_prefs_saver;
use crate::state::{ConnectionState, NoteKind};
use crate::ui::blocks::FloatingBlocks;
use crate::ui::composer::Composer;
use crate::ui::dialogs::Dialogs;
use crate::ui::file_view::FileViewPanel;
use crate::ui::header::Header;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::reorder::use_reorder;
use crate::ui::sidebar::Sidebar;
use crate::ui::splitter::{APP_ID, SHARE_PROPERTY, Splitter, use_splitter};
use crate::ui::transcript::TranscriptView;

#[component]
pub fn App() -> Element {
    let startup: Startup = use_hook(consume_context);
    let prefs_file = startup.prefs_file.clone();
    let cockpit = use_cockpit(startup);
    use_prefs_saver(cockpit, prefs_file);
    use_clock();
    use_bridge(cockpit);
    use_splitter(cockpit);
    use_reorder(cockpit);
    let state = cockpit.state.read();
    let file_view_open = state
        .ui
        .selected
        .is_some_and(|agent| state.ui.file_view(agent).is_some());
    let closes_on_click = state.ui.popover_closes_on_click();
    let disconnected = matches!(state.connection, ConnectionState::Disconnected { .. });
    let share = state.prefs.file_view.share();
    rsx! {
        div { id: APP_ID, class: "app", style: "{SHARE_PROPERTY}: {share}",
            Sidebar {}
            main { class: if file_view_open { "center narrow-chat" } else { "center" },
                Header {}
                if disconnected {
                    div { class: "connection-banner",
                        "Not connected to the daemon. The app retries on its own; what you see may be out of date."
                    }
                }
                div { class: "chat-area",
                    TranscriptView {}
                    FloatingBlocks {}
                    ErrorNotes {}
                }
                Composer {}
            }
            if file_view_open {
                Splitter { share }
                FileViewPanel {}
            }
            if closes_on_click {
                div {
                    class: "popover-backdrop",
                    onclick: move |_| cockpit.update(|state| state.ui.popover = None),
                }
            }
            Dialogs {}
        }
    }
}

#[component]
fn ErrorNotes() -> Element {
    let cockpit: Cockpit = use_context();
    let notes = cockpit.state.read().ui.errors.clone();
    rsx! {
        div { class: "error-notes", aria_live: "polite",
            for note in notes {
                div { key: "{note.id}", class: if note.kind == NoteKind::Info { "error-note info" } else { "error-note" },
                    span { "{note.text}" }
                    button {
                        class: "icon-button tiny",
                        aria_label: "Dismiss",
                        onclick: move |_| cockpit.update(|state| state.ui.dismiss(note.id)),
                        Icon { kind: IconKind::Close, size: 12 }
                    }
                }
            }
        }
    }
}

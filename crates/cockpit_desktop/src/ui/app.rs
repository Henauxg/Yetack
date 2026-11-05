//! The window: sidebar, center pane, file view and dialogs.

use dioxus::prelude::*;

use crate::bridge::use_bridge;
use crate::connection::{Cockpit, Startup, use_clock, use_cockpit};
use crate::state::{ConnectionState, Popover};
use crate::ui::blocks::FloatingBlocks;
use crate::ui::composer::Composer;
use crate::ui::dialogs::Dialogs;
use crate::ui::file_view::FileViewPanel;
use crate::ui::header::Header;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::sidebar::Sidebar;
use crate::ui::transcript::TranscriptView;

#[component]
pub fn App() -> Element {
    let startup = use_hook(consume_context::<Startup>);
    let cockpit = use_cockpit(startup);
    use_clock();
    use_bridge(cockpit);
    let state = cockpit.state.read();
    let file_view_open = state
        .ui
        .selected
        .is_some_and(|agent| state.ui.file_view(agent).is_some());
    // The palette stays open while you type in the composer; clicking elsewhere closes the
    // other popovers.
    let closes_on_click = state
        .ui
        .popover
        .is_some_and(|popover| popover != Popover::Palette);
    let disconnected = matches!(state.connection, ConnectionState::Disconnected { .. });
    rsx! {
        div { class: "app",
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
    let cockpit = use_context::<Cockpit>();
    let notes = cockpit.state.read().ui.errors.clone();
    rsx! {
        div { class: "error-notes", "aria-live": "polite",
            for note in notes {
                div { key: "{note.id}", class: "error-note",
                    span { "{note.text}" }
                    button {
                        class: "icon-button tiny",
                        "aria-label": "Dismiss",
                        onclick: move |_| cockpit.update(|state| state.ui.dismiss(note.id)),
                        Icon { kind: IconKind::Close, size: 12 }
                    }
                }
            }
        }
    }
}

//! The window: sidebar, center pane, file view and dialogs.

use std::time::Duration;

use dioxus::prelude::*;

use crate::bridge::use_bridge;
use crate::connection::{Cockpit, Startup, use_clock, use_cockpit};
use crate::prefs::use_prefs_saver;
use crate::state::{ConnectionState, NoteKind, actions};
use crate::ui::blocks::FloatingBlocks;
use crate::ui::composer::Composer;
use crate::ui::dialogs::Dialogs;
use crate::ui::file_view::FileViewPanel;
use crate::ui::header::Header;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::no_daemon::NoDaemonScreen;
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
    // With servers, the question shows from this computer's panel, and the servers stay in use.
    let no_daemon = matches!(state.machines.this.connection, ConnectionState::NoDaemon(_));
    if no_daemon && state.machines.others.is_empty() {
        return rsx! {
            NoDaemonScreen {}
        };
    }
    let file_view_open = state
        .ui
        .selected
        .is_some_and(|agent| state.ui.file_view(agent).is_some());
    let closes_on_click = state.ui.popover_closes_on_click();
    let disconnected = matches!(
        state.selected_machine_state().connection,
        ConnectionState::Disconnected { .. }
    );
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

/// How long the note of a closed agent offers it back.
const UNDO_CLOSE: Duration = Duration::from_secs(8);

#[component]
fn ErrorNotes() -> Element {
    let cockpit: Cockpit = use_context();
    let notes = cockpit.state.read().ui.errors.clone();
    rsx! {
        div { class: "error-notes", aria_live: "polite",
            ClosedNote {}
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

/// "Closed <title>", with Undo while `UNDO_CLOSE` runs. A later close replaces it.
#[component]
fn ClosedNote() -> Element {
    let cockpit: Cockpit = use_context();
    let closed = cockpit.state.read().ui.closed.clone();
    let mut timed = use_hook(|| CopyValue::new(None::<u32>));
    let Some(closed) = closed else {
        return rsx! {};
    };
    let serial = closed.serial;
    if timed() != Some(serial) {
        timed.set(Some(serial));
        spawn(async move {
            tokio::time::sleep(UNDO_CLOSE).await;
            cockpit.update(|state| state.ui.expire_closed(serial));
        });
    }
    rsx! {
        div { key: "closed-{serial}", class: "error-note info closed-note",
            span { "Closed {closed.title}." }
            if closed.session.is_some() {
                button {
                    class: "link-button undo-button",
                    onclick: move |_| cockpit.change(actions::undo_close),
                    "Undo"
                }
            }
            button {
                class: "icon-button tiny",
                aria_label: "Dismiss",
                onclick: move |_| cockpit.update(|state| state.ui.expire_closed(serial)),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

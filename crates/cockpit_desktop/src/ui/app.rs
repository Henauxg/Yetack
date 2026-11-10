//! The window: sidebar, center pane, file view and dialogs.

use std::time::Duration;

use cockpit_protocol::AgentId;
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
use crate::ui::presence::use_presence;
use crate::ui::reorder::use_reorder;
use crate::ui::sidebar::{RowMenu, Sidebar};
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
    let selected = state.ui.selected;
    let switching = use_switching(cockpit, selected);
    let file_view_agent = selected.filter(|agent| state.ui.file_view(*agent).is_some());
    let file_view_open = file_view_agent.is_some();
    let file_view = use_presence(file_view_agent);
    // A switch shows the next agent's layout at once: only the agent that closed its file view
    // sees it leave, and a file view that opens with its agent does not slide in.
    let file_view_leaving = file_view
        .as_ref()
        .is_some_and(|shown| shown.leaving && Some(shown.value) == selected);
    let mut last_opening = use_hook(|| CopyValue::new(FileViewOpening::default()));
    if let Some(shown) = &file_view
        && !shown.leaving
        && last_opening().opening != shown.opening
    {
        last_opening.set(FileViewOpening {
            opening: shown.opening,
            by_switch: switching,
        });
    }
    let mut app_classes = vec!["app"];
    if switching {
        app_classes.push("switching");
    }
    if file_view_open && last_opening().by_switch {
        app_classes.push("file-view-instant");
    }
    let app_class = app_classes.join(" ");
    // With servers, the question shows from this computer's panel, and the servers stay in use.
    // An update of this computer's daemon keeps the screen until the new daemon answers.
    let no_daemon = matches!(state.machines.this.connection, ConnectionState::NoDaemon(_))
        || state.machines.this.update.is_some();
    if no_daemon && state.machines.others.is_empty() {
        return rsx! {
            NoDaemonScreen {}
        };
    }
    let closes_on_click = state.ui.popover_closes_on_click();
    let disconnected = matches!(
        state.selected_machine_state().connection,
        ConnectionState::Disconnected { .. }
    );
    let share = state.prefs.file_view.share();
    rsx! {
        div { id: APP_ID, class: app_class, style: "{SHARE_PROPERTY}: {share}",
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
            } else if file_view_leaving {
                // Its content went with its state: the empty panel closes in its place.
                div { class: "file-view leaving", aria_hidden: true }
            }
            if closes_on_click {
                div {
                    class: "popover-backdrop",
                    onclick: move |_| cockpit.update(|state| state.ui.popover = None),
                }
            }
            RowMenu {}
            Dialogs {}
        }
    }
}

/// The file view's last opening, and whether an agent switch opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FileViewOpening {
    opening: u32,
    by_switch: bool,
}

/// True in the frame that draws another agent than the frame before. Effects run once the
/// frame's changes are on the page, so the next frame clears it.
fn use_switching(cockpit: Cockpit, selected: Option<AgentId>) -> bool {
    let shown = use_memo(move || cockpit.state.read().ui.selected);
    let mut drawn = use_signal(|| selected);
    use_effect(move || drawn.set(shown()));
    drawn() != selected
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

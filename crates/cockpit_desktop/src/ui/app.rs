//! The window: sidebar, center pane, file view, files pane and dialogs.

use std::time::{Duration, Instant};

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::bridge::use_bridge;
use crate::connection::{Cockpit, Startup, use_clock, use_cockpit};
use crate::prefs::{TerminalPlace, use_prefs_saver};
use crate::state::{
    ConnectionState, NoteKind, actions, daemon_update, file_tree, sending, terminals,
};
use crate::ui::app_update::use_release_checks;
use crate::ui::blocks::FloatingBlocks;
use crate::ui::chat_scroll::InView;
use crate::ui::composer::Composer;
use crate::ui::dialogs::Dialogs;
use crate::ui::file_view::{FileViewPanel, use_page_copies};
use crate::ui::files_pane::{FilesPane, TreeMenu};
use crate::ui::find::{ChatFindBar, use_find};
use crate::ui::header::Header;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::no_daemon::NoDaemonScreen;
use crate::ui::presence::use_presence;
use crate::ui::project_menu::ProjectMenu;
use crate::ui::quotes::{QuoteLayer, use_quotes};
use crate::ui::reorder::use_reorder;
use crate::ui::row_menu::RowMenu;
use crate::ui::sidebar::Sidebar;
use crate::ui::splitter::{
    APP_ID, FILES_WIDTH_PROPERTY, FilesSplitter, SHARE_PROPERTY, SIDEBAR_WIDTH_PROPERTY,
    SidebarSplitter, Splitter, use_splitter,
};
use crate::ui::tab_menu::TabMenu;
use crate::ui::terminal_pane::{TerminalPane, use_terminal_script};
use crate::ui::transcript::{TranscriptView, use_call_detail};

#[component]
pub fn App() -> Element {
    let startup: Startup = use_hook(consume_context);
    #[cfg(target_os = "linux")]
    use_hook(crate::webview_settings::turn_off_smooth_scrolling);
    let prefs_file = startup.prefs_file.clone();
    let cockpit = use_cockpit(startup);
    use_prefs_saver(cockpit, prefs_file);
    use_release_checks(cockpit);
    use_clock();
    use_bridge(cockpit);
    use_terminal_script(cockpit);
    use_quotes(cockpit);
    use_find(cockpit);
    use_call_detail(cockpit);
    use_splitter(cockpit);
    use_reorder(cockpit);
    use_held_release(cockpit);
    use_page_copies(cockpit);
    // The entries in the chat's view: the bubble and the Messages block follow them.
    let in_view = use_signal(|| None::<InView>);
    let state = cockpit.state.read();
    let selected = state.ui.selected;
    let switching = use_switching(cockpit, selected);
    let file_view_agent = selected.filter(|agent| {
        state.ui.file_view(*agent).is_some() || terminals::in_file_view(&state, *agent)
    });
    let file_view_open = file_view_agent.is_some();
    let file_view = use_presence(file_view_agent);
    // Each agent keeps its pane open or closed. As the file view, the pane leaves with a slide
    // only for the agent that closed it, and opens at once when a switch shows it.
    let files_agent = selected.filter(|agent| file_tree::pane_open(&state, *agent));
    let files_open = files_agent.is_some();
    let files_pane = use_presence(files_agent);
    // The terminal pane folds away as it closes, wherever it was. As the files pane, it leaves
    // with a fold only for the agent that closed it.
    let terminal_agent = selected.filter(|_| terminals::pane_open(&state));
    let terminal_pane =
        use_presence(terminal_agent.map(|agent| (agent, state.prefs.terminal.place)));
    let files_leaving = files_pane
        .as_ref()
        .is_some_and(|shown| shown.leaving && Some(shown.value) == selected);
    let mut last_files_opening = use_hook(|| CopyValue::new(PaneOpening::default()));
    if let Some(shown) = &files_pane
        && !shown.leaving
        && last_files_opening().opening != shown.opening
    {
        last_files_opening.set(PaneOpening {
            opening: shown.opening,
            by_switch: switching,
        });
    }
    // A switch shows the next agent's layout at once: only the agent that closed its file view
    // sees it leave, and a file view that opens with its agent does not slide in.
    let file_view_leaving = file_view
        .as_ref()
        .is_some_and(|shown| shown.leaving && Some(shown.value) == selected);
    let mut last_opening = use_hook(|| CopyValue::new(PaneOpening::default()));
    if let Some(shown) = &file_view
        && !shown.leaving
        && last_opening().opening != shown.opening
    {
        last_opening.set(PaneOpening {
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
    if files_open && last_files_opening().by_switch {
        app_classes.push("files-pane-instant");
    }
    let app_class = app_classes.join(" ");
    // With servers, the question shows from this computer's panel, and the servers stay in use.
    let no_daemon = matches!(state.machines.this.connection, ConnectionState::NoDaemon(_))
        || daemon_update::takes_the_window(&state.machines.this);
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
    let sidebar_width = state.prefs.sidebar.width();
    let files_width = state.prefs.files_pane.width();
    let terminal_at = |place| {
        terminal_pane
            .as_ref()
            .filter(|shown| {
                let (agent, at) = shown.value;
                at == place && (!shown.leaving || Some(agent) == selected)
            })
            .map(|shown| shown.leaving)
    };
    let (terminal_bottom, terminal_right) = (
        terminal_at(TerminalPlace::Bottom),
        terminal_at(TerminalPlace::Right),
    );
    rsx! {
        div { id: APP_ID, class: app_class, style: "{SHARE_PROPERTY}: {share}; {SIDEBAR_WIDTH_PROPERTY}: {sidebar_width}px; {FILES_WIDTH_PROPERTY}: {files_width}px",
            Sidebar {}
            SidebarSplitter { width: sidebar_width }
            main { class: if file_view_open { "center narrow-chat" } else { "center" },
                Header {}
                if disconnected {
                    div { class: "connection-banner",
                        "Not connected to the daemon. The app retries on its own; what you see may be out of date."
                    }
                }
                ChatFindBar {}
                div { class: "chat-area",
                    TranscriptView { in_view }
                    QuoteLayer {}
                    FloatingBlocks { in_view }
                    ErrorNotes {}
                }
                Composer {}
                if let Some(leaving) = terminal_bottom {
                    TerminalPane { place: TerminalPlace::Bottom, leaving }
                }
            }
            if file_view_open {
                Splitter { share }
                FileViewPanel {}
            } else if file_view_leaving {
                // Its content went with its state: the empty panel closes in its place.
                div { class: "file-view leaving", aria_hidden: true }
            }
            if files_open {
                FilesSplitter { width: files_width }
                FilesPane {}
            } else if files_leaving {
                div { class: "files-pane leaving", aria_hidden: true }
            }
            if let Some(leaving) = terminal_right {
                TerminalPane { place: TerminalPlace::Right, leaving }
            }
            if closes_on_click {
                div {
                    class: "popover-backdrop",
                    onclick: move |_| cockpit.update(|state| state.ui.close_popover()),
                    // A right-click on another row closes the open row menu, so the next one
                    // opens that row's.
                    oncontextmenu: move |event: MouseEvent| {
                        event.prevent_default();
                        cockpit.update(|state| state.ui.close_popover());
                    },
                }
            }
            RowMenu {}
            ProjectMenu {}
            TreeMenu {}
            TabMenu {}
            Dialogs {}
        }
    }
}

/// A side panel's last opening, and whether an agent switch opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PaneOpening {
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

/// How often the app looks for a held message that is due.
const HELD_CHECK: Duration = Duration::from_millis(200);

/// Sends each held message once its hold ends, for the window's life: a message leaves whichever
/// agent shows. A look costs a read of the state, and no render.
fn use_held_release(cockpit: Cockpit) {
    use_future(move || async move {
        loop {
            tokio::time::sleep(HELD_CHECK).await;
            let due = sending::next_release(&cockpit.state.peek())
                .is_some_and(|due| due <= Instant::now());
            if due {
                cockpit.change(|state| sending::release_due(state, Instant::now()));
            }
        }
    });
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

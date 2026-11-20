//! The menu a right-click in a terminal opens at the pointer: open or copy the link under the
//! pointer, copy, paste, select the screen, clear it, open another terminal, move the terminal,
//! or close it.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, TerminalId};
use dioxus::prelude::*;

use crate::bridge;
use crate::connection::Cockpit;
use crate::state::terminal_menu::{self, MenuSpot, TerminalItem, TerminalLink};
use crate::state::{actions, terminals};
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;
use crate::ui::terminal_pane::{self, PageAction, TerminalScript};

/// What a shell reads as "clear the screen": Ctrl+L.
const CLEAR_SCREEN: &str = "\x0c";

#[component]
pub fn TerminalMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let script: TerminalScript = use_context();
    let open = terminal_menu::target(&cockpit.state.read());
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let (spot, rows) = shown.value.clone();
    // A menu opened at another place starts with nothing highlighted.
    let key = format!("{}-{}-{}", shown.opening, spot.x, spot.y);
    let link = spot.link.as_ref();
    let flat: Vec<FlatRow> = rows
        .iter()
        .map(|row| FlatRow {
            label: row.item.label(link),
            key: row.item.key(),
            enabled: row.enabled,
            title: title(row.item, row.enabled, link),
            divided: row.divided,
            danger: row.item == TerminalItem::Close,
        })
        .collect();
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                FlatMenu {
                    key: "{key}",
                    x: spot.x,
                    y: spot.y,
                    rows: flat.clone(),
                    focus_back: None,
                    class: Some("terminal-menu"),
                    on_pick: {
                        let spot = spot.clone();
                        let rows = rows.clone();
                        move |index: usize| {
                            if let Some(row) = rows.get(index) {
                                run(cockpit, script, &spot, row.item);
                            }
                        }
                    },
                }
            }
        }
    }
}

/// What a row does, where its label does not say it. The link's rows name the link.
fn title(item: TerminalItem, enabled: bool, link: Option<&TerminalLink>) -> String {
    match item {
        TerminalItem::OpenLink | TerminalItem::CopyLink => {
            link.map(TerminalLink::text).unwrap_or_default()
        }
        TerminalItem::Copy if !enabled => "Select text first".to_owned(),
        TerminalItem::SelectAll => {
            "Selects the rows on screen, not the ones scrolled away".to_owned()
        }
        TerminalItem::Clear => {
            "Sends Ctrl+L, which a shell answers by clearing the screen".to_owned()
        }
        TerminalItem::Close => "Ends this terminal's shell".to_owned(),
        _ => String::new(),
    }
}

fn run(cockpit: Cockpit, script: TerminalScript, spot: &MenuSpot, item: TerminalItem) {
    let MenuSpot {
        agent, terminal, ..
    } = *spot;
    cockpit.update(terminal_menu::close_menu);
    match item {
        TerminalItem::OpenLink => match &spot.link {
            Some(TerminalLink::Web(url)) => terminal_pane::open_web_link(cockpit, url.clone()),
            Some(TerminalLink::File { path, line }) => {
                let (path, line) = (PathBuf::from(path), *line);
                cockpit.update(|state| actions::open_file(state, agent, path, Some(line)));
            }
            None => {}
        },
        TerminalItem::CopyLink => {
            if let Some(link) = &spot.link {
                bridge::copy_to_clipboard(link.text());
            }
        }
        TerminalItem::Copy => script.act(agent, terminal, PageAction::Copy),
        TerminalItem::Paste => paste(cockpit, script, agent, terminal),
        TerminalItem::SelectAll => script.act(agent, terminal, PageAction::SelectAll),
        TerminalItem::Clear => {
            cockpit.send_terminal_keys(terminals::input(agent, terminal, CLEAR_SCREEN.to_owned()))
        }
        TerminalItem::NewTerminal => cockpit.change(|state| terminals::open(state, agent)),
        TerminalItem::MoveTo(place) => cockpit.update(|state| terminals::move_to(state, place)),
        TerminalItem::Close => cockpit.change(|state| terminals::close(state, agent, terminal)),
    }
}

/// The menu's Paste. WebKitGTK refuses the page's own clipboard read, so on Linux the app reads
/// the clipboard through GTK, on the window's thread, and hands the text to the page. A clipboard
/// owner may answer late: the text goes only to a terminal that still shows.
#[cfg(target_os = "linux")]
fn paste(cockpit: Cockpit, script: TerminalScript, agent: AgentId, terminal: TerminalId) {
    let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
    clipboard.request_text(move |_, text| {
        let Some(text) = text.filter(|text| !text.is_empty()) else {
            cockpit.update(|state| state.ui.report("The clipboard holds no text.".to_owned()));
            return;
        };
        if terminals::shows(&cockpit.state.read(), agent, terminal) {
            script.paste(agent, terminal, text.to_owned());
        }
    });
}

#[cfg(not(target_os = "linux"))]
fn paste(_: Cockpit, script: TerminalScript, agent: AgentId, terminal: TerminalId) {
    script.act(agent, terminal, PageAction::Paste);
}

//! The menu a right-click on a terminal's tab opens at the pointer, under the terminal's id: let
//! the agent read the terminal or stop it, rename the tab, copy the id, pin or unpin the tab, move
//! the terminal to the other dock, or close it.

use dioxus::prelude::*;

use crate::bridge;
use crate::connection::Cockpit;
use crate::state::terminal_tab_menu::{self, TerminalTabItem, TerminalTabTarget};
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;

#[component]
pub fn TerminalTabMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = terminal_tab_menu::target(&cockpit.state.read());
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let menu = shown.value.clone();
    // A menu opened on another tab starts with nothing highlighted.
    let key = format!(
        "{}-{}-{}-{}",
        shown.opening,
        menu.terminal.get(),
        menu.x,
        menu.y
    );
    let rows: Vec<FlatRow> = menu
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| FlatRow {
            label: item.label(),
            key: item.key(),
            enabled: true,
            title: item.title().to_owned(),
            // The sharing row stands apart from the tab's own rows, and Close from them all.
            divided: index > 0 && matches!(item, TerminalTabItem::Rename | TerminalTabItem::Close),
            danger: *item == TerminalTabItem::Close,
        })
        .collect();
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                FlatMenu {
                    key: "{key}",
                    x: menu.x,
                    y: menu.y,
                    rows: rows.clone(),
                    focus_back: None,
                    heading: terminal_tab_menu::heading(menu.terminal),
                    on_pick: {
                        let menu = menu.clone();
                        move |index: usize| {
                            if let Some(item) = menu.items.get(index) {
                                run(cockpit, &menu, *item);
                            }
                        }
                    },
                }
            }
        }
    }
}

fn run(cockpit: Cockpit, menu: &TerminalTabTarget, item: TerminalTabItem) {
    let (agent, terminal) = (menu.agent, menu.terminal);
    if item == TerminalTabItem::CopyId {
        bridge::copy_to_clipboard(terminal_tab_menu::id_text(terminal));
    }
    cockpit.change(|state| terminal_tab_menu::run(state, agent, terminal, item));
}

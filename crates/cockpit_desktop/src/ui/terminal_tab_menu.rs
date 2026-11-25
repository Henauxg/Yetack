//! The menu a right-click on a terminal's tab opens at the pointer: pin or unpin the tab, move the
//! terminal to the other dock, or close it.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::docks::{self, TabKey};
use crate::state::terminal_tab_menu::{self, TerminalTabItem, TerminalTabTarget};
use crate::state::terminals;
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
        .map(|item| FlatRow {
            label: item.label(),
            key: item.key(),
            enabled: true,
            title: String::new(),
            divided: *item == TerminalTabItem::Close,
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
    match item {
        TerminalTabItem::Pin | TerminalTabItem::Unpin => cockpit.update(|state| {
            state.ui.popover = None;
            docks::toggle_pin(state, agent, &TabKey::Terminal(terminal));
        }),
        TerminalTabItem::MoveTo(dock) => cockpit.update(|state| {
            state.ui.popover = None;
            terminals::move_terminal(state, agent, terminal, dock, None);
        }),
        TerminalTabItem::Close => cockpit.change(|state| {
            state.ui.popover = None;
            terminals::close(state, agent, terminal)
        }),
    }
}

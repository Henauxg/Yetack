//! The right-click menu of a subagent, at the pointer: on its row in the agent map's outline, or
//! in the chat's Running block.

use dioxus::prelude::*;

use crate::bridge;
use crate::connection::Cockpit;
use crate::state::agent_map;
use crate::state::subagent_menu::{self, MenuOrigin, SubagentItem, SubagentTarget};
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;

/// The menu opened from the agent map, which the map draws.
#[component]
pub fn MapSubagentMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = subagent_menu::map_target(&cockpit.state.read());
    rsx! {
        SubagentMenu { open }
    }
}

/// The menu opened from the chat's Running block, over the window.
#[component]
pub fn BlockSubagentMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = subagent_menu::block_target(&cockpit.state.read());
    rsx! {
        SubagentMenu { open }
    }
}

#[component]
fn SubagentMenu(open: Option<SubagentTarget>) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let target = shown.value.clone();
    // A menu opened on another subagent starts with nothing highlighted.
    let key = format!(
        "{}-{}-{}-{}",
        shown.opening,
        target.item.get(),
        target.x,
        target.y
    );
    let rows: Vec<FlatRow> = target
        .rows
        .iter()
        .map(|row| FlatRow {
            label: row.item.label(),
            key: row.item.key(),
            enabled: row.enabled,
            title: row.item.title(row.enabled).to_owned(),
            divided: row.item.divided(),
            danger: matches!(row.item, SubagentItem::Stop(_)),
        })
        .collect();
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            // A click in the menu stays in it: the map closes the menu on the clicks it gets.
            onclick: move |event| event.stop_propagation(),
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
                            if let Some(row) = target.rows.get(index) {
                                run(cockpit, &target, row.item);
                            }
                        }
                    },
                }
            }
        }
    }
}

fn run(cockpit: Cockpit, target: &SubagentTarget, item: SubagentItem) {
    let SubagentTarget {
        agent, item: call, ..
    } = *target;
    match item {
        SubagentItem::ShowInChat => cockpit.update(|state| {
            subagent_menu::close(state);
            match target.origin {
                MenuOrigin::Map => agent_map::show_call(state, call),
                MenuOrigin::Block => agent_map::show_call_in_chat(state, agent, call),
            }
        }),
        SubagentItem::OpenInMap => cockpit.update(|state| {
            subagent_menu::close(state);
            agent_map::open_on(state, agent, call);
        }),
        SubagentItem::CopyPrompt => copy(cockpit, target.prompt.clone()),
        SubagentItem::CopyResult => copy(cockpit, target.result.clone()),
        SubagentItem::Stop(_) => cockpit.change(|state| {
            subagent_menu::close(state);
            agent_map::stop(state, agent, call)
        }),
    }
}

fn copy(cockpit: Cockpit, text: Option<String>) {
    if let Some(text) = text {
        bridge::copy_to_clipboard(text);
    }
    cockpit.update(subagent_menu::close);
}

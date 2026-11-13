//! The menu a right-click on a file view tab, or on its row in the list of open files, opens at
//! the pointer: close the tab or a group of tabs around it, copy its path, put it in the message,
//! or show it in the files pane.

use dioxus::prelude::*;

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::Cockpit;
use crate::state::tab_menu::{self, TabItem, TabTarget};
use crate::state::{TabMenuOrigin, file_tree};
use crate::ui::file_list::FILTER_ID;
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;

#[component]
pub fn TabMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = tab_menu::target(&cockpit.state.read(), &cockpit.drafts.read());
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let target = shown.value.clone();
    // A menu opened on another tab starts with nothing highlighted.
    let key = format!(
        "{}-{}-{}-{}",
        shown.opening,
        target.agent.get(),
        target.x,
        target.y
    );
    let rows: Vec<FlatRow> = target
        .rows
        .iter()
        .map(|row| FlatRow {
            label: row.item.label(target.origin),
            key: row.item.key(target.origin),
            enabled: row.enabled,
            title: title(row.item, row.enabled),
            divided: row.item == TabItem::CopyPath,
            danger: false,
        })
        .collect();
    let focus_back = (target.origin == TabMenuOrigin::List).then_some(FILTER_ID);
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                FlatMenu {
                    key: "{key}",
                    x: target.x,
                    y: target.y,
                    rows: rows.clone(),
                    focus_back,
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

/// Why a row cannot run, where its label does not say it.
fn title(item: TabItem, enabled: bool) -> String {
    match item {
        TabItem::ShowInPane if !enabled => {
            "The files pane shows the agent's folder and the files the agent read or changed"
                .to_owned()
        }
        _ => String::new(),
    }
}

fn run(cockpit: Cockpit, target: &TabTarget, item: TabItem) {
    let agent = target.agent;
    let path = &target.path;
    match item {
        TabItem::Close(group) => cockpit.update_with_drafts(|state, drafts| {
            tab_menu::close(state, drafts, agent, path, group, target.origin);
        }),
        TabItem::CopyPath => {
            bridge::copy_to_clipboard(target.full.clone());
            cockpit.update(tab_menu::close_menu);
        }
        TabItem::CopyRelativePath => {
            if let Some(relative) = &target.relative {
                bridge::copy_to_clipboard(relative.clone());
            }
            cockpit.update(tab_menu::close_menu);
        }
        TabItem::Mention => {
            cockpit.update_with_drafts(|state, drafts| {
                tab_menu::close_menu(state);
                file_tree::mention(state, drafts, agent, &target.absolute);
            });
            bridge::focus_field_at_end(COMPOSER_ID);
        }
        TabItem::ShowInPane => cockpit.update(|state| {
            tab_menu::close_menu(state);
            state.ui.popover = None;
            file_tree::show_in_pane(state, agent, path);
        }),
    }
}

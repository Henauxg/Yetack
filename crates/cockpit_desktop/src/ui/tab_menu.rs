//! The menu a right-click on a file view tab opens, at the pointer: close the tab or a group of
//! tabs around it, copy its path, put it in the message, or show it in the files pane.

use std::path::PathBuf;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::Cockpit;
use crate::path_style::{self, PathStyle};
use crate::state::tab_menu::{self, TabGroup};
use crate::state::{AppState, Popover, file_edits, file_tree};
use crate::ui::presence::use_presence;

/// What the open menu is about.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    agent: AgentId,
    x: i32,
    y: i32,
    /// The tab, as the file view keeps it: relative to the agent's folder when it is inside it.
    path: PathBuf,
    absolute: PathBuf,
    /// The path as paths show.
    full: String,
    /// The path from the agent's folder, for a file inside it.
    relative: Option<String>,
    left: bool,
    right: bool,
}

fn target(state: &AppState, agent: AgentId, x: i32, y: i32) -> Option<Target> {
    let entry = state.agents.get(&agent)?;
    let path = state.ui.file_view(agent)?.menu.clone()?;
    let tabs = tab_menu::tabs(state, agent);
    let at = tabs.iter().position(|tab| *tab == path)?;
    let folder = &entry.summary.folder;
    let style = PathStyle::of(folder);
    let absolute = PathBuf::from(style.resolve(&folder.to_string_lossy(), &path.to_string_lossy()));
    let relative = file_edits::shown_path(folder, &absolute);
    let inside = !style.is_absolute(&relative);
    Some(Target {
        agent,
        x,
        y,
        full: path_style::shown(&absolute),
        relative: inside.then_some(relative),
        left: at > 0,
        right: at + 1 < tabs.len(),
        path,
        absolute,
    })
}

#[component]
pub fn TabMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = {
        let state = cockpit.state.read();
        match state.ui.popover {
            Some(Popover::TabMenu { agent, x, y }) => target(&state, agent, x, y),
            _ => None,
        }
    };
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let Target {
        agent,
        x,
        y,
        path,
        absolute,
        full,
        relative,
        left,
        right,
    } = shown.value.clone();
    let close_row = move |group: TabGroup, label: &'static str, can_run: bool| {
        let path = path.clone();
        rsx! {
            button {
                class: "menu-item",
                role: "menuitem",
                disabled: !can_run,
                onclick: move |_| cockpit.update(|state| tab_menu::close(state, agent, &path, group)),
                span { class: "menu-label", "{label}" }
            }
        }
    };
    let copy_row = move |text: String, label: &'static str| {
        rsx! {
            button {
                class: "menu-item",
                role: "menuitem",
                onclick: move |_| {
                    bridge::copy_to_clipboard(text.clone());
                    cockpit.update(tab_menu::close_menu);
                },
                span { class: "menu-label", "{label}" }
            }
        }
    };
    let mention_path = absolute.clone();
    let pane_path = shown.value.path.clone();
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            div {
                class: "popover menu row-menu",
                role: "menu",
                style: "--menu-x: {x}px; --menu-y: {y}px",
                {close_row(TabGroup::This, "Close", true)}
                {close_row(TabGroup::Others, "Close the others", left || right)}
                {close_row(TabGroup::Left, "Close the tabs on the left", left)}
                {close_row(TabGroup::Right, "Close the tabs on the right", right)}
                {close_row(TabGroup::All, "Close all", true)}
                div { class: "menu-divider" }
                {copy_row(full, "Copy the path")}
                if let Some(relative) = relative {
                    {copy_row(relative, "Copy the relative path")}
                }
                button {
                    class: "menu-item",
                    role: "menuitem",
                    onclick: move |_| {
                        let path = mention_path.clone();
                        cockpit.update_with_drafts(|state, drafts| {
                            tab_menu::close_menu(state);
                            file_tree::mention(state, drafts, agent, &path);
                        });
                        bridge::focus_field_at_end(COMPOSER_ID);
                    },
                    span { class: "menu-label", "Mention in the message" }
                }
                button {
                    class: "menu-item",
                    role: "menuitem",
                    onclick: move |_| {
                        let path = pane_path.clone();
                        cockpit.update(|state| {
                            tab_menu::close_menu(state);
                            file_tree::show_in_pane(state, agent, &path);
                        });
                    },
                    span { class: "menu-label", "Show in the files pane" }
                }
            }
        }
    }
}

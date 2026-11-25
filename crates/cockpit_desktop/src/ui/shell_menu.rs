//! The shells a new terminal can run: the button beside the tab bar's "+", and the menu it opens
//! under or above itself, which lists the shells of the agent's machine.

use std::rc::Rc;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::bridge::{self, HIGHLIGHTED_CLASS};
use crate::connection::Cockpit;
use crate::state::docks::Dock;
use crate::state::menu_keys::MenuKey;
use crate::state::{MenuSide, terminals};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::menu::KeyedMenu;
use crate::ui::presence::{use_agent_presence, use_presence};

const WINDOW_HEIGHT_SCRIPT: &str = "return window.innerHeight;";

/// The button beside the "+", which opens the menu under or above itself, while the agent's daemon
/// `offers` a choice of shell. A switch shows the other agent's at once, and only the agent whose
/// shells go sees it leave.
#[component]
pub fn ShellButton(
    agent: AgentId,
    dock: Dock,
    offers: bool,
    open: bool,
    disabled: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    let Some(shown) = use_agent_presence(Some(agent), offers.then_some(agent)) else {
        return rsx! {};
    };
    let base = if open {
        "terminal-shells open"
    } else {
        "terminal-shells"
    };
    rsx! {
        button {
            key: "{shown.opening}",
            class: shown.class(base),
            inert: shown.leaving.then_some(true),
            aria_label: "New terminal with another shell",
            aria_haspopup: "menu",
            aria_expanded: open,
            title: "New terminal with another shell",
            disabled,
            onmounted: move |event| mounted.set(Some(event.data())),
            // A click from the keyboard has no pointer position: the menu goes by the button.
            onclick: move |event: MouseEvent| async move {
                let pointer = event.client_coordinates();
                let (mut left, mut top, mut bottom) = (pointer.x, pointer.y, pointer.y);
                let button = mounted.cloned();
                if let Some(button) = button
                    && let Ok(rect) = button.get_client_rect().await
                {
                    (left, top, bottom) = (rect.min_x(), rect.min_y(), rect.max_y());
                }
                // A window whose height cannot be read takes the menu under the button.
                let window = document::eval(WINDOW_HEIGHT_SCRIPT).join::<f64>().await;
                let window = window.map_or(i32::MAX, pixels);
                let (x, top, bottom) = (pixels(left), pixels(top), pixels(bottom));
                cockpit.update(|state| {
                    let side = terminals::shell_menu_side(state, agent, top, bottom, window);
                    let y = match side {
                        MenuSide::Below => bottom,
                        MenuSide::Above => top,
                    };
                    terminals::toggle_shell_menu(state, agent, dock, x, y, side);
                });
            },
            Icon { kind: IconKind::ChevronDown, size: 12 }
        }
    }
}

/// Whole pixels, which are enough to place a menu.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a window's pixels fit in i32"
)]
fn pixels(value: f64) -> i32 {
    value.round() as i32
}

#[component]
pub fn ShellMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = terminals::shell_menu(&cockpit.state.read());
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    // A menu opened again starts with nothing highlighted.
    let key = format!("{}-{}", shown.opening, shown.value.agent.get());
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                ShellList { key: "{key}", menu: shown.value.clone() }
            }
        }
    }
}

#[component]
fn ShellList(menu: terminals::ShellMenu) -> Element {
    let cockpit: Cockpit = use_context();
    let mut highlighted = use_signal(|| None::<usize>);
    let terminals::ShellMenu {
        agent,
        dock,
        x,
        y,
        side,
        shells,
    } = menu;
    let count = shells.len();
    let pick = {
        let shells = shells.clone();
        move |index: usize| {
            if let Some(shell) = shells.get(index) {
                let program = shell.program.clone();
                bridge::give_focus_back(None);
                cockpit.change(|state| terminals::pick_shell(state, agent, program, dock));
            }
        }
    };
    let now = highlighted();
    rsx! {
        KeyedMenu {
            x,
            y,
            focus_back: None,
            class: match side {
                MenuSide::Below => "shell-menu",
                MenuSide::Above => "shell-menu above",
            },
            on_key: {
                let pick = pick.clone();
                move |key: MenuKey| {
                    let now = *highlighted.peek();
                    match key {
                        MenuKey::Down if count > 0 => {
                            highlighted.set(Some(now.map_or(0, |at| (at + 1).min(count - 1))));
                        }
                        MenuKey::Up if count > 0 => {
                            highlighted.set(Some(now.map_or(0, |at| at.saturating_sub(1))));
                        }
                        MenuKey::Enter => {
                            if let Some(at) = now {
                                pick(at);
                            }
                        }
                        _ => {}
                    }
                }
            },
            div { class: "shell-menu-head", "New terminal with" }
            for (index, shell) in shells.into_iter().enumerate() {
                {
                    let pick = pick.clone();
                    let label = terminals::program_label(&shell.program);
                    let path = shell.program.display().to_string();
                    rsx! {
                        button {
                            key: "{path}",
                            class: if now == Some(index) { format!("menu-item {HIGHLIGHTED_CLASS}") } else { "menu-item".to_owned() },
                            role: "menuitem",
                            tabindex: -1,
                            title: "{path}",
                            onmouseenter: move |_| {
                                if *highlighted.peek() != Some(index) {
                                    highlighted.set(Some(index));
                                }
                            },
                            onclick: move |_| pick(index),
                            span { class: "menu-label", "{shell.name}" }
                            if shell.default {
                                span { class: "shell-default", "· default" }
                            }
                            span { class: "shell-program mono", "{label}" }
                        }
                    }
                }
            }
        }
    }
}

//! The notification levels in the window: the sidebar's bell for the whole app, the header's tag
//! for the selected agent, and the choices both open, which the row menu shows too.

use cockpit_protocol::AgentId;
use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::prefs::NotificationLevel;
use crate::state::{Popover, notification_levels};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::{Presence, use_presence};

/// A choice, as the menus list it.
pub fn choice_label(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::All => "All",
        NotificationLevel::NeedsYou => "Only when it needs you",
        NotificationLevel::Off => "Off",
    }
}

/// A choice in a few words, as the row menu's "Notifications" row shows the current one.
pub fn short_label(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::NeedsYou => "Needs you",
        NotificationLevel::All | NotificationLevel::Off => choice_label(level),
    }
}

/// An agent's level, as its row's bell says it.
pub fn agent_label(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::All => "All notifications",
        NotificationLevel::NeedsYou => "Notifies only when it needs you",
        NotificationLevel::Off => "Notifications off",
    }
}

/// An agent's level, as its header tag says it, shorter than the row's.
fn tag_label(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::NeedsYou => "Needs you only",
        NotificationLevel::All | NotificationLevel::Off => agent_label(level),
    }
}

fn app_label(level: NotificationLevel) -> &'static str {
    match level {
        NotificationLevel::All => "Notifications: all",
        NotificationLevel::NeedsYou => "Notifications: only when an agent needs you",
        NotificationLevel::Off => "Notifications are off",
    }
}

/// The level's bell: crossed out when off, with an amber dot for "needs you" only.
#[component]
pub fn LevelIcon(level: NotificationLevel, size: u16) -> Element {
    match level {
        NotificationLevel::Off => rsx! {
            Icon { kind: IconKind::BellOff, size }
        },
        NotificationLevel::All | NotificationLevel::NeedsYou => rsx! {
            span { class: "level-bell",
                Icon { kind: IconKind::Bell, size }
                if level == NotificationLevel::NeedsYou {
                    span { class: "bell-dot", aria_hidden: true }
                }
            }
        },
    }
}

/// The three levels, the current one checked. `agent` is `None` for the app's.
#[component]
pub fn LevelChoices(agent: Option<AgentId>, level: NotificationLevel) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        for &choice in NotificationLevel::VARIANTS {
            button {
                key: "{choice_label(choice)}",
                class: if choice == level { "menu-item notification-choice chosen" } else { "menu-item notification-choice" },
                role: "menuitemradio",
                aria_checked: choice == level,
                onclick: move |_| cockpit.set_notifications(agent, choice),
                span { class: "menu-label", "{choice_label(choice)}" }
                if choice == level {
                    Icon { kind: IconKind::Check, size: 14 }
                }
            }
        }
    }
}

/// The sidebar's bell, which sets the app's level.
#[component]
pub fn AppBell() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let level = notification_levels::app_level(&state);
    let open = state.ui.popover == Some(Popover::AppNotifications);
    let label = app_label(level);
    let class = match (level, open) {
        (NotificationLevel::Off, true) => "icon-button app-bell off active",
        (NotificationLevel::Off, false) => "icon-button app-bell off",
        (_, true) => "icon-button app-bell active",
        (_, false) => "icon-button app-bell",
    };
    rsx! {
        div { class: "bell-anchor",
            button {
                class,
                title: if open { String::new() } else { label.to_owned() },
                aria_label: "{label}",
                aria_expanded: open,
                onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::AppNotifications)),
                LevelIcon { level, size: 16 }
            }
            Presence { open,
                div { class: "popover menu notification-menu app-notifications", role: "menu",
                    span { class: "menu-title", "Notifications for the app" }
                    LevelChoices { agent: None, level }
                    p { class: "menu-note",
                        "Each agent keeps its own setting, which applies again once the app's are on."
                    }
                }
            }
        }
    }
}

/// The header's tag for an agent whose level is not "All", which opens its choices.
#[component]
pub fn AgentNotificationTag(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let level = notification_levels::agent_level(&state, agent);
    let open = state.ui.popover == Some(Popover::AgentNotifications);
    // While it leaves, the tag keeps the level it showed.
    let Some(shown) = use_presence((level != NotificationLevel::All).then_some(level)) else {
        return rsx! {};
    };
    let shown_level = shown.value;
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            span { class: "tag-anchor",
                button {
                    class: if open { "notification-tag open" } else { "notification-tag" },
                    title: if open { "" } else { "This agent's notifications: click to change them" },
                    aria_expanded: open,
                    onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::AgentNotifications)),
                    LevelIcon { level: shown_level, size: 12 }
                    "{tag_label(shown_level)}"
                }
                Presence { open,
                    div { class: "popover menu notification-menu agent-notifications", role: "menu",
                        span { class: "menu-title", "This agent's notifications" }
                        LevelChoices { agent: Some(agent), level }
                    }
                }
            }
        }
    }
}

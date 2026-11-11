//! The line above the sidebar's list that says what its panels group, and opens the choices.

use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::prefs::GroupBy;
use crate::state::{Popover, grouping};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::Presence;

/// A choice, as the switch and the Settings dialog name it.
pub fn group_by_label(group_by: GroupBy) -> &'static str {
    match group_by {
        GroupBy::Machine => "Machine",
        GroupBy::Project => "Project",
        GroupBy::None => "None",
    }
}

/// The choice inside the line's sentence.
fn group_by_words(group_by: GroupBy) -> &'static str {
    match group_by {
        GroupBy::Machine => "by machine",
        GroupBy::Project => "by project",
        GroupBy::None => "in one list",
    }
}

#[component]
pub fn GroupSwitch() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let group_by = state.prefs.sidebar.group_by;
    let open = state.ui.popover == Some(Popover::GroupBy);
    rsx! {
        div { class: "group-anchor",
            button {
                class: if open { "group-switch open" } else { "group-switch" },
                aria_haspopup: "menu",
                aria_expanded: open,
                title: "Group the agents",
                onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::GroupBy)),
                "Grouped "
                b { "{group_by_words(group_by)}" }
                Icon { kind: IconKind::ChevronDown, size: 12 }
            }
            Presence { open,
                div { class: "popover menu group-menu", role: "menu",
                    span { class: "menu-title", "Group the agents by" }
                    for &choice in GroupBy::VARIANTS {
                        button {
                            key: "{group_by_label(choice)}",
                            class: if choice == group_by { "menu-item chosen" } else { "menu-item" },
                            role: "menuitemradio",
                            aria_checked: choice == group_by,
                            onclick: move |_| cockpit.update(|state| grouping::set_group_by(state, choice)),
                            span { class: "menu-label", "{group_by_label(choice)}" }
                            if choice == group_by {
                                Icon { kind: IconKind::Check, size: 14 }
                            }
                        }
                    }
                }
            }
        }
    }
}

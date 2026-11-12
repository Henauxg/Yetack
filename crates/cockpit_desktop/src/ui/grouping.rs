//! The line above the sidebar's list that says what its panels group, and opens the choices.

use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::prefs::{GroupBy, ProjectSplit};
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

/// Under Project: how a project lists the agents of several machines.
pub fn project_split_label(split: ProjectSplit) -> &'static str {
    match split {
        ProjectSplit::OneList => "One list",
        ProjectSplit::ByMachine => "By machine",
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
    let split = state.prefs.sidebar.project_split;
    // A split needs a second machine.
    let splits = group_by == GroupBy::Project && !state.machines.others.is_empty();
    let words = if grouping::splits_by_machine(&state) {
        "by project and machine"
    } else {
        group_by_words(group_by)
    };
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
                b { "{words}" }
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
                        if choice == GroupBy::Project && splits {
                            div {
                                class: "segmented project-split",
                                role: "group",
                                aria_label: "A project's agents on several machines",
                                for &option in ProjectSplit::VARIANTS {
                                    button {
                                        key: "{project_split_label(option)}",
                                        class: if option == split { "segment chosen" } else { "segment" },
                                        role: "menuitemradio",
                                        aria_checked: option == split,
                                        onclick: move |_| cockpit.update(|state| grouping::set_project_split(state, option)),
                                        "{project_split_label(option)}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

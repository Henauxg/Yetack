//! The Settings dialog's Projects tab: a card per project, as the Accounts dialog shows accounts.
//! A kept project's card renames it, colors it, edits its rules and deletes it. A temporary
//! project's card shows its repository and keeps it.

use std::collections::HashMap;

use cockpit_protocol::Repository;

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::prefs::{AccountColor, GroupBy, ProjectId, ProjectRule};
use crate::state::projects::{self, ProjectKey, RuleKind};
use crate::state::{AppState, SettingsView, grouping, settings};
use crate::ui::accounts::{self, Swatches};
use crate::ui::grouping::group_by_label;
use crate::ui::icons::{Icon, IconKind};
use crate::{bridge, format};

/// The open card's fields: one card opens at a time.
const NAME_FIELD_ID: &str = "project-name-field";
const RULE_FIELD_ID: &str = "project-rule-field";

/// A project's card, read as the tab renders.
#[derive(Debug, Clone, PartialEq)]
struct ProjectCard {
    key: ProjectKey,
    name: String,
    color: AccountColor,
    temporary: bool,
    rules: Vec<ProjectRule>,
    agents: usize,
}

/// The cards: the kept projects in their order, then the temporary ones by name.
fn project_cards(state: &AppState) -> Vec<ProjectCard> {
    let mut agents: HashMap<ProjectKey, usize> = HashMap::new();
    for entry in state.agents.values() {
        *agents
            .entry(projects::project_of(state, entry))
            .or_default() += 1;
    }
    let count = |key: &ProjectKey| agents.get(key).copied().unwrap_or(0);
    let mut cards: Vec<ProjectCard> = state
        .prefs
        .projects
        .list
        .iter()
        .map(|project| {
            let key = ProjectKey::Kept(project.id);
            ProjectCard {
                agents: count(&key),
                key,
                name: project.name.clone(),
                color: project.color,
                temporary: false,
                rules: project.rules.clone(),
            }
        })
        .collect();
    for key in projects::shown_projects(state) {
        let ProjectKey::Repository(repository) = &key else {
            continue;
        };
        let look = projects::look(state, &key);
        cards.push(ProjectCard {
            agents: count(&key),
            name: look.name,
            color: look.color.unwrap_or(AccountColor::Blue),
            temporary: true,
            rules: vec![ProjectRule::Repository(repository.clone())],
            key,
        });
    }
    cards
}

fn rule_text(rule: &ProjectRule) -> &str {
    match rule {
        ProjectRule::Repository(repository) => repository.as_str(),
        ProjectRule::Folder(text) => text,
    }
}

/// The style that colors a project's swatch, from the color's CSS value. Empty for "No project",
/// which has no color.
pub fn swatch_style(color: Option<&str>) -> String {
    color
        .map(|color| format!("--project: {color}"))
        .unwrap_or_default()
}

fn kind_label(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Repository => "Repository",
        RuleKind::Folder => "Folder",
    }
}

fn kind_icon(kind: RuleKind) -> IconKind {
    match kind {
        RuleKind::Repository => IconKind::Repository,
        RuleKind::Folder => IconKind::Folder,
    }
}

#[component]
pub fn ProjectsTab(view: SettingsView) -> Element {
    let cockpit: Cockpit = use_context();
    let (cards, group_by) = {
        let state = cockpit.state.read();
        (project_cards(&state), state.prefs.sidebar.group_by)
    };
    rsx! {
        div { class: "dialog-body",
            p { class: "setting-help projects-intro",
                "An agent joins the project whose rule holds its folder most precisely: a folder, then its repository, then a folder that ends with *, the longest first. A repository no project claims gets a temporary project, which goes away with its last agent unless you keep it."
            }
            if group_by != GroupBy::Project {
                div { class: "projects-grouping",
                    span { class: "setting-help",
                        "The sidebar groups the agents by {group_by_label(group_by).to_lowercase()} now."
                    }
                    button {
                        class: "button small",
                        onclick: move |_| cockpit.update(|state| grouping::set_group_by(state, GroupBy::Project)),
                        "Group by project"
                    }
                }
            }
            if cards.is_empty() {
                p { class: "setting-help", "No project yet: agents in a git repository propose one." }
            }
            div { class: "acct-list projects",
                for card in cards {
                    ProjectCardView {
                        key: "{card.key}",
                        open: view.project.as_ref() == Some(&card.key),
                        adding: view.adding_rule,
                        fresh: matches!(card.key, ProjectKey::Kept(id) if view.created == Some(id)),
                        card: card.clone(),
                    }
                }
            }
            div { class: "projects-foot",
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(settings::new_project),
                    Icon { kind: IconKind::Plus, size: 14 }
                    "New project"
                }
            }
        }
    }
}

#[component]
fn ProjectCardView(
    card: ProjectCard,
    open: bool,
    adding: Option<RuleKind>,
    fresh: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let ProjectCard {
        key,
        name,
        color,
        temporary,
        rules,
        agents,
    } = card;
    let agents = format::count(agents, "agent", "agents");
    let summary = rules.iter().map(rule_text).collect::<Vec<_>>().join(" · ");
    let toggled = key.clone();
    let style = accounts::color_style(color);
    rsx! {
        div { class: if open { "acct-card open" } else { "acct-card" }, style,
            button {
                class: "acct-card-head",
                aria_expanded: open,
                onclick: move |_| cockpit.update(|state| settings::toggle_project(state, &toggled)),
                span { class: if temporary { "acct-swatch temporary" } else { "acct-swatch" }, aria_hidden: true }
                span { class: "acct-card-name",
                    b {
                        "{name}"
                        if temporary {
                            span { class: "default-tag", "Temporary" }
                        }
                    }
                    if summary.is_empty() {
                        span { class: "acct-card-folder", "No rule: only the agents moved to it" }
                    } else {
                        span { class: "acct-card-folder mono", "{summary}" }
                    }
                }
                span { class: "acct-card-agents", "{agents}" }
                span { class: if open { "chevron open" } else { "chevron" },
                    Icon { kind: IconKind::ChevronRight, size: 14 }
                }
            }
            if open {
                match key {
                    ProjectKey::Kept(id) => rsx! {
                        KeptBody { id, name, color, rules, adding, fresh }
                    },
                    ProjectKey::Repository(repository) => rsx! {
                        TemporaryBody { repository }
                    },
                    ProjectKey::None => rsx! {},
                }
            }
        }
    }
}

#[component]
fn KeptBody(
    id: ProjectId,
    name: String,
    color: AccountColor,
    rules: Vec<ProjectRule>,
    adding: Option<RuleKind>,
    fresh: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div { class: "acct-card-body",
            div { class: "form-row",
                div { class: "field-group",
                    label { class: "field-label", r#for: NAME_FIELD_ID, "Name" }
                    input {
                        // A rename from elsewhere mounts the field afresh, with the new name.
                        key: "{name}",
                        id: NAME_FIELD_ID,
                        class: "text-input",
                        r#type: "text",
                        spellcheck: false,
                        initial_value: "{name}",
                        onmounted: move |_| {
                            if fresh {
                                bridge::focus_field_at_end(NAME_FIELD_ID);
                            }
                        },
                        onchange: move |event: FormEvent| {
                            let typed = event.value();
                            cockpit.update(|state| projects::rename(state, id, &typed));
                        },
                    }
                }
                div { class: "field-group",
                    span { class: "field-label", "Color" }
                    Swatches {
                        chosen: color,
                        onpick: move |color| cockpit.update(|state| projects::set_color(state, id, color)),
                    }
                }
            }
            div { class: "field-group",
                span { class: "field-label", "An agent joins it when its folder is in" }
                if rules.is_empty() {
                    span { class: "hint", "No rule yet: only the agents moved to it from their row's menu, or by a drag, join it." }
                } else {
                    div { class: "rule-list",
                        for (index, rule) in rules.into_iter().enumerate() {
                            RuleRow { key: "{index}", id, index, rule }
                        }
                    }
                }
                if let Some(kind) = adding {
                    RuleField { id, kind }
                } else {
                    div { class: "rule-adds",
                        button {
                            class: "button small",
                            onclick: move |_| cockpit.update(|state| settings::show_rule_field(state, Some(RuleKind::Repository))),
                            Icon { kind: IconKind::Plus, size: 14 }
                            {kind_label(RuleKind::Repository)}
                        }
                        button {
                            class: "button small",
                            onclick: move |_| cockpit.update(|state| settings::show_rule_field(state, Some(RuleKind::Folder))),
                            Icon { kind: IconKind::Plus, size: 14 }
                            {kind_label(RuleKind::Folder)}
                        }
                    }
                }
            }
            div { class: "acct-card-actions",
                button {
                    class: "button danger small",
                    onclick: move |_| cockpit.update(|state| settings::delete_project(state, id)),
                    "Delete"
                }
                span { class: "hint", "Its agents go back to the project their folder matches, or to No project." }
            }
        }
    }
}

#[component]
fn RuleRow(id: ProjectId, index: usize, rule: ProjectRule) -> Element {
    let cockpit: Cockpit = use_context();
    let kind = RuleKind::from(&rule);
    let text = rule_text(&rule).to_owned();
    rsx! {
        div { class: "rule-row",
            Icon { kind: kind_icon(kind), size: 14 }
            span { class: "rule-value mono", title: "{text}", "{text}" }
            span { class: "rule-kind", {kind_label(kind)} }
            button {
                class: "icon-button tiny",
                title: "Remove this rule",
                aria_label: "Remove {text}",
                onclick: move |_| cockpit.update(|state| projects::remove_rule(state, id, index)),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

/// The field a new rule is typed in: Enter adds it, Esc puts the buttons back.
#[component]
fn RuleField(id: ProjectId, kind: RuleKind) -> Element {
    let cockpit: Cockpit = use_context();
    let mut text = use_signal(String::new);
    let placeholder = match kind {
        RuleKind::Repository => "github.com/acme/app",
        RuleKind::Folder => "/home/me/work/app, or /home/me/work/*",
    };
    let empty = text.read().trim().is_empty();
    let add = move || {
        let typed = text.peek().clone();
        cockpit.update(|state| settings::add_rule(state, id, kind, typed));
    };
    rsx! {
        div { class: "rule-row adding",
            Icon { kind: kind_icon(kind), size: 14 }
            input {
                id: RULE_FIELD_ID,
                class: "rule-input mono",
                r#type: "text",
                spellcheck: false,
                aria_label: kind_label(kind),
                placeholder,
                onmounted: move |_| bridge::focus_field_at_end(RULE_FIELD_ID),
                oninput: move |event: FormEvent| text.set(event.value()),
                // Esc reaches the app's keys, which close the field before the dialog.
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter {
                        event.prevent_default();
                        add();
                    }
                },
            }
            button {
                class: if empty { "button small disabled" } else { "button small primary" },
                disabled: empty,
                onclick: move |_| add(),
                "Add"
            }
            button {
                class: "icon-button tiny",
                title: "Cancel",
                aria_label: "Cancel",
                onclick: move |_| cockpit.update(|state| settings::show_rule_field(state, None)),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

#[component]
fn TemporaryBody(repository: Repository) -> Element {
    let cockpit: Cockpit = use_context();
    let text = repository.clone();
    rsx! {
        div { class: "acct-card-body",
            div { class: "field-group",
                span { class: "field-label", "An agent joins it when its folder is in" }
                div { class: "rule-list",
                    div { class: "rule-row",
                        Icon { kind: kind_icon(RuleKind::Repository), size: 14 }
                        span { class: "rule-value mono", title: "{text}", "{text}" }
                        span { class: "rule-kind", {kind_label(RuleKind::Repository)} }
                    }
                }
            }
            div { class: "acct-card-actions",
                button {
                    class: "button small",
                    onclick: move |_| {
                        let repository = repository.clone();
                        cockpit.update(move |state| settings::keep_project(state, &repository));
                    },
                    "Keep"
                }
                span { class: "hint", "Keep it to rename it, color it, or give it more rules. Until then it goes away with its last agent." }
            }
        }
    }
}

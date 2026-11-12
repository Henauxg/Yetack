//! The menu a right-click on a project's header in the sidebar opens, at the pointer, and the
//! field that renames the project in its header.
//!
//! The pointer and the arrows move one highlight through the rows. A letter picks the row that
//! shows it, and a digit from 1 to 6 picks a color.

use dioxus::prelude::*;
use strum::VariantArray;

use crate::bridge::{self, HIGHLIGHTED_CLASS};
use crate::connection::Cockpit;
use crate::prefs::{AccountColor, ProjectId};
use crate::state::grouping::{self, GroupKey};
use crate::state::menu_keys::{self, KeyedRow, MenuKey};
use crate::state::projects::{self, ProjectKey};
use crate::state::{AppState, Popover, project_menu};
use crate::ui::menu::KeyedMenu;
use crate::ui::presence::use_presence;
use crate::ui::{accounts, projects_tab};

/// The field that holds a project's name in its header.
pub const PROJECT_NAME_FIELD_ID: &str = "project-name-field";

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Keep,
    Rename,
    Rules,
    Fold,
    Delete,
}

impl Item {
    /// The letter that picks it.
    fn key(self) -> char {
        match self {
            Self::Keep => 'k',
            Self::Rename => 'r',
            Self::Rules => 'e',
            Self::Fold => 'f',
            Self::Delete => 'd',
        }
    }
}

/// What the menu shows, read as it opens.
#[derive(Debug, Clone, PartialEq)]
struct ProjectMenuShown {
    project: ProjectKey,
    x: i32,
    y: i32,
    name: String,
    color: Option<AccountColor>,
    temporary: bool,
    folded: bool,
    rules: usize,
}

impl ProjectMenuShown {
    fn read(state: &AppState) -> Option<Self> {
        let Some(Popover::ProjectMenu { x, y }) = state.ui.popover else {
            return None;
        };
        let project = project_menu::shown(state)?.clone();
        let look = projects::look(state, &project);
        let rules = match &project {
            ProjectKey::Kept(id) => projects::kept(state, *id).map_or(0, |kept| kept.rules.len()),
            ProjectKey::Repository(_) | ProjectKey::None => 0,
        };
        Some(Self {
            folded: grouping::is_folded(state, &GroupKey::Project(project.clone())),
            project,
            x,
            y,
            name: look.name,
            color: look.color,
            temporary: look.temporary,
            rules,
        })
    }

    /// The rows, in order. "No project" has no name, color or rules of its own to edit.
    fn items(&self) -> Vec<Item> {
        match self.project {
            ProjectKey::None => vec![Item::Fold],
            ProjectKey::Repository(_) => vec![Item::Keep, Item::Rename, Item::Rules, Item::Fold],
            ProjectKey::Kept(_) => vec![Item::Rename, Item::Rules, Item::Fold, Item::Delete],
        }
    }

    /// Whether the color row shows: a temporary project is kept by a pick.
    fn has_colors(&self) -> bool {
        self.project != ProjectKey::None
    }
}

/// The color a digit picks, 1 for the first.
fn digit_color(letter: char) -> Option<AccountColor> {
    let index = letter.to_digit(10)?.checked_sub(1)?;
    AccountColor::VARIANTS
        .get(usize::try_from(index).ok()?)
        .copied()
}

/// The project menu, at the pointer that opened it.
#[component]
pub fn ProjectMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = ProjectMenuShown::read(&cockpit.state.read());
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    // A menu opened again starts with nothing highlighted.
    let key = format!("{}-{}-{}", shown.opening, shown.value.x, shown.value.y);
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                MenuBody { key: "{key}", shown: shown.value.clone() }
            }
        }
    }
}

#[component]
fn MenuBody(shown: ProjectMenuShown) -> Element {
    let cockpit: Cockpit = use_context();
    let mut highlighted = use_signal(|| None::<usize>);
    // Delete asks first: the row says so until a second pick.
    let mut confirming = use_signal(|| false);
    let items = shown.items();
    let keyed: Vec<KeyedRow> = items
        .iter()
        .map(|item| KeyedRow {
            key: item.key(),
            enabled: true,
        })
        .collect();
    let project = shown.project.clone();
    let act = move |item: Item| {
        let project = project.clone();
        if item == Item::Delete && !*confirming.peek() {
            confirming.set(true);
            return;
        }
        cockpit.update(|state| match item {
            Item::Keep => project_menu::keep(state, &project),
            Item::Rename => project_menu::start_rename(state, &project),
            Item::Rules => project_menu::open_rules(state, &project),
            Item::Fold => project_menu::toggle_folded(state, &project),
            Item::Delete => project_menu::delete(state, &project),
        });
    };
    let pick_color = {
        let project = shown.project.clone();
        move |color: AccountColor| {
            let project = project.clone();
            cockpit.update(|state| project_menu::set_color(state, &project, color));
        }
    };
    let rules_hint = match (&shown.project, shown.rules) {
        (ProjectKey::Repository(_), _) => "its repository".to_owned(),
        (_, 1) => "1 rule".to_owned(),
        (_, rules) => format!("{rules} rules"),
    };
    let swatch_class = match (shown.color, shown.temporary) {
        (None, _) => "menu-swatch none",
        (Some(_), true) => "menu-swatch temporary",
        (Some(_), false) => "menu-swatch",
    };
    let swatch_style = projects_tab::swatch_style(shown.color.map(AccountColor::css));
    let now = highlighted();
    let colors = shown.has_colors();
    rsx! {
        KeyedMenu {
            x: shown.x,
            y: shown.y,
            focus_back: None,
            on_key: {
                let items = items.clone();
                let pick_color = pick_color.clone();
                let mut act = act.clone();
                move |key: MenuKey| {
                    if let MenuKey::Char(letter) = key
                        && colors
                        && let Some(color) = digit_color(letter)
                    {
                        pick_color(color);
                        return;
                    }
                    let step = menu_keys::step(&keyed, *highlighted.peek(), &key);
                    if step.highlighted != *highlighted.peek() {
                        highlighted.set(step.highlighted);
                    }
                    if let Some(item) = step.picked.and_then(|index| items.get(index)).copied() {
                        act(item);
                    }
                }
            },
            div { class: "menu-title project-menu-title",
                span { class: swatch_class, style: swatch_style, aria_hidden: true }
                span { class: "project-menu-name", "{shown.name}" }
                if shown.temporary {
                    span { class: "menu-tag", "temporary" }
                }
            }
            for (index, item) in items.into_iter().enumerate() {
                if item == Item::Fold || item == Item::Delete {
                    div { key: "divider-{index}", class: "menu-divider" }
                }
                {
                    let (label, title) = match item {
                        Item::Keep => ("Keep the project", "Keep it when it has no agent left, with its name, color and rules"),
                        Item::Rename => ("Rename…", "Edit the name in the sidebar"),
                        Item::Rules => ("Edit the rules…", "Open its card in Settings, where the rules pick its agents"),
                        Item::Fold if shown.folded => ("Show its agents", ""),
                        Item::Fold => ("Fold its agents", ""),
                        Item::Delete if confirming() => ("Delete it: pick again", "Its agents go to the project their folder picks"),
                        Item::Delete => ("Delete the project…", "Its agents go to the project their folder picks"),
                    };
                    let hint = match item {
                        Item::Rules => rules_hint.clone(),
                        _ => item.key().to_ascii_uppercase().to_string(),
                    };
                    let class = match (item == Item::Delete, now == Some(index)) {
                        (true, true) => format!("menu-item danger {HIGHLIGHTED_CLASS}"),
                        (true, false) => "menu-item danger".to_owned(),
                        (false, true) => format!("menu-item {HIGHLIGHTED_CLASS}"),
                        (false, false) => "menu-item".to_owned(),
                    };
                    let mut act = act.clone();
                    rsx! {
                        button {
                            key: "{index}",
                            class,
                            role: "menuitem",
                            tabindex: -1,
                            title,
                            onmouseenter: move |_| {
                                if *highlighted.peek() != Some(index) {
                                    highlighted.set(Some(index));
                                }
                            },
                            onclick: move |_| act(item),
                            span { class: "menu-label", "{label}" }
                            span { class: "menu-hint", "{hint}" }
                        }
                        if item == Item::Rename && colors {
                            div { class: "swatches menu-swatches", role: "radiogroup", aria_label: "Color",
                                for (number, color) in (1..).zip(AccountColor::VARIANTS.iter().copied()) {
                                    {
                                        let pick_color = pick_color.clone();
                                        let chosen = shown.color == Some(color) && !shown.temporary;
                                        rsx! {
                                            button {
                                                key: "{color.name()}",
                                                class: if chosen { "swatch chosen" } else { "swatch" },
                                                style: accounts::color_style(color),
                                                role: "radio",
                                                tabindex: -1,
                                                aria_checked: chosen,
                                                title: "{color.name()} ({number})",
                                                onclick: move |_| pick_color(color),
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
    }
}

/// The project's name as a field in its header: Enter or leaving the field saves it, Esc puts the
/// old name back.
#[component]
pub fn ProjectNameField(id: ProjectId, name: String) -> Element {
    let cockpit: Cockpit = use_context();
    let mut typed = use_signal(|| name.clone());
    let save = move || {
        let text = typed.peek().clone();
        cockpit.update(|state| project_menu::finish_rename(state, id, &text));
    };
    rsx! {
        input {
            id: PROJECT_NAME_FIELD_ID,
            class: "row-title-field project-name-field",
            r#type: "text",
            aria_label: "Project name",
            initial_value: "{name}",
            onmounted: move |_| bridge::focus_title_field(PROJECT_NAME_FIELD_ID, true),
            oninput: move |event: FormEvent| typed.set(event.value()),
            onkeydown: move |event: KeyboardEvent| match event.key() {
                Key::Enter => save(),
                Key::Escape => cockpit.update(project_menu::cancel_rename),
                _ => {}
            },
            onblur: move |_| bridge::save_on_blur(save, || {}),
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::Repository;

    use super::*;

    fn shown(project: ProjectKey) -> ProjectMenuShown {
        ProjectMenuShown {
            project,
            x: 0,
            y: 0,
            name: "calc".to_owned(),
            color: None,
            temporary: false,
            folded: false,
            rules: 0,
        }
    }

    #[test]
    fn each_kind_of_project_lists_what_it_can_change() {
        let temporary = shown(ProjectKey::Repository(Repository::from_remote(
            "https://github.com/acme/calc.git",
        )));
        let kept = shown(ProjectKey::Kept(ProjectId::new(1)));
        let none = shown(ProjectKey::None);

        assert_eq!(
            temporary.items(),
            [Item::Keep, Item::Rename, Item::Rules, Item::Fold]
        );
        assert_eq!(
            kept.items(),
            [Item::Rename, Item::Rules, Item::Fold, Item::Delete]
        );
        assert_eq!(none.items(), [Item::Fold]);
        assert!(!none.has_colors());
    }

    #[test]
    fn the_digits_pick_the_colors_in_their_order() {
        assert_eq!(digit_color('1'), AccountColor::VARIANTS.first().copied());
        assert_eq!(digit_color('6'), AccountColor::VARIANTS.get(5).copied());
        assert_eq!(digit_color('0'), None);
        assert_eq!(digit_color('7'), None);
        assert_eq!(digit_color('k'), None);
    }
}

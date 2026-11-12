//! The menu a right-click on a project's header in the sidebar opens, at the pointer: rename,
//! color, rules, fold and delete. A rename, a color or the rules keep a temporary project first,
//! since only a kept project has them.

use super::app_state::AppState;
use super::grouping::{self, GroupKey};
use super::projects::{self, ProjectKey};
use super::settings;
use super::ui_state::Popover;
use crate::prefs::{AccountColor, GroupBy, ProjectId};

/// Opens the project's menu at the pointer, in the window's pixels.
pub fn open(state: &mut AppState, project: ProjectKey, x: i32, y: i32) {
    state.ui.project_menu = Some(project);
    state.ui.popover = Some(Popover::ProjectMenu { x, y });
}

/// The project whose menu is open, while the sidebar heads it.
pub fn shown(state: &AppState) -> Option<&ProjectKey> {
    match state.ui.popover {
        Some(Popover::ProjectMenu { .. }) => state
            .ui
            .project_menu
            .as_ref()
            .filter(|project| is_headed(state, project)),
        _ => None,
    }
}

/// Whether the sidebar has a header for the project.
fn is_headed(state: &AppState, project: &ProjectKey) -> bool {
    state.prefs.sidebar.group_by == GroupBy::Project
        && projects::shown_projects(state).contains(project)
}

/// Closes the menu and the name field of a project the sidebar no longer heads: its last agent
/// left or moved, the sidebar groups otherwise, or the project went. A field that came back later
/// would take the focus from wherever the user is.
pub fn prune(state: &mut AppState) {
    if state
        .ui
        .project_rename
        .is_some_and(|id| !is_headed(state, &ProjectKey::Kept(id)))
    {
        state.ui.project_rename = None;
    }
    let menu_gone =
        matches!(state.ui.popover, Some(Popover::ProjectMenu { .. })) && shown(state).is_none();
    if menu_gone {
        close(state);
    }
}

fn close(state: &mut AppState) {
    if matches!(state.ui.popover, Some(Popover::ProjectMenu { .. })) {
        state.ui.popover = None;
    }
    state.ui.project_menu = None;
}

/// Puts the project's name in a field in its header.
pub fn start_rename(state: &mut AppState, project: &ProjectKey) {
    close(state);
    state.ui.project_rename = projects::kept_id(state, project);
}

/// Saves the name typed in the header's field. An empty name keeps the old one.
pub fn finish_rename(state: &mut AppState, id: ProjectId, name: &str) {
    if state.ui.project_rename != Some(id) {
        return;
    }
    state.ui.project_rename = None;
    let name = name.trim();
    if !name.is_empty() {
        projects::rename(state, id, name);
    }
}

pub fn cancel_rename(state: &mut AppState) {
    state.ui.project_rename = None;
}

pub fn set_color(state: &mut AppState, project: &ProjectKey, color: AccountColor) {
    close(state);
    if let Some(id) = projects::kept_id(state, project) {
        projects::set_color(state, id, color);
    }
}

/// Opens the project's card in Settings, where its rules are edited.
pub fn open_rules(state: &mut AppState, project: &ProjectKey) {
    close(state);
    if let Some(id) = projects::kept_id(state, project) {
        settings::show_project(state, id);
    }
}

pub fn keep(state: &mut AppState, project: &ProjectKey) {
    close(state);
    projects::kept_id(state, project);
}

pub fn toggle_folded(state: &mut AppState, project: &ProjectKey) {
    close(state);
    grouping::toggle_folded(state, &GroupKey::Project(project.clone()));
}

/// Deletes a kept project: its agents go to the project their folder picks.
pub fn delete(state: &mut AppState, project: &ProjectKey) {
    close(state);
    if let ProjectKey::Kept(id) = project {
        projects::delete(state, *id);
        prune(state);
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentId, AgentStatus, Repository};

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};

    fn repository() -> ProjectKey {
        ProjectKey::Repository(Repository::from_remote("https://github.com/acme/calc.git"))
    }

    #[test]
    fn a_rename_or_a_color_keeps_a_temporary_project_first() {
        let mut state = state_with(Vec::new());
        open(&mut state, repository(), 10, 20);

        start_rename(&mut state, &repository());
        assert_eq!(state.ui.popover, None);
        let Some(id) = state.ui.project_rename else {
            panic!("the rename keeps the temporary project");
        };

        finish_rename(&mut state, id, "  Calculator ");
        set_color(&mut state, &ProjectKey::Kept(id), AccountColor::Green);

        let kept = projects::kept(&state, id);
        assert_eq!(
            kept.map(|project| project.name.as_str()),
            Some("Calculator")
        );
        assert_eq!(kept.map(|project| project.color), Some(AccountColor::Green));
        assert_eq!(state.ui.project_rename, None);
    }

    #[test]
    fn an_empty_name_or_esc_keeps_the_old_name() {
        let mut state = state_with(Vec::new());
        let id = projects::create(&mut state, "billing");

        start_rename(&mut state, &ProjectKey::Kept(id));
        finish_rename(&mut state, id, "   ");
        start_rename(&mut state, &ProjectKey::Kept(id));
        state.ui.close_overlays();
        finish_rename(&mut state, id, "ignored");

        assert_eq!(
            projects::kept(&state, id).map(|project| project.name.as_str()),
            Some("billing")
        );
    }

    #[test]
    fn the_menu_and_the_field_go_with_their_header() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.prefs.sidebar.group_by = GroupBy::Project;
        let id = projects::create(&mut state, "billing");
        projects::move_agent(&mut state, AgentId::new(1), &ProjectKey::Kept(id));
        open(&mut state, ProjectKey::Kept(id), 0, 0);
        state.ui.project_rename = Some(id);
        assert_eq!(shown(&state), Some(&ProjectKey::Kept(id)));

        projects::move_agent(&mut state, AgentId::new(1), &ProjectKey::None);

        assert_eq!(shown(&state), None);
        assert_eq!(state.ui.popover, None);
        assert_eq!(state.ui.project_rename, None);
    }

    #[test]
    fn delete_removes_a_kept_project_and_closes_the_menu() {
        let mut state = state_with(Vec::new());
        let id = projects::create(&mut state, "billing");
        open(&mut state, ProjectKey::Kept(id), 0, 0);

        delete(&mut state, &ProjectKey::Kept(id));

        assert!(projects::kept(&state, id).is_none());
        assert_eq!(state.ui.popover, None);
        assert_eq!(shown(&state), None);
    }
}

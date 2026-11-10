//! Which notifications this desktop raises: a level for the whole app and one per agent, both in
//! the preferences. An alert shows only when both levels let its kind through.

use cockpit_protocol::AgentId;

use super::app_state::AppState;
use super::apply::AlertKind;
use super::ui_state::Popover;
use crate::machine::MachineId;
use crate::prefs::NotificationLevel;

/// The level the window saved, else the config file's.
pub fn app_level(state: &AppState) -> NotificationLevel {
    state
        .prefs
        .notifications
        .app
        .unwrap_or(state.config_notifications)
}

pub fn agent_level(state: &AppState, agent: AgentId) -> NotificationLevel {
    state
        .daemon_machine(agent)
        .map_or(NotificationLevel::All, |machine| {
            state
                .prefs
                .notifications
                .agent(machine, MachineId::daemon_agent(agent))
        })
}

/// Whether an alert of `kind` about `agent` shows.
pub fn shows(state: &AppState, agent: AgentId, kind: AlertKind) -> bool {
    lets_through(app_level(state), kind) && lets_through(agent_level(state, agent), kind)
}

pub fn set_agent(state: &mut AppState, agent: AgentId, level: NotificationLevel) {
    close_menus(state);
    if state.agents.contains_key(&agent) {
        save_agent(state, agent, level);
    }
}

pub fn set_app(state: &mut AppState, level: NotificationLevel) {
    close_menus(state);
    state.prefs.notifications.app = Some(level);
}

/// Drops the level of an agent the daemon removed.
pub fn forget_agent(state: &mut AppState, agent: AgentId) {
    save_agent(state, agent, NotificationLevel::All);
}

/// The config file's `[notifications] enabled = false` turns the app's notifications off, until a
/// choice in the window replaces it. The preferences never save it.
pub fn start_from_config(state: &mut AppState, enabled: bool) {
    state.config_notifications = if enabled {
        NotificationLevel::All
    } else {
        NotificationLevel::Off
    };
}

fn save_agent(state: &mut AppState, agent: AgentId, level: NotificationLevel) {
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    state
        .prefs
        .notifications
        .set_agent(&machine, MachineId::daemon_agent(agent), level);
}

fn lets_through(level: NotificationLevel, kind: AlertKind) -> bool {
    match level {
        NotificationLevel::All => true,
        NotificationLevel::NeedsYou => kind == AlertKind::NeedsYou,
        NotificationLevel::Off => false,
    }
}

fn close_menus(state: &mut AppState) {
    if matches!(
        state.ui.popover,
        Some(Popover::RowMenu { .. } | Popover::AgentNotifications | Popover::AppNotifications)
    ) {
        state.ui.popover = None;
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::{state_with, summary};

    #[test]
    fn an_alert_shows_when_both_levels_let_its_kind_through() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let agent = AgentId::new(1);
        let kinds = [AlertKind::NeedsYou, AlertKind::Finished];
        let shown = |state: &AppState| kinds.map(|kind| shows(state, agent, kind));

        let all = shown(&state);
        set_agent(&mut state, agent, NotificationLevel::NeedsYou);
        let agent_needs_you = shown(&state);
        set_app(&mut state, NotificationLevel::Off);
        let app_off = shown(&state);
        set_app(&mut state, NotificationLevel::All);
        set_agent(&mut state, agent, NotificationLevel::Off);
        let agent_off = shown(&state);

        assert_eq!(all, [true, true]);
        assert_eq!(agent_needs_you, [true, false]);
        assert_eq!(app_off, [false, false]);
        assert_eq!(agent_off, [false, false]);
    }

    #[test]
    fn an_agents_level_is_kept_by_machine_and_dropped_once_it_is_removed() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let agent = AgentId::new(1);

        set_agent(&mut state, agent, NotificationLevel::Off);
        let machine = state.machines.this.daemon.machine.clone();
        let kept = state.prefs.notifications.agent(&machine, agent);
        forget_agent(&mut state, agent);

        assert_eq!(kept, NotificationLevel::Off);
        assert_eq!(agent_level(&state, agent), NotificationLevel::All);
        assert!(state.prefs.notifications.agents.is_empty());
    }

    #[test]
    fn an_unknown_agent_gets_no_level() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        set_agent(&mut state, AgentId::new(9), NotificationLevel::Off);

        assert!(state.prefs.notifications.agents.is_empty());
    }

    #[test]
    fn a_pick_closes_the_menu_it_came_from() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.popover = Some(Popover::AppNotifications);

        set_app(&mut state, NotificationLevel::NeedsYou);

        assert_eq!(state.ui.popover, None);
        assert_eq!(app_level(&state), NotificationLevel::NeedsYou);
    }

    #[test]
    fn the_config_file_turns_notifications_off_until_the_window_chooses() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        start_from_config(&mut state, false);
        let from_config = app_level(&state);
        set_app(&mut state, NotificationLevel::NeedsYou);
        let chosen = app_level(&state);

        assert_eq!(from_config, NotificationLevel::Off);
        assert_eq!(chosen, NotificationLevel::NeedsYou);
    }

    #[test]
    fn the_config_files_level_is_never_saved() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        start_from_config(&mut state, false);
        let saved = state.prefs.notifications.clone();
        start_from_config(&mut state, true);

        assert_eq!(saved.app, None);
        assert_eq!(app_level(&state), NotificationLevel::All);
    }

    #[test]
    fn an_agent_of_an_unknown_machine_leaves_this_computers_levels_alone() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        set_agent(&mut state, AgentId::new(1), NotificationLevel::Off);

        let gone = MachineId::new(1).app_agent(AgentId::new(1));
        forget_agent(&mut state, gone);

        assert_eq!(agent_level(&state, gone), NotificationLevel::All);
        assert_eq!(agent_level(&state, AgentId::new(1)), NotificationLevel::Off);
    }
}

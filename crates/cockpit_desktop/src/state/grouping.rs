//! How the sidebar groups and orders the agents. Grouped by machine, each machine has a panel and
//! its daemon keeps the order of its agents. Grouped by project or not at all, the app keeps one
//! order across every machine in the preferences (`Prefs::agent_order`), and moves only change
//! it there.

use std::collections::HashMap;

use cockpit_protocol::AgentId;

use super::app_state::{AgentEntry, AppState};
use super::projects::{self, ProjectKey};
use super::ui_state::Popover;
use crate::machine::MachineId;
use crate::prefs::{AgentKey, GroupBy};

/// A header of the sidebar: a machine's, or a project's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKey {
    Machine(MachineId),
    Project(ProjectKey),
}

pub fn is_folded(state: &AppState, group: &GroupKey) -> bool {
    match group {
        GroupKey::Machine(machine) => state.ui.folded_machines.contains(machine),
        GroupKey::Project(project) => state.ui.folded_projects.contains(project),
    }
}

pub fn set_folded(state: &mut AppState, group: &GroupKey, folded: bool) {
    match (group, folded) {
        (GroupKey::Machine(machine), true) => {
            state.ui.folded_machines.insert(*machine);
        }
        (GroupKey::Machine(machine), false) => {
            state.ui.folded_machines.remove(machine);
        }
        (GroupKey::Project(project), true) => {
            state.ui.folded_projects.insert(project.clone());
        }
        (GroupKey::Project(project), false) => {
            state.ui.folded_projects.remove(project);
        }
    }
}

pub fn toggle_folded(state: &mut AppState, group: &GroupKey) {
    let folded = is_folded(state, group);
    set_folded(state, group, !folded);
}

/// The header the agent's row shows under, if the sidebar has headers.
pub fn group_of(state: &AppState, agent: AgentId) -> Option<GroupKey> {
    match state.prefs.sidebar.group_by {
        GroupBy::None => None,
        GroupBy::Machine => {
            (!state.machines.others.is_empty()).then(|| GroupKey::Machine(MachineId::of(agent)))
        }
        GroupBy::Project => state
            .agents
            .get(&agent)
            .map(|entry| GroupKey::Project(projects::project_of(state, entry))),
    }
}

/// The agent as the preferences name it. `None` for a machine the app no longer knows.
pub fn agent_key(state: &AppState, agent: AgentId) -> Option<AgentKey> {
    Some(AgentKey {
        machine: state.daemon_machine(agent)?.to_owned(),
        agent: MachineId::daemon_agent(agent),
    })
}

/// Every agent in the app's order: the ones the order does not hold yet first, in the daemons'
/// order, then the others as the order puts them.
pub fn app_order(state: &AppState) -> Vec<&AgentEntry> {
    let index: HashMap<&AgentKey, usize> = state
        .prefs
        .agent_order
        .iter()
        .enumerate()
        .map(|(at, key)| (key, at))
        .collect();
    let mut ranked: Vec<(Option<usize>, usize, &AgentEntry)> = state
        .agents_in_order()
        .into_iter()
        .enumerate()
        .map(|(daemon_at, entry)| {
            let at = agent_key(state, entry.id()).and_then(|key| index.get(&key).copied());
            (at, daemon_at, entry)
        })
        .collect();
    ranked.sort_by_key(|&(at, daemon_at, _)| (at.is_some(), at, daemon_at));
    ranked.into_iter().map(|(_, _, entry)| entry).collect()
}

/// Moves `agent` just above `before`, or to the bottom, in the app's order. The order then holds
/// every agent, so that one that arrives later shows above them all.
pub fn move_in_app_order(state: &mut AppState, agent: AgentId, before: Option<AgentId>) {
    let Some(moved) = agent_key(state, agent) else {
        return;
    };
    let mut keys: Vec<AgentKey> = app_order(state)
        .iter()
        .filter_map(|entry| agent_key(state, entry.id()))
        .filter(|key| *key != moved)
        .collect();
    let at = before
        .and_then(|before| agent_key(state, before))
        .and_then(|before| keys.iter().position(|key| *key == before))
        .unwrap_or(keys.len());
    keys.insert(at, moved);
    // Agents of a machine the app does not reach now keep their places: the others take the
    // places the order gave them, in their new order.
    let mut placed = keys.iter().cloned();
    let mut order: Vec<AgentKey> = state
        .prefs
        .agent_order
        .iter()
        .filter_map(|key| {
            if keys.contains(key) {
                placed.next()
            } else {
                Some(key.clone())
            }
        })
        .collect();
    order.extend(placed);
    state.prefs.agent_order = order;
}

/// Drops the place of an agent the daemon removed, whose id a later agent may take.
pub fn forget(state: &mut AppState, agent: AgentId) {
    if let Some(key) = agent_key(state, agent) {
        state.prefs.agent_order.retain(|kept| *kept != key);
    }
}

pub fn set_group_by(state: &mut AppState, group_by: GroupBy) {
    if state.ui.popover == Some(Popover::GroupBy) {
        state.ui.popover = None;
    }
    state.prefs.sidebar.group_by = group_by;
}

/// The machine a row names, when no panel says it: grouped by project or not at all, with
/// several machines, for an agent that does not run on this computer. `None` otherwise.
pub fn machine_shown_on_row(state: &AppState, agent: AgentId) -> Option<&str> {
    let one_list = state.prefs.sidebar.group_by != GroupBy::Machine;
    let several = !state.machines.others.is_empty();
    let elsewhere = MachineId::of(agent) != MachineId::THIS;
    if one_list && several && elsewhere {
        state.daemon_machine(agent)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::MachineState;
    use crate::state::app_state::tests::{placed, state_with};

    fn devbox() -> MachineId {
        MachineId::new(1)
    }

    /// This computer runs agents 1 and 2, the devbox its agent 1, each on top of its machine.
    fn two_machines() -> AppState {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 2048),
            placed(2, AgentStatus::Idle, 1024),
        ]);
        state.machines.others.insert(
            devbox(),
            MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
        );
        let mut remote = placed(1, AgentStatus::Idle, 2048);
        remote.id = devbox().app_agent(AgentId::new(1));
        state.agents.insert(remote.id, AgentEntry::new(remote));
        state
    }

    fn ids(entries: &[&AgentEntry]) -> Vec<AgentId> {
        entries.iter().map(|entry| entry.id()).collect()
    }

    #[test]
    fn a_move_orders_agents_across_machines_and_a_new_agent_comes_first() {
        let mut state = two_machines();
        let remote = devbox().app_agent(AgentId::new(1));
        state.prefs.sidebar.group_by = GroupBy::None;

        move_in_app_order(&mut state, remote, Some(AgentId::new(1)));
        let moved = ids(&app_order(&state));
        let mut newer = placed(3, AgentStatus::Idle, 512);
        newer.id = AgentId::new(3);
        state.agents.insert(newer.id, AgentEntry::new(newer));
        let with_newer = ids(&app_order(&state));

        assert_eq!(moved, [remote, AgentId::new(1), AgentId::new(2)]);
        assert_eq!(
            with_newer,
            [AgentId::new(3), remote, AgentId::new(1), AgentId::new(2)]
        );
    }

    #[test]
    fn an_agent_of_a_machine_not_reached_yet_keeps_its_place_in_the_order() {
        let mut state = two_machines();
        let remote = devbox().app_agent(AgentId::new(1));
        let key = |state: &AppState, agent| agent_key(state, agent).unwrap();
        state.prefs.agent_order = vec![
            key(&state, AgentId::new(1)),
            key(&state, remote),
            key(&state, AgentId::new(2)),
        ];
        let remote_key = key(&state, remote);
        state.agents.remove(&remote);

        move_in_app_order(&mut state, AgentId::new(2), Some(AgentId::new(1)));

        assert_eq!(
            state.prefs.agent_order,
            [
                key(&state, AgentId::new(2)),
                remote_key,
                key(&state, AgentId::new(1)),
            ]
        );
    }

    #[test]
    fn a_removed_agent_leaves_the_order() {
        let mut state = two_machines();
        move_in_app_order(&mut state, AgentId::new(2), Some(AgentId::new(1)));

        forget(&mut state, AgentId::new(2));

        assert_eq!(state.prefs.agent_order.len(), 2);
    }

    #[test]
    fn a_row_names_its_machine_only_when_no_panel_does() {
        let mut state = two_machines();
        let remote = devbox().app_agent(AgentId::new(1));

        let by_machine = machine_shown_on_row(&state, remote).map(str::to_owned);
        set_group_by(&mut state, GroupBy::None);

        assert_eq!(by_machine, None);
        assert_eq!(machine_shown_on_row(&state, remote), Some("devbox"));
        assert_eq!(machine_shown_on_row(&state, AgentId::new(1)), None);
    }
}

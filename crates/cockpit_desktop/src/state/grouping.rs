//! How the sidebar groups and orders the agents. Grouped by machine, each machine has a panel and
//! its daemon keeps the order of its agents. Grouped by project or not at all, the app keeps one
//! order across every machine in the preferences (`Prefs::agent_order`), and moves only change
//! it there.

use std::collections::HashMap;

use cockpit_protocol::AgentId;

use super::app_state::{AgentEntry, AppState};
use super::project_menu;
use super::projects::{self, ProjectKey};
use super::ui_state::Popover;
use crate::machine::MachineId;
use crate::prefs::{AgentKey, GroupBy, ProjectSplit};

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
        GroupBy::Machine => Some(GroupKey::Machine(MachineId::of(agent))),
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
    project_menu::prune(state);
}

pub fn set_project_split(state: &mut AppState, split: ProjectSplit) {
    if state.ui.popover == Some(Popover::GroupBy) {
        state.ui.popover = None;
    }
    state.prefs.sidebar.project_split = split;
}

/// Whether a project's panel shows a line per machine above that machine's agents: grouped by
/// project and split by machine, with several machines. A project on one machine has no lines.
pub fn splits_by_machine(state: &AppState) -> bool {
    state.prefs.sidebar.group_by == GroupBy::Project
        && state.prefs.sidebar.project_split == ProjectSplit::ByMachine
        && !state.machines.others.is_empty()
}

/// The machine all the project's agents run on. `None` when they run on several, or when the
/// project has none.
pub fn project_machine(state: &AppState, project: &ProjectKey) -> Option<MachineId> {
    let mut machines = state
        .agents
        .values()
        .filter(|entry| projects::project_of(state, entry) == *project)
        .map(|entry| MachineId::of(entry.id()));
    let first = machines.next()?;
    machines.all(|machine| machine == first).then_some(first)
}

/// The machine a project's header names: the one all its agents run on, with several machines,
/// when it is not this computer. Its rows then name none.
pub fn machine_on_project_head(state: &AppState, project: &ProjectKey) -> Option<MachineId> {
    if state.machines.others.is_empty() {
        return None;
    }
    project_machine(state, project).filter(|&machine| machine != MachineId::THIS)
}

/// The machine a row names, when nothing above it says it: with several machines, for an agent
/// that does not run on this computer, in one list or in a project of several machines that has
/// no line per machine. `None` otherwise.
pub fn machine_shown_on_row(state: &AppState, agent: AgentId) -> Option<MachineId> {
    let machine = MachineId::of(agent);
    if state.machines.others.is_empty() || machine == MachineId::THIS {
        return None;
    }
    let shown = match state.prefs.sidebar.group_by {
        GroupBy::Machine => false,
        GroupBy::None => true,
        GroupBy::Project => {
            !splits_by_machine(state)
                && state.agents.get(&agent).is_some_and(|entry| {
                    project_machine(state, &projects::project_of(state, entry)).is_none()
                })
        }
    };
    shown.then_some(machine)
}

/// A machine's tag is at least this many letters of its name.
const TAG_MIN_CHARS: usize = 3;

/// The machine's short tag on a row: the start of its name, as many letters as tell it apart
/// from the other machines' names, and at least [`TAG_MIN_CHARS`]. With machines "devbox" and
/// "devops", the tags are "devb" and "devo". `None` for a machine the app does not know.
pub fn machine_tag(state: &AppState, machine: MachineId) -> Option<String> {
    let name = state.machines.get(machine)?.daemon.machine.as_str();
    let others: Vec<&str> = state
        .machines
        .others
        .iter()
        .filter(|&(&other, _)| other != machine)
        .map(|(_, other)| other.daemon.machine.as_str())
        .collect();
    let tag = (TAG_MIN_CHARS..name.chars().count())
        .map(|length| name.chars().take(length).collect::<String>())
        .find(|tag| others.iter().all(|other| !other.starts_with(tag.as_str())))
        .unwrap_or_else(|| name.to_owned());
    Some(tag)
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

        let by_machine = machine_shown_on_row(&state, remote);
        set_group_by(&mut state, GroupBy::None);

        assert_eq!(by_machine, None);
        assert_eq!(machine_shown_on_row(&state, remote), Some(devbox()));
        assert_eq!(machine_shown_on_row(&state, AgentId::new(1)), None);
    }

    /// The three agents have no project: they share the "No project" panel, on two machines.
    #[test]
    fn a_project_of_several_machines_names_them_on_its_rows_or_on_its_lines() {
        let mut state = two_machines();
        let remote = devbox().app_agent(AgentId::new(1));
        set_group_by(&mut state, GroupBy::Project);

        let one_list = machine_shown_on_row(&state, remote);
        let head = machine_on_project_head(&state, &ProjectKey::None);
        set_project_split(&mut state, ProjectSplit::ByMachine);

        assert_eq!(one_list, Some(devbox()));
        assert_eq!(head, None);
        assert!(splits_by_machine(&state));
        assert_eq!(machine_shown_on_row(&state, remote), None);
    }

    #[test]
    fn split_by_machine_the_rows_follow_their_machines_and_keep_their_order_on_each() {
        let mut state = two_machines();
        let remote = devbox().app_agent(AgentId::new(1));
        set_group_by(&mut state, GroupBy::Project);
        move_in_app_order(&mut state, remote, Some(AgentId::new(1)));
        move_in_app_order(&mut state, AgentId::new(2), Some(AgentId::new(1)));

        let one_list = ids(&state.sidebar_agents());
        set_project_split(&mut state, ProjectSplit::ByMachine);

        assert_eq!(one_list, [remote, AgentId::new(2), AgentId::new(1)]);
        assert_eq!(
            ids(&state.sidebar_agents()),
            [AgentId::new(2), AgentId::new(1), remote]
        );
    }

    #[test]
    fn a_project_on_one_other_machine_names_it_on_its_header_alone() {
        let mut state = two_machines();
        let remote = devbox().app_agent(AgentId::new(1));
        state.agents.retain(|&agent, _| agent == remote);
        set_group_by(&mut state, GroupBy::Project);

        assert_eq!(project_machine(&state, &ProjectKey::None), Some(devbox()));
        assert_eq!(
            machine_on_project_head(&state, &ProjectKey::None),
            Some(devbox())
        );
        assert_eq!(machine_shown_on_row(&state, remote), None);
    }

    #[test]
    fn a_project_on_this_computer_alone_names_no_machine() {
        let mut state = two_machines();
        state
            .agents
            .retain(|&agent, _| MachineId::of(agent) == MachineId::THIS);
        set_group_by(&mut state, GroupBy::Project);

        assert_eq!(
            project_machine(&state, &ProjectKey::None),
            Some(MachineId::THIS)
        );
        assert_eq!(machine_on_project_head(&state, &ProjectKey::None), None);
    }

    #[test]
    fn a_tag_takes_as_many_letters_as_tell_the_machines_apart() {
        let mut state = two_machines();
        let named = |name: &str| MachineState::new(DaemonLabel::ssh(name.to_owned()));
        state
            .machines
            .others
            .insert(MachineId::new(2), named("devops"));
        state
            .machines
            .others
            .insert(MachineId::new(3), named("gpu"));
        state.machines.others.insert(MachineId::new(4), named("ci"));

        let tags: Vec<Option<String>> = (1..=4)
            .map(|machine| machine_tag(&state, MachineId::new(machine)))
            .collect();

        assert_eq!(
            tags,
            [
                Some("devb".to_owned()),
                Some("devo".to_owned()),
                Some("gpu".to_owned()),
                Some("ci".to_owned()),
            ]
        );
        assert_eq!(machine_tag(&state, MachineId::new(9)), None);
    }
}

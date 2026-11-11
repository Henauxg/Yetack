//! The arrow keys on the sidebar's rows and headers, a machine's or a project's. Up and Down
//! select the agent above or below among the rows on screen, and the focus follows. Left folds
//! a header's agents and moves the focus to the header, Right shows them again.

use cockpit_protocol::AgentId;
use serde::Deserialize;

use super::actions;
use super::app_state::AppState;
use super::grouping::{self, GroupKey};
use super::projects::ProjectKey;
use super::shortcuts::{KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_UP};
use crate::machine::MachineId;

/// The keys the page's script passes on from a sidebar row or header.
pub const SIDEBAR_KEYS: [&str; 4] = [KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT];

/// The sidebar element that has the focus.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum SidebarItem {
    Agent(AgentId),
    /// A machine's header.
    Machine(MachineId),
    /// A project's header.
    Project(ProjectKey),
}

impl SidebarItem {
    fn of_group(group: GroupKey) -> Self {
        match group {
            GroupKey::Machine(machine) => Self::Machine(machine),
            GroupKey::Project(project) => Self::Project(project),
        }
    }
}

/// Acts on `key` pressed on `focused`, and returns the element that takes the focus next.
pub fn press(state: &mut AppState, focused: &SidebarItem, key: &str) -> Option<SidebarItem> {
    match key {
        KEY_UP => select_next(state, focused, Step::Up),
        KEY_DOWN => select_next(state, focused, Step::Down),
        KEY_LEFT => set_folded(state, focused, true),
        KEY_RIGHT => set_folded(state, focused, false),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Up,
    Down,
}

/// The sidebar from top to bottom: each header, then its rows unless it is folded. Without
/// headers, the rows alone.
fn items(state: &AppState) -> Vec<SidebarItem> {
    let groups = state.sidebar_groups();
    if groups.is_empty() {
        return state
            .sidebar_agents()
            .iter()
            .map(|entry| SidebarItem::Agent(entry.id()))
            .collect();
    }
    let mut items = Vec::new();
    for group in groups {
        let folded = grouping::is_folded(state, &group.key);
        items.push(SidebarItem::of_group(group.key));
        if !folded {
            items.extend(
                group
                    .rows
                    .iter()
                    .map(|entry| SidebarItem::Agent(entry.id())),
            );
        }
    }
    items
}

/// Selects the first agent row above or below `focused`, passing over the headers.
fn select_next(state: &mut AppState, focused: &SidebarItem, step: Step) -> Option<SidebarItem> {
    let items = items(state);
    let index = items.iter().position(|item| item == focused)?;
    let agent_at = |item: &SidebarItem| match item {
        SidebarItem::Agent(agent) => Some(*agent),
        SidebarItem::Machine(_) | SidebarItem::Project(_) => None,
    };
    let next = match step {
        Step::Up => items.get(..index)?.iter().rev().find_map(agent_at),
        Step::Down => items.get(index + 1..)?.iter().find_map(agent_at),
    }?;
    actions::select_agent(state, next);
    Some(SidebarItem::Agent(next))
}

/// Folds or shows the header of `focused`. Folding from a row moves the focus to the header,
/// as the row goes away.
fn set_folded(state: &mut AppState, focused: &SidebarItem, fold: bool) -> Option<SidebarItem> {
    let group = match focused {
        SidebarItem::Agent(agent) => grouping::group_of(state, *agent)?,
        SidebarItem::Machine(machine) => GroupKey::Machine(*machine),
        SidebarItem::Project(project) => GroupKey::Project(project.clone()),
    };
    grouping::set_folded(state, &group, fold);
    (fold && matches!(focused, SidebarItem::Agent(_))).then(|| SidebarItem::of_group(group))
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::app_state::tests::{placed, state_with};
    use crate::state::{AgentEntry, MachineState};

    fn devbox() -> MachineId {
        MachineId::new(1)
    }

    /// This computer runs agents 1 and 2, the devbox runs its agents 1 and 2, each machine's
    /// agent 1 on top: a higher position is higher in the list.
    fn two_machines() -> AppState {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 2048),
            placed(2, AgentStatus::Idle, 1024),
        ]);
        state.machines.others.insert(
            devbox(),
            MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
        );
        for (id, position) in [(1, 2048), (2, 1024)] {
            let mut remote = placed(id, AgentStatus::Idle, position);
            remote.id = remote_agent(id);
            state.agents.insert(remote.id, AgentEntry::new(remote));
        }
        state
    }

    fn remote_agent(id: u32) -> AgentId {
        devbox().app_agent(AgentId::new(id))
    }

    fn agent(id: u32) -> SidebarItem {
        SidebarItem::Agent(AgentId::new(id))
    }

    #[test]
    fn up_and_down_select_the_next_row_across_machines() {
        let mut state = two_machines();
        state.ui.selected = Some(AgentId::new(2));

        let down = press(&mut state, &agent(2), KEY_DOWN);
        let selected_down = state.ui.selected;
        let up = press(&mut state, &SidebarItem::Agent(remote_agent(1)), KEY_UP);

        assert_eq!(down, Some(SidebarItem::Agent(remote_agent(1))));
        assert_eq!(selected_down, Some(remote_agent(1)));
        assert_eq!(up, Some(agent(2)));
        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
    }

    #[test]
    fn the_first_and_last_rows_stay_put() {
        let mut state = two_machines();
        state.ui.selected = Some(AgentId::new(1));

        let above_first = press(&mut state, &agent(1), KEY_UP);
        let below_last = press(&mut state, &SidebarItem::Agent(remote_agent(2)), KEY_DOWN);

        assert_eq!(above_first, None);
        assert_eq!(below_last, None);
        assert_eq!(state.ui.selected, Some(AgentId::new(1)));
    }

    #[test]
    fn left_folds_the_rows_machine_and_moves_the_focus_to_its_header() {
        let mut state = two_machines();

        let focus = press(&mut state, &SidebarItem::Agent(remote_agent(1)), KEY_LEFT);
        let up_from_header = press(&mut state, &SidebarItem::Machine(devbox()), KEY_UP);
        let down_from_this = press(&mut state, &agent(2), KEY_DOWN);

        assert_eq!(focus, Some(SidebarItem::Machine(devbox())));
        assert!(state.ui.folded_machines.contains(&devbox()));
        assert_eq!(up_from_header, Some(agent(2)));
        // The folded machine's rows are off screen: nothing is below.
        assert_eq!(down_from_this, None);
    }

    #[test]
    fn right_shows_this_computers_rows_and_the_focus_stays() {
        let mut state = two_machines();
        state.ui.folded_machines.insert(MachineId::THIS);

        let on_header = press(
            &mut state,
            &SidebarItem::Machine(MachineId::THIS),
            KEY_RIGHT,
        );
        let on_row = press(&mut state, &agent(1), KEY_RIGHT);
        let down_from_header = press(&mut state, &SidebarItem::Machine(MachineId::THIS), KEY_DOWN);

        assert_eq!(on_header, None);
        assert_eq!(on_row, None);
        assert!(state.ui.folded_machines.is_empty());
        assert_eq!(down_from_header, Some(agent(1)));
    }

    #[test]
    fn with_this_computer_alone_left_folds_nothing() {
        let mut state = state_with(vec![placed(1, AgentStatus::Idle, 1024)]);

        let focus = press(&mut state, &agent(1), KEY_LEFT);

        assert_eq!(focus, None);
        assert!(state.ui.folded_machines.is_empty());
    }
}

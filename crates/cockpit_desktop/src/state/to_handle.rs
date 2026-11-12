//! The "to handle" mark: a reminder you put on an agent, from its row's menu or its shortcut
//! (Ctrl+H by default).
//! The daemon keeps it. A mark of the kind "until a message" goes when you send the agent a
//! prompt, a kept one only when you clear it. The shortcut gives the kind the settings put first,
//! then the other kind, then clears it.

use cockpit_protocol::{AgentId, Mark, MarkParams, Request};

use super::app_state::AppState;
use super::requests::Outgoing;
use super::ui_state::Popover;
use crate::prefs::FirstMark;

/// Marks the agent, or clears its mark. The row changes once the daemon's update comes back.
pub fn set(state: &mut AppState, agent: AgentId, mark: Mark) -> Vec<Outgoing> {
    close_menu(state);
    if !state.agents.contains_key(&agent) {
        return Vec::new();
    }
    vec![Outgoing::action(Request::AgentMark(MarkParams {
        agent,
        mark,
    }))]
}

/// The mark after `mark`, as the shortcut and the row's menu go: the first kind, the other kind,
/// then none.
pub fn next(mark: Mark, first: FirstMark) -> Mark {
    let [first, second] = first.order();
    match mark {
        Mark::Unmarked => first,
        mark if mark == first => second,
        Mark::ToHandle | Mark::ToHandleKept => Mark::Unmarked,
    }
}

/// The mark's shortcut: gives the selected agent its next mark.
pub fn cycle_selected(state: &mut AppState) -> Vec<Outgoing> {
    let Some(entry) = state.selected() else {
        return Vec::new();
    };
    let (agent, mark) = (entry.id(), entry.summary.mark);
    let next = next(mark, state.prefs.sidebar.first_mark);
    set(state, agent, next)
}

/// A right-click on the agent's row opens its menu at the pointer, in the window's pixels.
pub fn open_menu(state: &mut AppState, agent: AgentId, x: i32, y: i32) {
    if state.agents.contains_key(&agent) {
        state.ui.popover = Some(Popover::RowMenu { agent, x, y });
    }
}

fn close_menu(state: &mut AppState) {
    if matches!(state.ui.popover, Some(Popover::RowMenu { .. })) {
        state.ui.popover = None;
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};

    fn marks(requests: &[Outgoing]) -> Vec<MarkParams> {
        requests
            .iter()
            .filter_map(|outgoing| match &outgoing.request {
                Request::AgentMark(params) => Some(*params),
                _ => None,
            })
            .collect()
    }

    /// Each press asks for the next mark, which the daemon's update then puts on the row.
    fn presses(first: FirstMark) -> Vec<Mark> {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(AgentId::new(1));
        state.prefs.sidebar.first_mark = first;
        let mut asked = Vec::new();
        for _ in 0..3 {
            let mark = marks(&cycle_selected(&mut state))
                .first()
                .map_or(Mark::Unmarked, |params| params.mark);
            if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
                entry.summary.mark = mark;
            }
            asked.push(mark);
        }
        asked
    }

    #[test]
    fn the_shortcut_gives_the_first_kind_then_the_other_then_clears_it() {
        assert_eq!(
            presses(FirstMark::Kept),
            [Mark::ToHandleKept, Mark::ToHandle, Mark::Unmarked]
        );
        assert_eq!(
            presses(FirstMark::UntilMessage),
            [Mark::ToHandle, Mark::ToHandleKept, Mark::Unmarked]
        );
    }

    #[test]
    fn the_menu_opens_on_a_known_agent_and_closes_on_a_pick() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        open_menu(&mut state, AgentId::new(9), 10, 20);
        let unknown = state.ui.popover;
        open_menu(&mut state, AgentId::new(1), 10, 20);
        let opened = state.ui.popover;
        let picked = set(&mut state, AgentId::new(1), Mark::ToHandle);

        assert_eq!(unknown, None);
        assert_eq!(
            opened,
            Some(Popover::RowMenu {
                agent: AgentId::new(1),
                x: 10,
                y: 20
            })
        );
        assert_eq!(state.ui.popover, None);
        assert_eq!(marks(&picked).len(), 1);
    }

    #[test]
    fn nothing_is_marked_without_a_selection() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        assert!(cycle_selected(&mut state).is_empty());
    }
}

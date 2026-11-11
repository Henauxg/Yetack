//! "Mark as unread" from a sidebar row's menu: the row shows its unread dot and bold title again,
//! as when the agent finishes out of sight, until you select the agent. The mark lives in the
//! window only, as the one a finish sets.

use cockpit_protocol::AgentId;

use super::app_state::AppState;
use super::ui_state::Popover;

pub fn mark_unread(state: &mut AppState, agent: AgentId) {
    close_menu(state);
    if state.agents.contains_key(&agent) {
        state.ui.unread.insert(agent);
    }
}

pub fn mark_read(state: &mut AppState, agent: AgentId) {
    close_menu(state);
    state.ui.unread.remove(&agent);
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
    use crate::state::{actions, state_with, summary};

    #[test]
    fn an_agent_marked_unread_stays_so_until_it_is_selected() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        actions::select_agent(&mut state, AgentId::new(1));
        state.ui.popover = Some(Popover::RowMenu {
            agent: AgentId::new(1),
            x: 10,
            y: 20,
        });

        mark_unread(&mut state, AgentId::new(1));
        let marked = state.is_unread(AgentId::new(1));
        state.fix_selection();
        let after_an_update = state.is_unread(AgentId::new(1));
        actions::select_agent(&mut state, AgentId::new(2));
        actions::select_agent(&mut state, AgentId::new(1));

        assert!(marked);
        assert!(after_an_update);
        assert!(!state.is_unread(AgentId::new(1)));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn each_mark_does_what_it_says_whatever_the_agent_was() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let agent = AgentId::new(1);

        mark_unread(&mut state, agent);
        mark_unread(&mut state, agent);
        let unread = state.is_unread(agent);
        mark_read(&mut state, agent);
        mark_read(&mut state, agent);

        assert!(unread);
        assert!(!state.is_unread(agent));
    }

    #[test]
    fn an_unknown_agent_is_not_marked() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        mark_unread(&mut state, AgentId::new(9));

        assert!(state.ui.unread.is_empty());
    }
}

//! Where each agent shows in the agent list: an order the user sets, which the daemon keeps.
//!
//! The list shows the highest position first. A move gives the moved agent a position between
//! those of its new neighbours, so one agent changes. Only when they leave no room does every agent
//! get a new position, `GAP` apart. For example, with C at 3072, B at 2048 and A at 1024, moving
//! A just above B gives A 2560, and nothing else changes.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::agent::AgentSummary;
use crate::ids::AgentId;

/// The room between two agents after a renumbering, and above the top agent for a new one. Ten
/// moves fit between the same two neighbours before the next renumbering.
const GAP: u32 = 1024;

/// An agent's place in the list: a higher position shows higher.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Position(u32);

impl Position {
    #[must_use]
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

/// An agent and its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Placed {
    pub agent: AgentId,
    pub position: Position,
}

impl AgentSummary {
    #[must_use]
    pub fn place(&self) -> Placed {
        Placed {
            agent: self.id,
            position: self.position,
        }
    }
}

/// The list's order: the higher position first, then the older agent. Agents saved before
/// positions existed all have position 0, so they keep the order of their ids.
#[must_use]
pub fn list_order(left: Placed, right: Placed) -> Ordering {
    right
        .position
        .cmp(&left.position)
        .then(left.agent.cmp(&right.agent))
}

/// The places that change when `agent` goes just above `before`, or to the bottom when `before`
/// is `None`. `list` holds every agent, in the list's order.
///
/// Gives the moved agent alone when its new neighbours leave room between their positions, and
/// every agent whose position changes otherwise. Gives nothing when the agent is already there,
/// when `before` is `agent`, or when either is not in `list`.
#[must_use]
pub fn move_agent(list: &[Placed], agent: AgentId, before: Option<AgentId>) -> Vec<Placed> {
    let Some(from) = list.iter().position(|place| place.agent == agent) else {
        return Vec::new();
    };
    let mut others: Vec<Placed> = list
        .iter()
        .copied()
        .filter(|place| place.agent != agent)
        .collect();
    let to = match before {
        Some(before) => match others.iter().position(|place| place.agent == before) {
            Some(index) => index,
            None => return Vec::new(),
        },
        None => others.len(),
    };
    if to == from {
        return Vec::new();
    }
    let above = to
        .checked_sub(1)
        .and_then(|index| others.get(index))
        .map(|place| place.position);
    let below = others.get(to).map(|place| place.position);
    if let Some(position) = between(above, below) {
        return vec![Placed { agent, position }];
    }
    others.insert(
        to,
        Placed {
            agent,
            position: Position::default(),
        },
    );
    renumbered(&others, list)
}

/// The places for a new agent at the top of `list`, which does not hold it yet: the new agent's,
/// and those of the agents that move to leave it room. With `list` empty, the new agent keeps
/// position 0 and nothing is given.
#[must_use]
pub fn place_on_top(list: &[Placed], agent: AgentId) -> Vec<Placed> {
    let mut with_new = list.to_vec();
    with_new.push(Placed {
        agent,
        position: Position::default(),
    });
    let top = list.first().map(|place| place.agent);
    move_agent(&with_new, agent, top)
}

/// A position strictly between `above` and `below`, where `None` is the end of the list.
fn between(above: Option<Position>, below: Option<Position>) -> Option<Position> {
    match (above, below) {
        (None, None) => Some(Position(GAP)),
        (None, Some(below)) => below.0.checked_add(GAP).map(Position),
        (Some(above), None) => (above.0 > 0).then_some(Position(above.0 / 2)),
        (Some(above), Some(below)) => {
            let room = above.0.checked_sub(below.0)?;
            (room >= 2).then_some(Position(below.0 + room / 2))
        }
    }
}

/// Numbers `order` from the top, `GAP` apart, and gives the places that differ from `before`.
fn renumbered(order: &[Placed], before: &[Placed]) -> Vec<Placed> {
    let mut changed = Vec::new();
    let mut steps = order.len();
    for place in order {
        let step = u32::try_from(steps).unwrap_or(u32::MAX);
        let position = Position(step.saturating_mul(GAP));
        steps = steps.saturating_sub(1);
        let unchanged = before
            .iter()
            .any(|old| old.agent == place.agent && old.position == position);
        if !unchanged {
            changed.push(Placed {
                agent: place.agent,
                position,
            });
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(agent: u32, position: u32) -> Placed {
        Placed {
            agent: AgentId::new(agent),
            position: Position(position),
        }
    }

    fn agent(id: u32) -> AgentId {
        AgentId::new(id)
    }

    /// The agents of `list` in the list's order once `changes` apply.
    fn order_after(list: &[Placed], changes: &[Placed]) -> Vec<u32> {
        let mut places: Vec<Placed> = list
            .iter()
            .map(|place| {
                changes
                    .iter()
                    .find(|change| change.agent == place.agent)
                    .copied()
                    .unwrap_or(*place)
            })
            .collect();
        places.sort_by(|left, right| list_order(*left, *right));
        places.iter().map(|place| place.agent.get()).collect()
    }

    #[test]
    fn the_higher_position_comes_first_then_the_older_agent() {
        let mut places = [placed(1, 0), placed(2, 2048), placed(3, 0), placed(4, 1024)];

        places.sort_by(|left, right| list_order(*left, *right));

        let agents: Vec<u32> = places.iter().map(|place| place.agent.get()).collect();
        assert_eq!(agents, [2, 4, 1, 3]);
    }

    #[test]
    fn a_move_with_room_changes_the_moved_agent_alone() {
        let list = [placed(3, 3072), placed(2, 2048), placed(1, 1024)];

        let between = move_agent(&list, agent(1), Some(agent(2)));
        let to_top = move_agent(&list, agent(1), Some(agent(3)));
        let to_bottom = move_agent(&list, agent(3), None);

        assert_eq!(between, [placed(1, 2560)]);
        assert_eq!(to_top, [placed(1, 3072 + GAP)]);
        assert_eq!(to_bottom, [placed(3, 512)]);
        assert_eq!(order_after(&list, &to_bottom), [2, 1, 3]);
    }

    #[test]
    fn a_move_without_room_renumbers_every_agent_that_changes() {
        let list = [placed(3, 2049), placed(2, 2048), placed(1, 1024)];

        let changes = move_agent(&list, agent(1), Some(agent(2)));

        assert_eq!(changes, [placed(3, 3072), placed(1, 2048), placed(2, 1024)]);
        assert_eq!(order_after(&list, &changes), [3, 1, 2]);
    }

    #[test]
    fn a_move_to_the_bottom_of_position_zero_renumbers() {
        let list = [placed(2, 1024), placed(1, 0)];

        let changes = move_agent(&list, agent(2), None);

        // Agent 2 keeps 1024 after the renumbering: only agent 1 changes.
        assert_eq!(changes, [placed(1, 2048)]);
        assert_eq!(order_after(&list, &changes), [1, 2]);
    }

    #[test]
    fn a_move_that_changes_nothing_gives_nothing() {
        let list = [placed(3, 3072), placed(2, 2048), placed(1, 1024)];

        assert_eq!(move_agent(&list, agent(2), Some(agent(1))), []);
        assert_eq!(move_agent(&list, agent(1), None), []);
        assert_eq!(move_agent(&list, agent(2), Some(agent(2))), []);
        assert_eq!(move_agent(&list, agent(9), None), []);
        assert_eq!(move_agent(&list, agent(2), Some(agent(9))), []);
    }

    /// An `agents.json` saved before positions existed gives every agent position 0.
    #[test]
    fn agents_saved_without_positions_keep_their_order_until_one_moves() {
        let list = [placed(1, 0), placed(2, 0), placed(3, 0)];

        let changes = move_agent(&list, agent(3), Some(agent(2)));

        assert_eq!(changes, [placed(1, 3072), placed(3, 2048), placed(2, 1024)]);
        assert_eq!(order_after(&list, &changes), [1, 3, 2]);
    }

    #[test]
    fn a_new_agent_goes_on_top() {
        let list = [placed(2, 2048), placed(1, 1024)];

        assert_eq!(place_on_top(&[], agent(1)), []);
        assert_eq!(place_on_top(&[placed(1, 0)], agent(2)), [placed(2, GAP)]);
        assert_eq!(place_on_top(&list, agent(3)), [placed(3, 2048 + GAP)]);
    }

    #[test]
    fn a_new_agent_renumbers_when_the_top_position_is_taken() {
        let list = [placed(2, u32::MAX), placed(1, 1024)];

        let changes = place_on_top(&list, agent(3));

        assert_eq!(changes, [placed(3, 3072), placed(2, 2048)]);
    }
}

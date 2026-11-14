//! How the details events and the `agent.details` answer reach an agent's details.
//!
//! An update changes the details as the agent's last details event left them, so the app
//! applies it only to those. The answer can reach the app after events the daemon sent after it,
//! or before events it already holds: its `last_seq` tells them apart.

use cockpit_protocol::{AgentDetails, DetailsChange, Seq};

use super::app_state::{AgentEntry, Loadable};
use super::sending;

/// A details update that came while the details load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarlyUpdate {
    seq: Seq,
    changes: Vec<DetailsChange>,
}

/// Replaces the details with the whole ones of the event `seq`.
pub fn replace(entry: &mut AgentEntry, seq: Seq, details: AgentDetails) {
    entry.details = Loadable::Loaded(details);
    entry.details_seq = Some(seq);
    entry.early_details.clear();
}

/// Applies the update `seq`, unless the details already hold it. While they load, the update
/// waits for the answer. Before they are asked, it goes: the answer will hold it.
pub fn update(entry: &mut AgentEntry, seq: Seq, changes: Vec<DetailsChange>) {
    match &mut entry.details {
        Loadable::Loaded(details) => {
            if entry.details_seq.is_some_and(|held| seq <= held) {
                return;
            }
            for change in changes {
                details.apply(change);
            }
            entry.details_seq = Some(seq);
        }
        Loadable::Loading => entry.early_details.push(EarlyUpdate { seq, changes }),
        Loadable::NotRequested | Loadable::Failed(_) => {}
    }
}

/// Takes the details `agent.details` answered, which hold the events up to `last_seq`, then the
/// updates that came while they loaded. Details that an event replaced meanwhile are newer, and
/// stay.
pub fn answered(entry: &mut AgentEntry, details: AgentDetails, last_seq: Option<Seq>) {
    let early = std::mem::take(&mut entry.early_details);
    // The app drops updates while it does not wait for an answer, as after a snapshot.
    if entry.details == Loadable::Loading {
        entry.details = Loadable::Loaded(details);
        entry.details_seq = last_seq;
    }
    // A message sent while the details loaded knew the answer's waiting messages, not those of
    // the updates after it.
    sending::details_loaded(entry);
    for EarlyUpdate { seq, changes } in early {
        update(entry, seq, changes);
    }
}

/// The answer failed: `failure` takes the place of the details, unless an event replaced them
/// meanwhile, and the updates that waited for it go.
pub fn failed(entry: &mut AgentEntry, failure: Loadable<AgentDetails>) {
    entry.early_details.clear();
    if !matches!(entry.details, Loadable::Loaded(_)) {
        entry.details = failure;
    }
}

/// Forgets the details, which missed events: the app asks for them again.
pub fn forget(entry: &mut AgentEntry) {
    entry.details = Loadable::NotRequested;
    entry.details_seq = None;
    entry.early_details.clear();
}

/// The agent's loaded details, unless a details event reached them since a request left with
/// `asked`, their seq then. The daemon also sends what it answered in a details event, and one
/// applied since may be newer than the answer.
pub fn unchanged_since(entry: &mut AgentEntry, asked: Option<Seq>) -> Option<&mut AgentDetails> {
    if entry.details_seq != asked {
        return None;
    }
    entry.details.loaded_mut()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use cockpit_client::{RequestError, SessionUpdate};
    use cockpit_protocol::{
        AgentId, AgentParams, AgentStatus, BackgroundTaskId, Catchup, ContextUsage, Event,
        EventRecord, ItemId, McpServer, McpServerStatus, Method, PlanUsage, Request, ResponseBody,
        Settings, Snapshot, Subagent, SubagentRun, SubagentStatus, SubagentToolCall, TaskListItem,
        TaskStatus, Timestamp,
    };

    use super::*;
    use crate::machine::MachineId;
    use crate::state::app_state::tests::{connected_update, state_with, summary};
    use crate::state::ui_state::Block;
    use crate::state::{
        AppState, Drafts, Outgoing, Purpose, actions, apply_response, apply_update, needed_requests,
    };

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn subagent(item: u32, tool_calls: u32) -> Subagent {
        Subagent {
            item: ItemId::new(item),
            parent: None,
            description: format!("Check the cart, part {item}"),
            agent_type: Some("Explore".to_owned()),
            run: SubagentRun::Background,
            status: SubagentStatus::Running,
            started_at: Timestamp::from_unix_millis(1_000),
            ended_at: None,
            tokens: None,
            tool_calls,
            last_tool: Some(SubagentToolCall {
                item: Some(ItemId::new(item + tool_calls)),
                tool: "Read".to_owned(),
                summary: "src/cart.rs".to_owned(),
            }),
            error: None,
            turn: 1,
            task: Some(BackgroundTaskId::new(format!("task-{item}"))),
        }
    }

    /// Details that go through each kind of change: a subagent's call, a new subagent and one
    /// that leaves, another part, a subagent started again, a new one after it, and none left.
    fn steps() -> Vec<AgentDetails> {
        let mut details = AgentDetails {
            subagents: vec![subagent(10, 1), subagent(20, 1), subagent(30, 1)],
            ..AgentDetails::default()
        };
        let mut steps = vec![details.clone()];
        details.subagents[1].tool_calls = 2;
        steps.push(details.clone());
        details.subagents.remove(0);
        details.subagents.push(subagent(40, 0));
        steps.push(details.clone());
        details.tasks = vec![TaskListItem {
            content: "Fix the total".to_owned(),
            status: TaskStatus::InProgress,
            active_form: "Fixing the total".to_owned(),
        }];
        steps.push(details.clone());
        details.subagents.remove(0);
        details.subagents.push(subagent(20, 0));
        steps.push(details.clone());
        details.subagents.push(subagent(50, 0));
        steps.push(details);
        steps.push(AgentDetails::default());
        steps
    }

    fn working() -> AppState {
        state_with(vec![summary(1, AgentStatus::Working)])
    }

    fn loading() -> AppState {
        let mut state = working();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.details = Loadable::Loading;
        }
        state
    }

    fn event(state: &mut AppState, seq: u64, event: Event) {
        let record = EventRecord {
            seq: Seq::new(seq),
            event,
        };
        apply_update(
            state,
            &mut Drafts::default(),
            SessionUpdate::Event(record),
            Instant::now(),
            MachineId::THIS,
        );
    }

    fn whole(state: &mut AppState, seq: u64, details: AgentDetails) {
        let agent = agent();
        event(state, seq, Event::DetailsChanged { agent, details });
    }

    /// The update the daemon sends from `older` to `newer`.
    fn update_to(state: &mut AppState, seq: u64, older: &AgentDetails, newer: &AgentDetails) {
        let changes = newer.changes_since(older);
        let agent = agent();
        event(state, seq, Event::DetailsUpdated { agent, changes });
    }

    fn answer(state: &mut AppState, result: Result<ResponseBody, RequestError>) {
        apply_response(
            state,
            &mut Drafts::default(),
            Method::AgentDetails,
            Purpose::Details { agent: agent() },
            result,
            MachineId::THIS,
        );
    }

    fn answer_details(state: &mut AppState, details: AgentDetails, last_seq: u64) {
        let last_seq = Some(Seq::new(last_seq));
        answer(state, Ok(ResponseBody::Details { details, last_seq }));
    }

    fn details(state: &AppState) -> &Loadable<AgentDetails> {
        &state.agents[&agent()].details
    }

    fn loaded(details: &AgentDetails) -> Loadable<AgentDetails> {
        Loadable::Loaded(details.clone())
    }

    #[test]
    fn updates_lead_to_the_details_that_whole_events_give() {
        let steps = steps();
        let mut state = working();

        whole(&mut state, 1, steps[0].clone());
        for (seq, pair) in (2..).zip(steps.windows(2)) {
            update_to(&mut state, seq, &pair[0], &pair[1]);

            assert_eq!(*details(&state), loaded(&pair[1]), "after the update {seq}");
        }
    }

    #[test]
    fn updates_that_come_while_the_details_load_follow_the_answer() {
        let steps = steps();
        let mut state = loading();

        update_to(&mut state, 5, &steps[0], &steps[1]);
        update_to(&mut state, 6, &steps[1], &steps[2]);
        let waiting = details(&state).clone();
        answer_details(&mut state, steps[1].clone(), 5);

        assert_eq!(waiting, Loadable::Loading);
        assert_eq!(*details(&state), loaded(&steps[2]));
    }

    /// The daemon's answer can overtake the events it sent before it: it holds them already.
    #[test]
    fn updates_the_answer_already_holds_change_nothing() {
        let steps = steps();
        let mut state = loading();

        answer_details(&mut state, steps[5].clone(), 7);
        update_to(&mut state, 6, &steps[3], &steps[4]);
        update_to(&mut state, 7, &steps[4], &steps[5]);
        let late = details(&state).clone();
        update_to(&mut state, 8, &steps[5], &steps[6]);

        assert_eq!(late, loaded(&steps[5]));
        assert_eq!(*details(&state), loaded(&steps[6]));
    }

    #[test]
    fn an_update_before_the_details_are_asked_is_dropped() {
        let steps = steps();
        let mut state = working();

        update_to(&mut state, 2, &steps[0], &steps[1]);

        assert_eq!(*details(&state), Loadable::NotRequested);
        assert!(state.agents[&agent()].early_details.is_empty());
    }

    #[test]
    fn whole_details_that_come_while_the_details_load_win_over_the_answer() {
        let steps = steps();
        let mut state = loading();

        update_to(&mut state, 3, &steps[0], &steps[1]);
        whole(&mut state, 4, steps[2].clone());
        answer_details(&mut state, steps[1].clone(), 3);
        update_to(&mut state, 5, &steps[2], &steps[3]);

        assert_eq!(*details(&state), loaded(&steps[3]));
    }

    #[test]
    fn a_failed_answer_drops_the_updates_that_waited_for_it() {
        let steps = steps();
        let mut state = loading();

        update_to(&mut state, 3, &steps[0], &steps[1]);
        answer(&mut state, Err(RequestError::Disconnected));

        assert_eq!(*details(&state), Loadable::NotRequested);
        assert!(state.agents[&agent()].early_details.is_empty());
    }

    /// A replay sends the latest whole details in place of the updates the app missed.
    #[test]
    fn a_replay_brings_the_whole_details() {
        let steps = steps();
        let mut state = working();
        whole(&mut state, 1, steps[0].clone());
        update_to(&mut state, 2, &steps[0], &steps[1]);

        apply_update(
            &mut state,
            &mut Drafts::default(),
            connected_update(Catchup::Replay),
            Instant::now(),
            MachineId::THIS,
        );
        whole(&mut state, 9, steps[3].clone());
        update_to(&mut state, 10, &steps[3], &steps[4]);

        assert_eq!(*details(&state), loaded(&steps[4]));
    }

    /// Without a replay, the app asks for the details again, and the updates after the answer
    /// apply to it.
    #[test]
    fn after_a_snapshot_the_answer_and_the_updates_after_it_give_the_details() {
        let steps = steps();
        let mut state = working();
        whole(&mut state, 1, steps[0].clone());
        let snapshot = Snapshot {
            agents: vec![summary(1, AgentStatus::Working)],
            plan_usage: PlanUsage::default(),
            settings: Settings::default(),
            last_seq: Some(Seq::new(40)),
            accounts: Vec::new(),
            other_plan_usage: Vec::new(),
        };

        apply_update(
            &mut state,
            &mut Drafts::default(),
            connected_update(Catchup::Snapshot(snapshot)),
            Instant::now(),
            MachineId::THIS,
        );
        update_to(&mut state, 41, &steps[1], &steps[2]);
        state.ui.selected = Some(agent());
        let asked = needed_requests(&mut state);
        update_to(&mut state, 42, &steps[2], &steps[3]);
        answer_details(&mut state, steps[2].clone(), 41);

        let details_asked = Request::AgentDetails(AgentParams { agent: agent() });
        assert!(
            asked
                .iter()
                .any(|outgoing| outgoing.request == details_asked),
            "the app asks for the details"
        );
        assert_eq!(*details(&state), loaded(&steps[3]));
    }

    /// A replay's whole details can be applied after an answer the app asked for once connected:
    /// the updates after them, which follow them on the connection, bring the details back.
    #[test]
    fn whole_details_older_than_the_answer_are_caught_up_by_the_updates_after_them() {
        let steps = steps();
        let mut state = loading();

        answer_details(&mut state, steps[3].clone(), 12);
        whole(&mut state, 10, steps[1].clone());
        update_to(&mut state, 11, &steps[1], &steps[2]);
        update_to(&mut state, 12, &steps[2], &steps[3]);
        update_to(&mut state, 13, &steps[3], &steps[4]);

        assert_eq!(*details(&state), loaded(&steps[4]));
    }

    /// While the close is on its way, the app has no entry for the agent and drops its events.
    #[test]
    fn an_agent_whose_close_failed_asks_for_its_details_again() {
        let steps = steps();
        let mut state = working();
        whole(&mut state, 1, steps[0].clone());

        let closing = actions::close_agent(&mut state, &mut Drafts::default(), agent());
        update_to(&mut state, 2, &steps[0], &steps[1]);
        for outgoing in closing {
            let method = outgoing.request.method();
            let refused = Err(RequestError::Disconnected);
            apply_response(
                &mut state,
                &mut Drafts::default(),
                method,
                outgoing.purpose,
                refused,
                MachineId::THIS,
            );
        }
        update_to(&mut state, 3, &steps[1], &steps[2]);

        assert_eq!(*details(&state), Loadable::NotRequested);
    }

    fn with_context(tokens: u32) -> AgentDetails {
        AgentDetails {
            context: Some(ContextUsage {
                tokens,
                max_tokens: 200_000,
                percent: 1,
                categories: Vec::new(),
            }),
            ..AgentDetails::default()
        }
    }

    fn with_server(status: McpServerStatus) -> AgentDetails {
        AgentDetails {
            mcp_servers: vec![McpServer {
                name: "notes".to_owned(),
                status,
                error: None,
                version: None,
                transport: None,
                scope: None,
                tools: Vec::new(),
            }],
            ..AgentDetails::default()
        }
    }

    /// Answers the requests with `body`.
    fn answer_all(state: &mut AppState, asked: Vec<Outgoing>, body: &ResponseBody) {
        for outgoing in asked {
            let method = outgoing.request.method();
            apply_response(
                state,
                &mut Drafts::default(),
                method,
                outgoing.purpose,
                Ok(body.clone()),
                MachineId::THIS,
            );
        }
    }

    #[test]
    fn a_context_answer_shows_at_once() {
        let mut state = working();
        whole(&mut state, 1, AgentDetails::default());
        let answered = with_context(4_000);

        let asked = actions::toggle_usage(&mut state, agent());
        let context = answered.context.clone().unwrap();
        answer_all(&mut state, asked, &ResponseBody::ContextUsage(context));

        assert_eq!(*details(&state), loaded(&answered));
    }

    /// The daemon sends the context it answered in an update too, and the app can apply the
    /// updates before the answer.
    #[test]
    fn a_late_context_answer_leaves_a_newer_update() {
        let mut state = working();
        whole(&mut state, 1, AgentDetails::default());
        let answered = with_context(4_000);
        let newer = with_context(9_000);

        let asked = actions::toggle_usage(&mut state, agent());
        update_to(&mut state, 2, &AgentDetails::default(), &answered);
        update_to(&mut state, 3, &answered, &newer);
        let context = answered.context.clone().unwrap();
        answer_all(&mut state, asked, &ResponseBody::ContextUsage(context));

        assert_eq!(*details(&state), loaded(&newer));
    }

    #[test]
    fn a_late_mcp_answer_leaves_a_newer_update() {
        let mut state = working();
        whole(&mut state, 1, AgentDetails::default());
        let answered = with_server(McpServerStatus::Failed);
        let newer = with_server(McpServerStatus::Connected);

        let asked = actions::toggle_block(&mut state, agent(), Block::Mcp);
        update_to(&mut state, 2, &AgentDetails::default(), &answered);
        update_to(&mut state, 3, &answered, &newer);
        let servers = answered.mcp_servers.clone();
        answer_all(&mut state, asked, &ResponseBody::McpServers { servers });

        assert_eq!(*details(&state), loaded(&newer));
    }
}

//! How the daemon's events and the connection's changes update the state. Pure: the caller shows
//! the returned alerts.

use std::error::Error;
use std::time::Instant;

use cockpit_client::SessionUpdate;
use cockpit_protocol::{
    AgentId, AgentStatus, AgentSummary, AnswerState, Catchup, Event, EventRecord, ItemBody,
    SignInWait, Snapshot,
};

use super::accounts::{self, SignIns};
use super::actions;
use super::app_state::{AgentEntry, AppState, ConnectionState, Loadable};
use super::drafts::Drafts;
use super::transcript::KEPT_ITEMS_AWAY;
use crate::machine::MachineId;

/// A desktop notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub agent: AgentId,
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    NeedsYou,
    Finished,
}

/// Applies an update from `machine`'s daemon, whose agent ids are already the app's.
pub fn apply_update(
    state: &mut AppState,
    drafts: &mut Drafts,
    update: SessionUpdate,
    now: Instant,
    machine: MachineId,
) -> Vec<Alert> {
    let Some(known) = state.machines.get_mut(machine) else {
        return Vec::new();
    };
    match update {
        SessionUpdate::Connecting { attempt } => {
            known.connection = ConnectionState::Connecting { attempt };
            Vec::new()
        }
        SessionUpdate::Connected(catchup) => {
            known.connection = ConnectionState::Connected;
            // The daemon may have been updated.
            known.folders_unknown = false;
            apply_catchup(state, drafts, catchup, machine);
            Vec::new()
        }
        SessionUpdate::Disconnected { error, retry_in } => {
            known.connection = ConnectionState::Disconnected {
                error: error_chain(&error),
                retry_at: now.checked_add(retry_in).unwrap_or(now),
            };
            Vec::new()
        }
        SessionUpdate::Event(record) => {
            note_turn_start(state, &record.event, now);
            apply(state, drafts, record, machine).into_iter().collect()
        }
    }
}

/// Keeps when the app saw the agent's turn start, for a daemon that does not send its own. A
/// turn runs while the agent works or waits for an answer. The daemon sends a queued prompt as a
/// turn ends, so the agent works on without a pause: a turn's end starts the next one, until the
/// agent idles. An agent that waits only for its account's sign-in runs no turn.
fn note_turn_start(state: &mut AppState, event: &Event, now: Instant) {
    let in_turn = |summary: &AgentSummary| {
        matches!(
            accounts::turn_status(summary),
            AgentStatus::Working | AgentStatus::NeedsYou
        )
    };
    match event {
        Event::AgentUpdated(summary) => {
            let Some(entry) = state.agents.get_mut(&summary.id) else {
                return;
            };
            match (in_turn(&entry.summary), in_turn(summary)) {
                (false, true) => entry.turn_started = Some(now),
                (_, false) => entry.turn_started = None,
                (true, true) => {}
            }
        }
        Event::TranscriptAppended { agent, item } if matches!(item.body, ItemBody::TurnEnd(_)) => {
            if let Some(entry) = state.agents.get_mut(agent)
                && in_turn(&entry.summary)
            {
                entry.turn_started = Some(now);
            }
        }
        _ => {}
    }
}

/// With a snapshot, `machine`'s agents are replaced and what came by request (transcripts,
/// details, file texts) is forgotten, to be asked for again. With a replay, the events that
/// follow catch up.
pub fn apply_catchup(
    state: &mut AppState,
    drafts: &mut Drafts,
    catchup: Catchup,
    machine: MachineId,
) {
    let Catchup::Snapshot(snapshot) = catchup else {
        return;
    };
    let Snapshot {
        agents,
        plan_usage,
        settings,
        last_seq: _,
        accounts,
        other_plan_usage,
    } = snapshot;
    state
        .agents
        .retain(|agent, _| MachineId::of(*agent) != machine);
    state.agents.extend(
        agents
            .into_iter()
            .map(|summary| (summary.id, AgentEntry::new(summary))),
    );
    if let Some(known) = state.machines.get_mut(machine) {
        known.plan_usage = plan_usage;
        known.settings = settings;
        known.accounts = accounts;
        // A new connection may reach another daemon: its accounts' sign-ins are asked again.
        known.sign_ins = SignIns::default();
        known.other_plan_usage = other_plan_usage
            .into_iter()
            .map(|account| (account.account, account.usage))
            .collect();
    }
    // The snapshot holds the agents the daemon still has, closed or not.
    state
        .ui
        .closing
        .retain(|agent| MachineId::of(*agent) != machine);
    let agents = &state.agents;
    state.ui.dialog.take_if(|dialog| {
        dialog
            .agent()
            .is_some_and(|agent| !agents.contains_key(&agent))
    });
    state.ui.unread.retain(|agent| agents.contains_key(agent));
    state
        .ui
        .title_edit
        .take_if(|edit| !agents.contains_key(&edit.agent));
    state
        .ui
        .file_views
        .retain(|agent, _| agents.contains_key(agent));
    state
        .ui
        .finished_tasks_unfolded
        .retain(|agent| agents.contains_key(agent));
    state
        .ui
        .chat_scrolled_up
        .retain(|agent, _| agents.contains_key(agent));
    state
        .ui
        .chat_new_from
        .retain(|agent, _| agents.contains_key(agent));
    drafts.retain_agents(|agent| agents.contains_key(&agent));
    state.fix_selection();
    actions::follow_hidden_title_edit(state, drafts);
}

pub fn apply(
    state: &mut AppState,
    drafts: &mut Drafts,
    record: EventRecord,
    machine: MachineId,
) -> Option<Alert> {
    match record.event {
        Event::AgentAdded(summary) if state.ui.closing.contains(&summary.id) => None,
        Event::AgentAdded(summary) => {
            state.agents.insert(summary.id, AgentEntry::new(summary));
            if state.ui.selected.is_none() {
                state.fix_selection();
            }
            None
        }
        Event::AgentUpdated(summary) => {
            let alert = update_agent(state, summary);
            actions::follow_hidden_title_edit(state, drafts);
            alert
        }
        Event::AgentRemoved { agent } => {
            forget_agent(state, drafts, agent);
            state.ui.closing.remove(&agent);
            if state.ui.selected == Some(agent) {
                state.ui.selected = None;
                state.fix_selection();
            }
            None
        }
        Event::TranscriptAppended { agent, item } => {
            let looked_at = state.ui.selected == Some(agent);
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.transcript.append(item);
                if !looked_at {
                    entry.transcript.trim(KEPT_ITEMS_AWAY);
                }
            }
            None
        }
        Event::TranscriptChanged { agent, item } => {
            if let ItemBody::Question(question) = &item.body
                && question.answer != AnswerState::Waiting
            {
                drafts.questions.remove(&question.pending);
            }
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.transcript.change(item);
            }
            None
        }
        Event::DetailsChanged { agent, details } => {
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.details = Loadable::Loaded(details);
            }
            None
        }
        Event::PlanUsageChanged(plan_usage) => {
            if let Some(known) = state.machines.get_mut(machine) {
                known.plan_usage = plan_usage;
            }
            None
        }
        Event::SettingsChanged(settings) => {
            if let Some(known) = state.machines.get_mut(machine) {
                known.settings = settings;
            }
            None
        }
        Event::AccountPlanUsageChanged(changed) => {
            if let Some(known) = state.machines.get_mut(machine) {
                known
                    .other_plan_usage
                    .insert(changed.account, changed.usage);
            }
            None
        }
        Event::AccountsChanged { accounts } => {
            if let Some(known) = state.machines.get_mut(machine) {
                known.accounts = accounts;
                known.sign_ins.refresh();
            }
            None
        }
    }
}

/// Forgets an agent the daemon removed or the app closed: its entry, which it returns, its drafts,
/// its file view, and an edit or a dialog about it.
pub fn forget_agent(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
) -> Option<AgentEntry> {
    let entry = state.agents.remove(&agent);
    let items = entry.iter().flat_map(|entry| entry.transcript.items());
    for item in items {
        if let ItemBody::Question(question) = &item.body {
            drafts.questions.remove(&question.pending);
        }
    }
    state.ui.unread.remove(&agent);
    state.ui.file_views.remove(&agent);
    state.ui.finished_tasks_unfolded.remove(&agent);
    state.ui.chat_scrolled_up.remove(&agent);
    state.ui.chat_new_from.remove(&agent);
    drafts.retain_agents(|kept| kept != agent);
    state.ui.title_edit.take_if(|edit| edit.agent == agent);
    state
        .ui
        .dialog
        .take_if(|dialog| dialog.agent() == Some(agent));
    entry
}

fn update_agent(state: &mut AppState, summary: AgentSummary) -> Option<Alert> {
    let agent = summary.id;
    if state.ui.closing.contains(&agent) {
        return None;
    }
    let selected = state.ui.selected == Some(agent);
    let looked_at = selected && state.ui.window_focused;
    let account_name =
        accounts::account(state, summary.account).map(|account| account.name.clone());
    let Some(entry) = state.agents.get_mut(&agent) else {
        state.agents.insert(agent, AgentEntry::new(summary));
        return None;
    };
    let before = entry.summary.status;
    entry.summary = summary;
    let summary = &entry.summary;
    let after = summary.status;
    if after == AgentStatus::NeedsYou && before != AgentStatus::NeedsYou {
        let body = if summary.sign_in_wait == SignInWait::Waiting {
            match account_name {
                Some(name) => format!("The account {name} is not signed in."),
                None => "Its account is not signed in.".to_owned(),
            }
        } else {
            summary.activity.clone().unwrap_or_else(|| {
                "An approval, a question or a plan waits for your answer.".to_owned()
            })
        };
        return Some(Alert {
            agent,
            kind: AlertKind::NeedsYou,
            title: format!("{} needs you", summary.title),
            body,
        });
    }
    let finished = after == AgentStatus::Idle
        && matches!(before, AgentStatus::Working | AgentStatus::NeedsYou);
    if !finished {
        return None;
    }
    let mut body = summary.activity.clone().unwrap_or_default();
    if let Some(progress) = summary.task_progress {
        if !body.is_empty() {
            body.push_str(". ");
        }
        body.push_str(&format!(
            "{} of {} tasks done.",
            progress.done, progress.total
        ));
    }
    let title = format!("{} finished", summary.title);
    if !selected {
        state.ui.unread.insert(agent);
    }
    (!looked_at).then_some(Alert {
        agent,
        kind: AlertKind::Finished,
        title,
        body,
    })
}

/// An error with its causes, as one line for a person.
pub fn error_chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let part = cause.to_string();
        if !text.ends_with(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use cockpit_client::{ConnectError, SessionError};
    use cockpit_protocol::{
        AgentDetails, CostUsd, ItemId, PendingId, PlanUsage, Question, Seq, Settings, TaskProgress,
        TokenUsage, TranscriptItem, TurnEnd, TurnOutcome,
    };

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::prefs::StatusFilter;
    use crate::state::MachineState;
    use crate::state::agent_map;
    use crate::state::app_state::tests::{placed, state_with, summary};
    use crate::state::ui_state::{Block, Dialog, PaletteOrigin, Popover, TitleEdit, TitlePlace};

    fn record(event: Event) -> EventRecord {
        EventRecord {
            seq: Seq::new(1),
            event,
        }
    }

    fn with_status(id: u32, status: AgentStatus) -> AgentSummary {
        let mut summary = summary(id, status);
        summary.activity = Some("Running the tests".to_owned());
        summary
    }

    fn notice(id: u32, text: &str) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Notice {
                text: text.to_owned(),
                detail: None,
            },
        }
    }

    #[test]
    fn a_machines_snapshot_and_events_leave_the_other_machines_alone() {
        let devbox = MachineId::new(1);
        let on_devbox = |id| devbox.app_agent(AgentId::new(id));
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.others.insert(
            devbox,
            MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
        );
        let mut remote = summary(1, AgentStatus::Working);
        remote.id = on_devbox(1);

        apply_update(
            &mut state,
            &mut Drafts::default(),
            SessionUpdate::Connected(Catchup::Snapshot(Snapshot {
                agents: vec![remote],
                plan_usage: PlanUsage::default(),
                settings: Settings::default(),
                last_seq: None,
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            })),
            Instant::now(),
            devbox,
        );
        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::SettingsChanged(Settings {
                hidden_mcp_servers: BTreeSet::from(["notes".to_owned()]),
            })),
            devbox,
        );

        let agents: Vec<AgentId> = state.agents.keys().copied().collect();
        assert_eq!(agents, [AgentId::new(1), on_devbox(1)]);
        assert_eq!(
            state.machines.this.connection,
            ConnectionState::Connecting { attempt: 1 }
        );
        assert!(state.machines.others[&devbox].is_connected());
        assert!(state.machines.this.settings.hidden_mcp_servers.is_empty());
        assert!(
            state.machines.others[&devbox]
                .settings
                .hidden_mcp_servers
                .contains("notes")
        );
    }

    #[test]
    fn needing_you_raises_an_alert_once() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        state.ui.selected = Some(AgentId::new(1));

        let alert = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::NeedsYou))),
            MachineId::THIS,
        );
        let again = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::NeedsYou))),
            MachineId::THIS,
        );

        let alert = alert.unwrap();
        assert_eq!(alert.kind, AlertKind::NeedsYou);
        assert_eq!(alert.title, "Agent 1 needs you");
        assert_eq!(alert.body, "Running the tests");
        assert_eq!(again, None);
    }

    #[test]
    fn an_agent_the_filter_hides_still_raises_its_alerts() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        state.prefs.sidebar.filter = StatusFilter::Idle;

        let alert = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(2, AgentStatus::NeedsYou))),
            MachineId::THIS,
        );

        assert_eq!(alert.map(|alert| alert.kind), Some(AlertKind::NeedsYou));
    }

    #[test]
    fn finishing_elsewhere_marks_unread_and_alerts() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Working),
            summary(2, AgentStatus::Idle),
        ]);
        state.ui.selected = Some(AgentId::new(2));
        let mut finished = with_status(1, AgentStatus::Idle);
        finished.task_progress = Some(TaskProgress { done: 4, total: 4 });

        let alert = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(finished)),
            MachineId::THIS,
        )
        .unwrap();

        assert_eq!(alert.kind, AlertKind::Finished);
        assert_eq!(alert.body, "Running the tests. 4 of 4 tasks done.");
        assert!(state.is_unread(AgentId::new(1)));
    }

    #[test]
    fn finishing_under_your_eyes_is_silent_but_not_when_the_window_is_away() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        state.ui.selected = Some(AgentId::new(1));

        let silent = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::Idle))),
            MachineId::THIS,
        );
        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::Working))),
            MachineId::THIS,
        );
        state.ui.window_focused = false;
        let away = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::Idle))),
            MachineId::THIS,
        );

        assert_eq!(silent, None);
        assert!(away.is_some());
        assert!(!state.is_unread(AgentId::new(1)));
    }

    #[test]
    fn transcript_events_reach_the_agent() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        let agent = AgentId::new(1);

        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::TranscriptAppended {
                agent,
                item: notice(1, "first"),
            }),
            MachineId::THIS,
        );
        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::TranscriptChanged {
                agent,
                item: notice(1, "changed"),
            }),
            MachineId::THIS,
        );

        let transcript = &state.agents[&agent].transcript;
        assert_eq!(transcript.get(ItemId::new(1)), Some(&notice(1, "changed")));
    }

    #[test]
    fn an_agent_you_do_not_look_at_keeps_its_newest_items() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Working),
            summary(2, AgentStatus::Working),
        ]);
        state.ui.selected = Some(AgentId::new(2));
        let last = u32::try_from(KEPT_ITEMS_AWAY).unwrap() + 3;
        for id in 1..=last {
            for agent in [AgentId::new(1), AgentId::new(2)] {
                apply(
                    &mut state,
                    &mut Drafts::default(),
                    record(Event::TranscriptAppended {
                        agent,
                        item: notice(id, "live"),
                    }),
                    MachineId::THIS,
                );
            }
        }

        let away = &state.agents[&AgentId::new(1)].transcript;
        let looked_at = &state.agents[&AgentId::new(2)].transcript;
        assert_eq!(away.items().count(), KEPT_ITEMS_AWAY);
        assert_eq!(away.first_id(), Some(ItemId::new(4)));
        assert_eq!(looked_at.items().count(), KEPT_ITEMS_AWAY + 3);
    }

    #[test]
    fn an_answered_question_drops_its_draft() {
        let mut state = state_with(vec![summary(1, AgentStatus::NeedsYou)]);
        let mut drafts = Drafts::default();
        let pending = PendingId::new("request-1".to_owned());
        drafts.questions.insert(pending.clone(), Default::default());
        let question = Question {
            pending: pending.clone(),
            questions: Vec::new(),
            answer: AnswerState::Cancelled,
        };

        apply(
            &mut state,
            &mut drafts,
            record(Event::TranscriptChanged {
                agent: AgentId::new(1),
                item: TranscriptItem {
                    id: ItemId::new(1),
                    parent: None,
                    body: ItemBody::Question(question),
                },
            }),
            MachineId::THIS,
        );

        assert!(!drafts.questions.contains_key(&pending));
    }

    /// The question of an agent that is gone is never answered, so its draft would stay.
    #[test]
    fn a_removed_agent_leaves_no_question_draft() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::NeedsYou),
            summary(2, AgentStatus::NeedsYou),
        ]);
        let mut drafts = Drafts::default();
        for agent in [1, 2] {
            let pending = PendingId::new(format!("request-{agent}"));
            drafts.questions.insert(pending.clone(), Default::default());
            let question = Question {
                pending,
                questions: Vec::new(),
                answer: AnswerState::Waiting,
            };
            if let Some(entry) = state.agents.get_mut(&AgentId::new(agent)) {
                entry.transcript.append(TranscriptItem {
                    id: ItemId::new(4),
                    parent: Some(ItemId::new(3)),
                    body: ItemBody::Question(question),
                });
            }
        }

        apply(
            &mut state,
            &mut drafts,
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );

        let kept: Vec<&PendingId> = drafts.questions.keys().collect();
        assert_eq!(kept, [&PendingId::new("request-2".to_owned())]);
    }

    /// Ids are not reused, so a removed agent's marks would stay for the app's life.
    #[test]
    fn a_removed_agent_leaves_no_chat_marks() {
        let marked = || {
            let mut state = state_with(vec![
                summary(1, AgentStatus::Idle),
                summary(2, AgentStatus::Idle),
            ]);
            for agent in [AgentId::new(1), AgentId::new(2)] {
                state
                    .ui
                    .chat_scrolled_up
                    .insert(agent, Some(ItemId::new(3)));
                state.ui.chat_new_from.insert(agent, ItemId::new(4));
            }
            state
        };
        let mut removed = marked();
        let mut dropped = marked();

        apply(
            &mut removed,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );
        apply_catchup(
            &mut dropped,
            &mut Drafts::default(),
            Catchup::Snapshot(Snapshot {
                agents: vec![summary(2, AgentStatus::Idle)],
                plan_usage: PlanUsage::default(),
                settings: Settings::default(),
                last_seq: Some(Seq::new(40)),
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            }),
            MachineId::THIS,
        );

        for state in [&removed, &dropped] {
            let scrolled: Vec<&AgentId> = state.ui.chat_scrolled_up.keys().collect();
            let new_from: Vec<&AgentId> = state.ui.chat_new_from.keys().collect();
            assert_eq!(
                (scrolled, new_from),
                (vec![&AgentId::new(2)], vec![&AgentId::new(2)])
            );
        }
    }

    #[test]
    fn a_removed_agent_closes_its_map_and_leaves_another_agents_map_open() {
        let mapped = |agent| {
            let mut state = state_with(vec![
                summary(1, AgentStatus::Working),
                summary(2, AgentStatus::Working),
            ]);
            state.ui.selected = Some(AgentId::new(agent));
            agent_map::toggle(&mut state);
            state
        };
        let mut removed = mapped(1);
        let mut dropped = mapped(1);
        let mut other = mapped(2);

        apply(
            &mut removed,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );
        apply_catchup(
            &mut dropped,
            &mut Drafts::default(),
            Catchup::Snapshot(Snapshot {
                agents: vec![summary(2, AgentStatus::Working)],
                plan_usage: PlanUsage::default(),
                settings: Settings::default(),
                last_seq: Some(Seq::new(40)),
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            }),
            MachineId::THIS,
        );
        apply(
            &mut other,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );

        assert_eq!(removed.ui.dialog, None);
        assert_eq!(dropped.ui.dialog, None);
        let shown = other.ui.dialog.as_ref().and_then(Dialog::agent);
        assert_eq!(shown, Some(AgentId::new(2)));
    }

    #[test]
    fn removing_the_selected_agent_selects_the_sidebars_first() {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 1024),
            placed(2, AgentStatus::Working, 3072),
            placed(3, AgentStatus::NeedsYou, 2048),
        ]);
        state.ui.selected = Some(AgentId::new(3));

        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(3),
            }),
            MachineId::THIS,
        );

        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
        assert!(!state.agents.contains_key(&AgentId::new(3)));
    }

    /// The palette typed in the removed agent's composer would stay open over the next agent's,
    /// and Enter there would pick a command instead of sending.
    #[test]
    fn removing_the_selected_agent_closes_its_popovers() {
        let mut removed = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
        ]);
        removed.ui.selected = Some(AgentId::new(1));
        removed.ui.popover = Some(Popover::Palette(PaletteOrigin::Typed));
        removed.ui.badge = Some(Block::Mcp);
        removed.ui.mcp.detail = Some("search".to_owned());
        let mut dropped = removed.clone();
        let mut other = removed.clone();

        apply(
            &mut removed,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );
        apply_catchup(
            &mut dropped,
            &mut Drafts::default(),
            Catchup::Snapshot(Snapshot {
                agents: vec![summary(2, AgentStatus::Working)],
                plan_usage: PlanUsage::default(),
                settings: Settings::default(),
                last_seq: Some(Seq::new(40)),
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            }),
            MachineId::THIS,
        );
        apply(
            &mut other,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(2),
            }),
            MachineId::THIS,
        );

        for state in [&removed, &dropped] {
            assert_eq!(state.ui.selected, Some(AgentId::new(2)));
            assert_eq!(
                (
                    state.ui.popover,
                    state.ui.badge,
                    state.ui.mcp.detail.as_deref()
                ),
                (None, None, None)
            );
        }
        assert_eq!(
            other.ui.popover,
            Some(Popover::Palette(PaletteOrigin::Typed)),
            "another agent's removal leaves the shown one's palette open"
        );
    }

    /// A title edit left open for a removed agent would take the first Esc meant to stop the next
    /// agent.
    #[test]
    fn removing_the_agent_being_renamed_ends_the_rename() {
        let renamed = |agent: u32| TitleEdit {
            agent: AgentId::new(agent),
            title: format!("Agent {agent}"),
            place: TitlePlace::Header,
            select_all: true,
            kept_open: false,
        };
        let mut removed = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
        ]);
        removed.ui.selected = Some(AgentId::new(1));
        removed.ui.title_edit = Some(renamed(1));
        let mut dropped = removed.clone();
        let mut other = removed.clone();
        other.ui.title_edit = Some(renamed(2));

        apply(
            &mut removed,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );
        apply_catchup(
            &mut dropped,
            &mut Drafts::default(),
            Catchup::Snapshot(Snapshot {
                agents: vec![summary(2, AgentStatus::Working)],
                plan_usage: PlanUsage::default(),
                settings: Settings::default(),
                last_seq: Some(Seq::new(40)),
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            }),
            MachineId::THIS,
        );
        apply(
            &mut other,
            &mut Drafts::default(),
            record(Event::AgentRemoved {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );

        assert_eq!(removed.ui.title_edit, None);
        assert_eq!(dropped.ui.title_edit, None);
        assert_eq!(other.ui.title_edit, Some(renamed(2)));
        assert!(!removed.ui.close_overlays(), "nothing is left open for Esc");
    }

    /// The row's title field would leave the page in the middle of a word, and no field would
    /// show the edit.
    #[test]
    fn a_title_edited_in_a_row_the_filter_then_hides_moves_to_the_header_with_its_text() {
        let working = |id| placed(id, AgentStatus::Working, id * 1024);
        let mut updated = state_with(vec![working(1), working(2)]);
        let mut drafts = Drafts::default();
        updated.ui.selected = Some(AgentId::new(1));
        updated.prefs.sidebar.filter = StatusFilter::Working;
        actions::rename_key(&mut updated, &mut drafts);
        actions::rename_text(&updated, &mut drafts, "Cart tot".to_owned());
        let mut dropped = updated.clone();
        let mut dropped_drafts = drafts.clone();

        apply(
            &mut updated,
            &mut drafts,
            record(Event::AgentUpdated(with_status(1, AgentStatus::Idle))),
            MachineId::THIS,
        );
        apply_catchup(
            &mut dropped,
            &mut dropped_drafts,
            Catchup::Snapshot(Snapshot {
                agents: vec![
                    placed(1, AgentStatus::Idle, 1024),
                    placed(2, AgentStatus::Working, 2048),
                ],
                plan_usage: PlanUsage::default(),
                settings: Settings::default(),
                last_seq: Some(Seq::new(40)),
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            }),
            MachineId::THIS,
        );

        let header = Some(TitleEdit {
            agent: AgentId::new(1),
            title: "Cart tot".to_owned(),
            place: TitlePlace::Header,
            select_all: false,
            kept_open: false,
        });
        assert_eq!(updated.ui.title_edit, header);
        assert_eq!(dropped.ui.title_edit, header);
        assert_eq!(drafts.title, "Cart tot");
    }

    #[test]
    fn the_first_agent_added_is_selected() {
        let mut state = state_with(Vec::new());

        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentAdded(summary(7, AgentStatus::Working))),
            MachineId::THIS,
        );

        assert_eq!(state.ui.selected, Some(AgentId::new(7)));
    }

    #[test]
    fn a_snapshot_replaces_agents_and_forgets_requested_data() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        let first = AgentId::new(1);
        state.ui.selected = Some(AgentId::new(2));
        state.ui.unread.insert(AgentId::new(2));
        if let Some(entry) = state.agents.get_mut(&first) {
            entry.transcript.append(notice(1, "old"));
            entry.details = Loadable::Loaded(AgentDetails::default());
        }
        let mut settings = Settings::default();
        settings.hidden_mcp_servers.insert("search".to_owned());

        apply_catchup(
            &mut state,
            &mut Drafts::default(),
            Catchup::Snapshot(Snapshot {
                agents: vec![summary(1, AgentStatus::Working)],
                plan_usage: PlanUsage::default(),
                settings: settings.clone(),
                last_seq: Some(Seq::new(40)),
                accounts: Vec::new(),
                other_plan_usage: Vec::new(),
            }),
            MachineId::THIS,
        );

        let entry = &state.agents[&first];
        assert_eq!(entry.transcript.items().count(), 0);
        assert_eq!(entry.details, Loadable::NotRequested);
        assert_eq!(entry.summary.status, AgentStatus::Working);
        assert_eq!(state.ui.selected, Some(first));
        assert!(state.ui.unread.is_empty());
        assert_eq!(state.machines.this.settings, settings);
    }

    #[test]
    fn a_turn_starts_when_the_agent_starts_working() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let agent = AgentId::new(1);
        let start = Instant::now();
        let later = start + Duration::from_secs(30);
        let mut change = |status, now| {
            apply_update(
                &mut state,
                &mut Drafts::default(),
                SessionUpdate::Event(record(Event::AgentUpdated(summary(1, status)))),
                now,
                MachineId::THIS,
            );
            state.agents[&agent].turn_started
        };

        assert_eq!(change(AgentStatus::Working, start), Some(start));
        assert_eq!(change(AgentStatus::NeedsYou, later), Some(start));
        assert_eq!(change(AgentStatus::Working, later), Some(start));
        assert_eq!(change(AgentStatus::Idle, later), None);
        assert_eq!(change(AgentStatus::Working, later), Some(later));
    }

    #[test]
    fn a_wait_for_the_accounts_sign_in_runs_no_turn() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let agent = AgentId::new(1);
        let start = Instant::now();
        let mut waiting = summary(1, AgentStatus::NeedsYou);
        waiting.sign_in_wait = SignInWait::Waiting;
        let mut update = |summary, now| {
            apply_update(
                &mut state,
                &mut Drafts::default(),
                SessionUpdate::Event(record(Event::AgentUpdated(summary))),
                now,
                MachineId::THIS,
            );
            state.agents[&agent].turn_start()
        };

        update(summary(1, AgentStatus::Working), start);
        let after_the_failed_turn = update(waiting, start + Duration::from_secs(1));

        assert_eq!(after_the_failed_turn, None);
    }

    /// The daemon sends a queued prompt as the turn before it ends: the agent goes on working,
    /// and the working line counts from the end of the last turn.
    #[test]
    fn a_queued_prompt_starts_its_turn_when_the_last_one_ends() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let agent = AgentId::new(1);
        let start = Instant::now();
        let ended = start + Duration::from_secs(360);
        let mut update = |event, now| {
            apply_update(
                &mut state,
                &mut Drafts::default(),
                SessionUpdate::Event(record(event)),
                now,
                MachineId::THIS,
            );
            state.agents[&agent].turn_started
        };
        let turn_end = || TranscriptItem {
            id: ItemId::new(9),
            parent: None,
            body: ItemBody::TurnEnd(TurnEnd {
                duration: Duration::from_secs(360),
                cost: CostUsd::new(0.4),
                usage: TokenUsage::default(),
                outcome: TurnOutcome::Completed,
                error: None,
            }),
        };

        update(Event::AgentUpdated(summary(1, AgentStatus::Working)), start);
        let after_the_end = update(
            Event::TranscriptAppended {
                agent,
                item: turn_end(),
            },
            ended,
        );
        let next_turn = update(
            Event::AgentUpdated(summary(1, AgentStatus::Working)),
            ended + Duration::from_secs(1),
        );
        let idle = update(Event::AgentUpdated(summary(1, AgentStatus::Idle)), ended);
        let ended_while_idle = update(
            Event::TranscriptAppended {
                agent,
                item: turn_end(),
            },
            ended,
        );

        assert_eq!((after_the_end, next_turn), (Some(ended), Some(ended)));
        assert_eq!((idle, ended_while_idle), (None, None));
    }

    #[test]
    fn a_replay_keeps_the_state() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let before = state.clone();

        apply_catchup(
            &mut state,
            &mut Drafts::default(),
            Catchup::Replay,
            MachineId::THIS,
        );

        assert_eq!(state, before);
    }

    #[test]
    fn connection_updates_set_the_connection_state() {
        let mut state = state_with(Vec::new());
        let now = Instant::now();

        apply_update(
            &mut state,
            &mut Drafts::default(),
            SessionUpdate::Connecting { attempt: 3 },
            now,
            MachineId::THIS,
        );
        assert_eq!(
            state.machines.this.connection,
            ConnectionState::Connecting { attempt: 3 }
        );

        apply_update(
            &mut state,
            &mut Drafts::default(),
            SessionUpdate::Disconnected {
                error: SessionError::Connect(ConnectError::Ssh {
                    stderr: "Permission denied (publickey).".to_owned(),
                }),
                retry_in: Duration::from_secs(4),
            },
            now,
            MachineId::THIS,
        );
        assert_eq!(
            state.machines.this.connection,
            ConnectionState::Disconnected {
                error: "ssh failed: Permission denied (publickey).".to_owned(),
                retry_at: now + Duration::from_secs(4),
            }
        );

        apply_update(
            &mut state,
            &mut Drafts::default(),
            SessionUpdate::Connected(Catchup::Replay),
            now,
            MachineId::THIS,
        );
        assert!(state.is_connected());
    }
}

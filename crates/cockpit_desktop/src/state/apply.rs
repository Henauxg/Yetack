//! How the daemon's events and the connection's changes update the state. Pure: the caller shows
//! the returned alerts.

use std::error::Error;
use std::time::Instant;

use cockpit_client::SessionUpdate;
use cockpit_protocol::{
    AgentId, AgentStatus, AgentSummary, AnswerState, Catchup, Event, EventRecord, ItemBody,
    Snapshot,
};

use super::app_state::{AgentEntry, AppState, ConnectionState, Loadable};
use super::drafts::Drafts;
use super::transcript::KEPT_ITEMS_AWAY;

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

pub fn apply_update(
    state: &mut AppState,
    drafts: &mut Drafts,
    update: SessionUpdate,
    now: Instant,
) -> Vec<Alert> {
    match update {
        SessionUpdate::Connecting { attempt } => {
            state.connection = ConnectionState::Connecting { attempt };
            Vec::new()
        }
        SessionUpdate::Connected(catchup) => {
            state.connection = ConnectionState::Connected;
            apply_catchup(state, drafts, catchup);
            Vec::new()
        }
        SessionUpdate::Disconnected { error, retry_in } => {
            state.connection = ConnectionState::Disconnected {
                error: error_chain(&error),
                retry_at: now.checked_add(retry_in).unwrap_or(now),
            };
            Vec::new()
        }
        SessionUpdate::Event(record) => {
            if let Event::AgentUpdated(summary) = &record.event {
                note_turn_start(state, summary, now);
            }
            apply(state, drafts, record).into_iter().collect()
        }
    }
}

/// Keeps when the agent's turn started, for the working line's elapsed time. A turn runs while
/// the agent works or waits for an answer.
fn note_turn_start(state: &mut AppState, summary: &AgentSummary, now: Instant) {
    let Some(entry) = state.agents.get_mut(&summary.id) else {
        return;
    };
    let in_turn = |status| matches!(status, AgentStatus::Working | AgentStatus::NeedsYou);
    match (in_turn(entry.summary.status), in_turn(summary.status)) {
        (false, true) => entry.turn_started = Some(now),
        (_, false) => entry.turn_started = None,
        (true, true) => {}
    }
}

/// With a snapshot, the agents are replaced and what came by request (transcripts, details, file
/// texts) is forgotten, to be asked for again. With a replay, the events that follow catch up.
pub fn apply_catchup(state: &mut AppState, drafts: &mut Drafts, catchup: Catchup) {
    let Catchup::Snapshot(snapshot) = catchup else {
        return;
    };
    let Snapshot {
        agents,
        plan_usage,
        settings,
        last_seq: _,
    } = snapshot;
    state.agents = agents
        .into_iter()
        .map(|summary| (summary.id, AgentEntry::new(summary)))
        .collect();
    state.plan_usage = plan_usage;
    state.settings = settings;
    let agents = &state.agents;
    state.ui.unread.retain(|agent| agents.contains_key(agent));
    state
        .ui
        .file_views
        .retain(|agent, _| agents.contains_key(agent));
    drafts
        .composer
        .retain(|agent, _| agents.contains_key(agent));
    state.fix_selection();
}

pub fn apply(state: &mut AppState, drafts: &mut Drafts, record: EventRecord) -> Option<Alert> {
    match record.event {
        Event::AgentAdded(summary) => {
            state.agents.insert(summary.id, AgentEntry::new(summary));
            if state.ui.selected.is_none() {
                state.fix_selection();
            }
            None
        }
        Event::AgentUpdated(summary) => update_agent(state, summary),
        Event::AgentRemoved { agent } => {
            state.agents.remove(&agent);
            state.ui.unread.remove(&agent);
            state.ui.file_views.remove(&agent);
            drafts.composer.remove(&agent);
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
            state.plan_usage = plan_usage;
            None
        }
        Event::SettingsChanged(settings) => {
            state.settings = settings;
            None
        }
    }
}

fn update_agent(state: &mut AppState, summary: AgentSummary) -> Option<Alert> {
    let agent = summary.id;
    let selected = state.ui.selected == Some(agent);
    let looked_at = selected && state.ui.window_focused;
    let Some(entry) = state.agents.get_mut(&agent) else {
        state.agents.insert(agent, AgentEntry::new(summary));
        return None;
    };
    let before = entry.summary.status;
    entry.summary = summary;
    let summary = &entry.summary;
    let after = summary.status;
    if after == AgentStatus::NeedsYou && before != AgentStatus::NeedsYou {
        let body = summary.activity.clone().unwrap_or_else(|| {
            "An approval, a question or a plan waits for your answer.".to_owned()
        });
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
    use std::time::Duration;

    use cockpit_client::{ConnectError, SessionError};
    use cockpit_protocol::{
        AgentDetails, ItemId, PendingId, PlanUsage, Question, Seq, Settings, TaskProgress,
        TranscriptItem,
    };

    use super::*;
    use crate::state::app_state::tests::{placed, state_with, summary};

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
            },
        }
    }

    #[test]
    fn needing_you_raises_an_alert_once() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        state.ui.selected = Some(AgentId::new(1));

        let alert = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::NeedsYou))),
        );
        let again = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::NeedsYou))),
        );

        let alert = alert.unwrap();
        assert_eq!(alert.kind, AlertKind::NeedsYou);
        assert_eq!(alert.title, "Agent 1 needs you");
        assert_eq!(alert.body, "Running the tests");
        assert_eq!(again, None);
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
        );
        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::Working))),
        );
        state.ui.window_focused = false;
        let away = apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentUpdated(with_status(1, AgentStatus::Idle))),
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
        );
        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::TranscriptChanged {
                agent,
                item: notice(1, "changed"),
            }),
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
        );

        assert!(!drafts.questions.contains_key(&pending));
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
        );

        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
        assert!(!state.agents.contains_key(&AgentId::new(3)));
    }

    #[test]
    fn the_first_agent_added_is_selected() {
        let mut state = state_with(Vec::new());

        apply(
            &mut state,
            &mut Drafts::default(),
            record(Event::AgentAdded(summary(7, AgentStatus::Working))),
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
            }),
        );

        let entry = &state.agents[&first];
        assert_eq!(entry.transcript.items().count(), 0);
        assert_eq!(entry.details, Loadable::NotRequested);
        assert_eq!(entry.summary.status, AgentStatus::Working);
        assert_eq!(state.ui.selected, Some(first));
        assert!(state.ui.unread.is_empty());
        assert_eq!(state.settings, settings);
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
    fn a_replay_keeps_the_state() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let before = state.clone();

        apply_catchup(&mut state, &mut Drafts::default(), Catchup::Replay);

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
        );
        assert_eq!(state.connection, ConnectionState::Connecting { attempt: 3 });

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
        );
        assert_eq!(
            state.connection,
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
        );
        assert!(state.is_connected());
    }
}

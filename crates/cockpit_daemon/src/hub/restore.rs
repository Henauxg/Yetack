//! The state a restarted daemon reads back: `agents.json`, `settings.json`, and the event logs.

use std::collections::{BTreeMap, HashMap};

use cockpit_protocol::{
    AgentDetails, AgentId, AgentSummary, Event, ItemId, PlanUsage, Seq, TranscriptItem,
};

use crate::hub::agents::initial_summary;
use crate::store::{
    AgentRecord, AgentsFile, DaemonSettings, EventLogs, LogName, StateDir, StoreError, read_log,
};
use crate::transcript::AgentView;

pub(crate) struct Restored {
    pub(crate) agents: Vec<(AgentRecord, AgentView)>,
    pub(crate) next_agent: AgentId,
    pub(crate) replay_floor: Option<Seq>,
    pub(crate) settings: DaemonSettings,
    pub(crate) plan_usage: PlanUsage,
    /// The last sequence number in the logs or the replay floor, which numbering continues from.
    pub(crate) last_seq: Option<Seq>,
}

/// What one agent's log says, once its events are applied in order.
#[derive(Default)]
struct LoggedAgent {
    summary: Option<AgentSummary>,
    details: AgentDetails,
    items: BTreeMap<ItemId, TranscriptItem>,
}

pub(crate) fn restore(state: &StateDir) -> Result<Restored, StoreError> {
    let file = AgentsFile::load(&state.agents_file())?;
    let settings = DaemonSettings::load(&state.settings_file())?;
    let mut last_seq: Option<Seq> = None;
    let mut plan_usage = PlanUsage::default();
    let mut logged: HashMap<AgentId, LoggedAgent> = HashMap::new();
    for (log, path) in EventLogs::new(state.events()).list()? {
        for record in read_log(&path)? {
            last_seq = last_seq.max(Some(record.seq));
            match log {
                LogName::Daemon => {
                    if let Event::PlanUsageChanged(usage) = record.event {
                        plan_usage = usage;
                    }
                }
                LogName::Agent(agent) => logged.entry(agent).or_default().apply(record.event),
            }
        }
    }
    let ttl = settings.prompt_cache_ttl();
    let agents: Vec<(AgentRecord, AgentView)> = file
        .agents
        .into_iter()
        .map(|record| {
            let logged = logged.remove(&record.id).unwrap_or_default();
            let mut summary = logged.summary.unwrap_or_else(|| initial_summary(&record));
            // The record is the reference for what a client chose.
            summary.title = record.title.clone();
            summary.folder = record.folder.clone();
            let items: Vec<TranscriptItem> = logged.items.into_values().collect();
            let view = AgentView::restore(summary, logged.details, items, ttl);
            (record, view)
        })
        .collect();
    Ok(Restored {
        agents,
        next_agent: file.next_agent,
        replay_floor: file.replay_floor,
        settings,
        plan_usage,
        // The floor may name an event clients saw but no log holds.
        last_seq: last_seq.max(file.replay_floor),
    })
}

impl LoggedAgent {
    fn apply(&mut self, event: Event) {
        match event {
            Event::AgentAdded(summary) | Event::AgentUpdated(summary) => {
                self.summary = Some(summary);
            }
            Event::TranscriptAppended { item, .. } | Event::TranscriptChanged { item, .. } => {
                self.items.insert(item.id, item);
            }
            Event::DetailsChanged { details, .. } => self.details = details,
            Event::AgentRemoved { .. } | Event::PlanUsageChanged(_) | Event::SettingsChanged(_) => {
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{
        AgentStatus, EventRecord, Frame, ItemBody, ServerMessage, Settings, Timestamp,
    };

    use super::*;

    fn record(id: u32) -> AgentRecord {
        AgentRecord {
            id: AgentId::new(id),
            title: format!("Agent {id}"),
            folder: PathBuf::from("/srv/work"),
            session: None,
            model: None,
            effort: None,
            mode: None,
            running: true,
            started_at: Timestamp::from_unix_millis(0),
        }
    }

    fn log(logs: &mut EventLogs, name: LogName, seq: u64, event: Event) {
        let frame = Frame::encode(&ServerMessage::Event(EventRecord {
            seq: Seq::new(seq),
            event,
        }))
        .unwrap();
        logs.append(name, &frame).unwrap();
    }

    #[test]
    fn rebuilds_the_agents_and_continues_the_numbering() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        AgentsFile {
            next_agent: AgentId::new(3),
            replay_floor: None,
            agents: vec![record(1), record(2)],
        }
        .save(&state.agents_file())
        .unwrap();
        let mut logs = EventLogs::new(state.events());
        let agent = LogName::Agent(AgentId::new(1));
        let mut summary = initial_summary(&record(1));
        summary.turns = 4;
        log(&mut logs, agent, 1, Event::AgentAdded(summary.clone()));
        let item = |id: u32, text: &str| TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Notice {
                text: text.to_owned(),
            },
        };
        log(
            &mut logs,
            agent,
            2,
            Event::TranscriptAppended {
                agent: AgentId::new(1),
                item: item(1, "first"),
            },
        );
        log(
            &mut logs,
            LogName::Daemon,
            3,
            Event::SettingsChanged(Settings::default()),
        );
        log(
            &mut logs,
            agent,
            4,
            Event::TranscriptChanged {
                agent: AgentId::new(1),
                item: item(1, "changed"),
            },
        );
        log(
            &mut logs,
            LogName::Daemon,
            5,
            Event::PlanUsageChanged(PlanUsage::default()),
        );

        let restored = restore(&state).unwrap();

        assert_eq!(restored.last_seq, Some(Seq::new(5)));
        assert_eq!(restored.next_agent, AgentId::new(3));
        let (_, first) = &restored.agents[0];
        assert_eq!(first.summary().turns, 4);
        assert_eq!(first.summary().status, AgentStatus::Exited);
        assert_eq!(first.items(), [item(1, "changed")]);
        let (_, second) = &restored.agents[1];
        assert_eq!(second.summary().title, "Agent 2");
        assert!(second.items().is_empty());
    }

    /// The floor is the sequence number of an event that could not be logged, which clients may
    /// have seen: a number at or below it must not name a new event.
    #[test]
    fn the_numbering_continues_above_the_replay_floor() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        AgentsFile {
            next_agent: AgentId::new(1),
            replay_floor: Some(Seq::new(9)),
            agents: Vec::new(),
        }
        .save(&state.agents_file())
        .unwrap();
        let mut logs = EventLogs::new(state.events());
        log(
            &mut logs,
            LogName::Daemon,
            8,
            Event::SettingsChanged(Settings::default()),
        );

        let restored = restore(&state).unwrap();

        assert_eq!(restored.last_seq, Some(Seq::new(9)));
    }

    #[test]
    fn an_empty_folder_restores_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();

        let restored = restore(&state).unwrap();

        assert!(restored.agents.is_empty());
        assert_eq!(restored.last_seq, None);
        assert_eq!(restored.next_agent, AgentId::new(1));
    }
}

//! The state a restarted daemon reads back: `agents.json`, `settings.json`, and the event logs.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use cockpit_protocol::{
    AccountId, AgentDetails, AgentId, AgentSummary, Event, ItemId, PlanUsage, Seq, TranscriptItem,
};

use crate::hub::agents::initial_summary;
use crate::store::{
    AgentRecord, AgentsFile, DaemonSettings, EventLogs, LogName, StateDir, StoreError,
    for_each_event,
};
use crate::transcript::AgentView;
use crate::{repository, sessions};

pub(crate) struct Restored {
    pub(crate) agents: Vec<(AgentRecord, AgentView)>,
    pub(crate) next_agent: AgentId,
    pub(crate) replay_floor: Option<Seq>,
    pub(crate) settings: DaemonSettings,
    /// Each account's plan usage, once one of its agents reported it.
    pub(crate) plan_usage: BTreeMap<AccountId, PlanUsage>,
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

/// `default_folder` is the default account's Claude Code folder. An account's folder holds the
/// session files that agents made from past sessions read again.
pub(crate) fn restore(state: &StateDir, default_folder: &Path) -> Result<Restored, StoreError> {
    let file = AgentsFile::load(&state.agents_file())?;
    let settings = DaemonSettings::load(&state.settings_file())?;
    let mut last_seq: Option<Seq> = None;
    let mut plan_usage: BTreeMap<AccountId, PlanUsage> = BTreeMap::new();
    let mut logged: HashMap<AgentId, LoggedAgent> = HashMap::new();
    for (log, path) in EventLogs::new(state.events()).list()? {
        for_each_event(&path, |record| {
            last_seq = last_seq.max(Some(record.seq));
            match log {
                LogName::Daemon => match record.event {
                    Event::PlanUsageChanged(usage) => {
                        plan_usage.insert(AccountId::DEFAULT, usage);
                    }
                    Event::AccountPlanUsageChanged(changed) => {
                        plan_usage.insert(changed.account, changed.usage);
                    }
                    _ => {}
                },
                LogName::Agent(agent) => logged.entry(agent).or_default().apply(record.event),
            }
        })?;
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
            summary.repository = repository::of_folder(&record.folder);
            summary.position = record.position;
            summary.account = record.account;
            summary.mark = record.mark;
            let items: Vec<TranscriptItem> = logged.items.into_values().collect();
            let mut view = AgentView::restore(summary, logged.details, items, ttl);
            let account = settings.accounts.get(record.account, default_folder);
            if let (Some(file), Some(account)) = (&record.past, account) {
                view.show_past(sessions::past_items(&account.folder, &record.folder, file));
            }
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
            Event::AgentRemoved { .. }
            | Event::PlanUsageChanged(_)
            | Event::SettingsChanged(_)
            | Event::AccountPlanUsageChanged(_)
            | Event::AccountsChanged { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{
        AgentStatus, ClaudeSessionId, EventRecord, Frame, ItemBody, Mark, Position, ServerMessage,
        Settings, Timestamp,
    };

    use crate::store::{PastFile, ResumeAs};

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
            past: None,
            position: Position::default(),
            process: None,
            account: AccountId::DEFAULT,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        }
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
        log(
            &mut logs,
            agent,
            2,
            Event::TranscriptAppended {
                agent: AgentId::new(1),
                item: notice(1, "first"),
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
                item: notice(1, "changed"),
            },
        );
        log(
            &mut logs,
            LogName::Daemon,
            5,
            Event::PlanUsageChanged(PlanUsage::default()),
        );

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        assert_eq!(restored.last_seq, Some(Seq::new(5)));
        assert_eq!(restored.next_agent, AgentId::new(3));
        let (_, first) = &restored.agents[0];
        assert_eq!(first.summary().turns, 4);
        assert_eq!(first.summary().status, AgentStatus::Exited);
        assert_eq!(first.items(), [notice(1, "changed")]);
        let (_, second) = &restored.agents[1];
        assert_eq!(second.summary().title, "Agent 2");
        assert!(second.items().is_empty());
    }

    /// The saved mark wins over the one the agent's last logged summary carries.
    #[test]
    fn the_mark_comes_back_from_the_agents_file() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        let marked = AgentRecord {
            mark: Mark::ToHandle,
            ..record(1)
        };
        AgentsFile {
            next_agent: AgentId::new(3),
            replay_floor: None,
            agents: vec![marked.clone(), record(2)],
        }
        .save(&state.agents_file())
        .unwrap();
        let mut logs = EventLogs::new(state.events());
        let mut cleared = initial_summary(&marked);
        cleared.mark = Mark::Unmarked;
        log(
            &mut logs,
            LogName::Agent(AgentId::new(1)),
            1,
            Event::AgentAdded(cleared),
        );
        let mut stale = initial_summary(&record(2));
        stale.mark = Mark::ToHandle;
        log(
            &mut logs,
            LogName::Agent(AgentId::new(2)),
            2,
            Event::AgentAdded(stale),
        );

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        let marks: Vec<Mark> = restored
            .agents
            .iter()
            .map(|(_, view)| view.summary().mark)
            .collect();
        assert_eq!(marks, [Mark::ToHandle, Mark::Unmarked]);
    }

    /// Each log applies in its own order, wherever the other logs put their numbers, and a line
    /// that does not decode costs only itself.
    #[test]
    fn interleaved_logs_and_a_broken_line_restore_the_state_they_logged() {
        use std::fs::OpenOptions;
        use std::io::Write;

        use cockpit_protocol::{ChangedFile, RateLimitStatus, UsageWindow, WindowUsage};

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
        let (first, second) = (AgentId::new(1), AgentId::new(2));
        let mut summary = initial_summary(&record(1));
        let details = AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/srv/work/cart.rs"),
                lines_added: 3,
                lines_removed: 1,
            }],
            ..AgentDetails::default()
        };
        let usage = PlanUsage {
            windows: BTreeMap::from([(
                UsageWindow::FiveHour,
                WindowUsage {
                    utilization: Some(0.5),
                    resets_at: None,
                    status: RateLimitStatus::Allowed,
                },
            )]),
        };
        let appended = |agent, item| Event::TranscriptAppended { agent, item };
        log(
            &mut logs,
            LogName::Agent(first),
            1,
            Event::AgentAdded(summary.clone()),
        );
        log(
            &mut logs,
            LogName::Agent(second),
            2,
            appended(second, notice(1, "a")),
        );
        let details_changed = Event::DetailsChanged {
            agent: first,
            details: details.clone(),
        };
        log(&mut logs, LogName::Agent(first), 3, details_changed);
        OpenOptions::new()
            .append(true)
            .open(logs.path(LogName::Agent(second)))
            .unwrap()
            .write_all(b"not an event\n")
            .unwrap();
        log(
            &mut logs,
            LogName::Agent(second),
            4,
            appended(second, notice(2, "b")),
        );
        log(
            &mut logs,
            LogName::Daemon,
            5,
            Event::PlanUsageChanged(usage.clone()),
        );
        summary.turns = 2;
        log(
            &mut logs,
            LogName::Agent(first),
            6,
            Event::AgentUpdated(summary),
        );

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        assert_eq!(restored.last_seq, Some(Seq::new(6)));
        assert_eq!(
            restored.plan_usage,
            BTreeMap::from([(AccountId::DEFAULT, usage)])
        );
        let (_, first) = &restored.agents[0];
        assert_eq!(first.summary().turns, 2);
        assert_eq!(first.details(), &details);
        let (_, second) = &restored.agents[1];
        assert_eq!(second.items(), [notice(1, "a"), notice(2, "b")]);
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

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        assert_eq!(restored.last_seq, Some(Seq::new(9)));
    }

    #[test]
    fn an_agent_made_from_a_past_session_reads_its_past_again_above_its_own_items() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        let session = ClaudeSessionId::new("11111111-2222-3333-4444-555555555555".to_owned());
        let project = temp.path().join("claude/projects/-srv-work");
        std::fs::create_dir_all(&project).unwrap();
        let past = r#"{"type":"user","message":{"role":"user","content":"Fix the cart"}}"#;
        let own = r#"{"type":"user","message":{"role":"user","content":"Own prompt"}}"#;
        std::fs::write(
            project.join(format!("{}.jsonl", session.as_str())),
            format!("{past}\n{own}\n"),
        )
        .unwrap();
        let mut agent = record(1);
        agent.session = Some(session.clone());
        agent.past = Some(PastFile {
            session,
            bytes: u64::try_from(past.len() + 1).unwrap(),
        });
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![agent.clone()],
        }
        .save(&state.agents_file())
        .unwrap();
        let mut logs = EventLogs::new(state.events());
        let name = LogName::Agent(AgentId::new(1));
        log(
            &mut logs,
            name,
            1,
            Event::AgentAdded(initial_summary(&agent)),
        );
        log(
            &mut logs,
            name,
            2,
            Event::TranscriptAppended {
                agent: AgentId::new(1),
                item: notice(2002, "own"),
            },
        );

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        let (_, view) = &restored.agents[0];
        let ids: Vec<u32> = view.items().iter().map(|item| item.id.get()).collect();
        assert_eq!(ids, [2001, 2002]);
        let ItemBody::UserMessage(message) = &view.items()[0].body else {
            panic!("expected the past prompt, got {:?}", view.items());
        };
        assert_eq!(message.text, "Fix the cart");
    }

    /// Each account keeps the last usage logged for it.
    #[test]
    fn plan_usage_comes_back_per_account() {
        use cockpit_protocol::{AccountPlanUsage, RateLimitStatus, UsageWindow, WindowUsage};

        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        let usage = |utilization| PlanUsage {
            windows: BTreeMap::from([(
                UsageWindow::FiveHour,
                WindowUsage {
                    utilization: Some(utilization),
                    resets_at: None,
                    status: RateLimitStatus::Allowed,
                },
            )]),
        };
        let work = AccountId::new(2);
        let mut logs = EventLogs::new(state.events());
        let events = [
            Event::PlanUsageChanged(usage(0.1)),
            Event::AccountPlanUsageChanged(AccountPlanUsage {
                account: work,
                usage: usage(0.6),
            }),
            Event::PlanUsageChanged(usage(0.2)),
        ];
        for (seq, event) in (1..).zip(events) {
            log(&mut logs, LogName::Daemon, seq, event);
        }

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        assert_eq!(
            restored.plan_usage,
            BTreeMap::from([(AccountId::DEFAULT, usage(0.2)), (work, usage(0.6))])
        );
    }

    #[test]
    fn an_empty_folder_restores_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();

        let restored = restore(&state, &temp.path().join("claude")).unwrap();

        assert!(restored.agents.is_empty());
        assert_eq!(restored.last_seq, None);
        assert_eq!(restored.next_agent, AgentId::new(1));
    }
}

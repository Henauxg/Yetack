//! The agent list's order: a new agent goes on top, and only `agent.move` moves an agent.

use cockpit_protocol::{AgentId, ErrorBody, MoveParams, Placed, ResponseBody};

use crate::hub::errors;
use crate::hub::state::Hub;

impl Hub {
    /// Every agent's place, in the list's order.
    pub(super) fn places(&self) -> Vec<Placed> {
        let mut places: Vec<Placed> = self
            .agents
            .values()
            .map(|agent| Placed {
                agent: agent.record.id,
                position: agent.record.position,
            })
            .collect();
        places.sort_by(|left, right| cockpit_protocol::list_order(*left, *right));
        places
    }

    /// Gives agents their new positions, in their records and their summaries. The caller saves
    /// and publishes.
    pub(super) fn set_places(&mut self, places: &[Placed]) {
        for place in places {
            if let Some(agent) = self.agents.get_mut(&place.agent) {
                agent.record.position = place.position;
                agent.view.set_position(place.position);
            }
        }
    }

    pub(super) fn move_agent(&mut self, params: MoveParams) -> Result<ResponseBody, ErrorBody> {
        let MoveParams { agent, before } = params;
        let unknown = [Some(agent), before]
            .into_iter()
            .flatten()
            .find(|id| !self.agents.contains_key(id));
        if let Some(id) = unknown {
            return Err(errors::agent_not_found(id));
        }
        let changed = cockpit_protocol::move_agent(&self.places(), agent, before);
        if changed.is_empty() {
            return Ok(ResponseBody::Done);
        }
        self.set_places(&changed);
        self.save_agents();
        let moved: Vec<AgentId> = changed.iter().map(|place| place.agent).collect();
        for id in moved {
            self.publish(id);
        }
        Ok(ResponseBody::Done)
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::path::Path;

    use cockpit_protocol::{
        AccountId, ClaudeSessionId, ErrorCode, Mark, PastSession, Position, RenameParams,
        ResponseResult, StartParams, Timestamp,
    };
    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::sessions::{FoundSession, SessionPast};
    use crate::store::{AgentRecord, AgentsFile, PastFile, ResumeAs, StateDir};
    use crate::test_script;

    /// A hub whose `claude` reads its stdin and says nothing.
    fn hub(folder: &Path) -> Hub {
        let mut hub = Hub::for_tests(&folder.join("state"));
        let claude = folder.join("claude");
        test_script::write_script(&claude, "#!/bin/sh\nexec cat >/dev/null\n");
        hub.config.claude_program = claude;
        hub
    }

    fn start(hub: &mut Hub, folder: &Path, title: &str) -> AgentId {
        let params = StartParams {
            folder: folder.to_owned(),
            title: Some(title.to_owned()),
            model: None,
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        };
        match hub.start_agent(params) {
            Ok(ResponseBody::AgentStarted { agent }) => agent,
            other => panic!("the agent did not start: {other:?}"),
        }
    }

    fn order(hub: &Hub) -> Vec<u32> {
        hub.places().iter().map(|place| place.agent.get()).collect()
    }

    /// The order a client gets from a snapshot.
    fn snapshot_order(hub: &Hub) -> Vec<u32> {
        let mut agents = hub.snapshot().agents;
        agents.sort_by(|left, right| cockpit_protocol::list_order(left.place(), right.place()));
        agents.iter().map(|summary| summary.id.get()).collect()
    }

    fn move_params(agent: u32, before: Option<u32>) -> MoveParams {
        MoveParams {
            agent: AgentId::new(agent),
            before: before.map(AgentId::new),
        }
    }

    #[tokio::test]
    async fn a_new_agent_goes_on_top_and_a_move_publishes_the_moved_agent_alone() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = hub(temp.path());
        for title in ["First", "Second", "Third"] {
            start(&mut hub, temp.path(), title);
        }
        let after_start = order(&hub);
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);

        let answer = hub.move_agent(move_params(1, Some(3)));

        assert_eq!(after_start, [3, 2, 1]);
        assert_eq!(answer, Ok(ResponseBody::Done));
        assert_eq!(order(&hub), [1, 3, 2]);
        assert_eq!(snapshot_order(&hub), [1, 3, 2]);
        let mut sent = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            sent.push(String::from_utf8_lossy(frame.as_bytes()).into_owned());
        }
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].contains(r#""type":"agent_updated","id":1,"#),
            "{sent:?}"
        );
        let saved = AgentsFile::load(&hub.state.agents_file()).unwrap();
        let saved_top = saved
            .agents
            .iter()
            .max_by_key(|record| record.position)
            .map(|record| record.id);
        assert_eq!(saved_top, Some(AgentId::new(1)));
    }

    #[tokio::test]
    async fn an_agent_moves_to_the_bottom_and_back() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = hub(temp.path());
        for title in ["First", "Second", "Third"] {
            start(&mut hub, temp.path(), title);
        }

        hub.move_agent(move_params(3, None)).unwrap();
        let at_bottom = order(&hub);
        hub.move_agent(move_params(3, Some(2))).unwrap();
        let back = order(&hub);
        hub.move_agent(move_params(2, Some(2))).unwrap();

        assert_eq!(at_bottom, [2, 1, 3]);
        assert_eq!(back, [3, 2, 1]);
        assert_eq!(order(&hub), [3, 2, 1]);
    }

    #[tokio::test]
    async fn a_rename_or_a_stop_does_not_move_an_agent() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = hub(temp.path());
        for title in ["First", "Second"] {
            start(&mut hub, temp.path(), title);
        }
        let before = hub.places();

        let rename = RenameParams {
            agent: AgentId::new(1),
            title: "Renamed".to_owned(),
        };
        hub.rename(rename).unwrap();
        hub.stop(AgentId::new(1)).unwrap();

        assert_eq!(hub.places(), before);
    }

    #[test]
    fn moving_an_unknown_agent_or_before_one_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(temp.path());

        let unknown = hub.move_agent(move_params(4, None)).unwrap_err();

        assert_eq!(unknown.code, ErrorCode::NotFound);
        assert!(unknown.message.contains('4'), "{}", unknown.message);
    }

    /// `agents.json` from before positions: every agent reads 0, and the list shows the newest
    /// first, as it shows new agents.
    #[test]
    fn a_saved_file_without_positions_loads_newest_first() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let state = StateDir::create(&folder).unwrap();
        let record = |id: u32| AgentRecord {
            id: AgentId::new(id),
            title: format!("Agent {id}"),
            folder: temp.path().to_owned(),
            session: None,
            model: None,
            effort: None,
            mode: None,
            running: false,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account: AccountId::DEFAULT,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        };
        AgentsFile {
            next_agent: AgentId::new(4),
            replay_floor: None,
            agents: vec![record(1), record(2), record(3)],
        }
        .save(&state.agents_file())
        .unwrap();
        let mut hub = Hub::for_tests(&folder);

        let loaded = snapshot_order(&hub);
        hub.move_agent(move_params(1, Some(2))).unwrap();

        assert_eq!(loaded, [3, 2, 1]);
        assert_eq!(snapshot_order(&hub), [3, 1, 2]);
    }

    /// A past session made into an agent is a new agent.
    #[tokio::test]
    async fn a_resumed_past_session_goes_on_top() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = hub(temp.path());
        start(&mut hub, temp.path(), "First");
        let id = ClaudeSessionId::new("11111111-2222-3333-4444-555555555555".to_owned());
        let found = FoundSession {
            session: PastSession {
                id: id.clone(),
                title: "Fix the cart".to_owned(),
                folder: temp.path().to_owned(),
                last_activity: Timestamp::from_unix_millis(0),
                turns: 1,
                account: AccountId::DEFAULT,
            },
            past: SessionPast {
                items: Vec::new(),
                file: PastFile {
                    session: id,
                    bytes: 0,
                },
            },
        };
        let (reply, mut answer) = oneshot::channel();

        hub.resume_session(found, reply);

        let started = ResponseBody::AgentStarted {
            agent: AgentId::new(2),
        };
        assert_eq!(answer.try_recv().unwrap(), ResponseResult::Ok(started));
        assert_eq!(order(&hub), [2, 1]);
    }
}

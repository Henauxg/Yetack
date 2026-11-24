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
    use std::time::{Duration, Instant};

    use cockpit_protocol::{
        AccountId, ClaudeSessionId, Effort, ErrorCode, Mark, PastSession, PermissionMode, Position,
        RenameParams, Request, StartParams, Timestamp,
    };
    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::hub::closed_choices::{Choices, UNDO_FOR};
    use crate::hub::{HubInput, state};
    use crate::process::STOP_TIMEOUT;
    use crate::sessions::{FoundSession, SessionPast};
    use crate::store::{AgentRecord, AgentsFile, PastFile, ResumeAs, StateDir};
    use crate::test_script;

    /// A hub whose `claude` reads its stdin and says nothing, and the inputs its tasks send it.
    fn hub(folder: &Path) -> (Hub, mpsc::Receiver<HubInput>) {
        let (mut hub, inputs) = Hub::for_tests_with_inputs(&folder.join("state"));
        let claude = folder.join("claude");
        test_script::write_script(&claude, "#!/bin/sh\nexec cat >/dev/null\n");
        hub.config.claude_program = claude;
        (hub, inputs)
    }

    async fn start(
        hub: &mut Hub,
        inputs: &mut mpsc::Receiver<HubInput>,
        folder: &Path,
        title: &str,
    ) -> AgentId {
        let params = StartParams {
            folder: folder.to_owned(),
            title: Some(title.to_owned()),
            model: None,
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        };
        match hub.ask(inputs, Request::AgentStart(params)).await {
            Ok(ResponseBody::AgentStarted { agent }) => agent,
            other => panic!("the agent did not start: {other:?}"),
        }
    }

    /// Resumes the past session [`SESSION`] in `folder`, whose file last records the mode
    /// `recorded`, handing the hub its inputs until it answers.
    async fn resume_session(
        hub: &mut Hub,
        inputs: &mut mpsc::Receiver<HubInput>,
        folder: &Path,
        asked_at: Instant,
        recorded: Recorded,
    ) -> Result<ResponseBody, ErrorBody> {
        let (reply, answered) = oneshot::channel();
        let found = past_session(folder, recorded);
        hub.resume_session(found, Ok(folder.to_owned()), asked_at, reply);
        hub.await_answer(inputs, answered).await
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
        let (mut hub, mut inputs) = hub(temp.path());
        for title in ["First", "Second", "Third"] {
            start(&mut hub, &mut inputs, temp.path(), title).await;
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
        let (mut hub, mut inputs) = hub(temp.path());
        for title in ["First", "Second", "Third"] {
            start(&mut hub, &mut inputs, temp.path(), title).await;
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
        let (mut hub, mut inputs) = hub(temp.path());
        for title in ["First", "Second"] {
            start(&mut hub, &mut inputs, temp.path(), title).await;
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

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";

    /// What a past session's file records of its last prompt.
    #[derive(Debug, Clone, Copy, Default)]
    struct Recorded {
        mode: Option<PermissionMode>,
        at: Option<Timestamp>,
    }

    impl Recorded {
        fn mode(mode: PermissionMode) -> Self {
            Self {
                mode: Some(mode),
                at: None,
            }
        }
    }

    /// The past session [`SESSION`] in `folder`, found on disk, whose file records `recorded`.
    fn past_session(folder: &Path, recorded: Recorded) -> FoundSession {
        let id = ClaudeSessionId::new(SESSION.to_owned());
        FoundSession {
            session: PastSession {
                id: id.clone(),
                title: "Fix the cart".to_owned(),
                folder: folder.to_owned(),
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
                mode: recorded.mode,
                last_prompt_at: recorded.at,
            },
        }
    }

    /// Closes an agent on [`SESSION`] that has `choices`, then resumes the session, asked `after`
    /// the close, from a file that records `recorded`. The new agent's choices.
    async fn close_then_resume(choices: Choices, after: Duration, recorded: Recorded) -> Choices {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = hub(temp.path());
        let first = start(&mut hub, &mut inputs, temp.path(), "First").await;
        if let Some(agent) = hub.agents.get_mut(&first) {
            agent.record.session = Some(ClaudeSessionId::new(SESSION.to_owned()));
            agent.record.model = choices.model;
            agent.record.effort = choices.effort;
            agent.record.mode = choices.mode;
        }
        hub.close(first).unwrap();
        let now = Instant::now();
        let asked_at = now.checked_add(after).unwrap_or(now);
        resume_session(&mut hub, &mut inputs, temp.path(), asked_at, recorded)
            .await
            .unwrap();
        let back = hub.agents.values().next().map(|agent| &agent.record);
        back.map(|record| Choices {
            model: record.model.clone(),
            effort: record.effort,
            mode: record.mode,
        })
        .unwrap_or_default()
    }

    /// Opus with a high effort, in `mode`.
    fn opus_in(mode: PermissionMode) -> Choices {
        Choices {
            model: Some("opus[1m]".to_owned()),
            effort: Some(Effort::High),
            mode: Some(mode),
        }
    }

    /// A later resume gets the closed agent's choices back, but for the bypass permissions mode,
    /// which only an undo brings back. The undo's time counts from the close to the resume
    /// request, not to the end of the session file's reading.
    #[tokio::test]
    async fn a_later_resume_gets_the_choices_back_but_not_the_bypass_mode() {
        let late = UNDO_FOR + Duration::from_secs(1);
        let bypass = opus_in(PermissionMode::BypassPermissions);

        let later =
            close_then_resume(opus_in(PermissionMode::Auto), late, Recorded::default()).await;
        let bypass_later = close_then_resume(bypass.clone(), late, Recorded::default()).await;
        let bypass_undone = close_then_resume(bypass, Duration::ZERO, Recorded::default()).await;

        assert_eq!(later, opus_in(PermissionMode::Auto));
        assert_eq!(bypass_later.mode, Some(PermissionMode::Default));
        assert_eq!(bypass_undone.mode, Some(PermissionMode::BypassPermissions));
    }

    /// An auto mode prompt the session file records `after_close` after now, the time of the close.
    fn auto_prompt_in(after_close: Duration) -> Recorded {
        let at = state::now()
            .unix_millis()
            .saturating_add(u64::try_from(after_close.as_millis()).unwrap_or(u64::MAX));
        Recorded {
            mode: Some(PermissionMode::Auto),
            at: Some(Timestamp::from_unix_millis(at)),
        }
    }

    /// A prompt the session file records after the close, as when the session went on in a
    /// terminal, is newer than the closed agent's choices: its mode wins, and the model and effort
    /// go back to the defaults. A prompt the closed `claude` wrote as it exited does not count,
    /// and an undo always gets the closed agent's choices.
    #[tokio::test]
    async fn a_prompt_after_the_close_wins_over_the_closed_agents_choices() {
        let late = UNDO_FOR + Duration::from_secs(1);
        let accept = opus_in(PermissionMode::AcceptEdits);
        let before = Recorded {
            mode: Some(PermissionMode::Auto),
            at: Some(Timestamp::from_unix_millis(0)),
        };

        let went_on = close_then_resume(
            accept.clone(),
            late,
            auto_prompt_in(Duration::from_secs(3_600)),
        )
        .await;
        let ended_here = close_then_resume(accept.clone(), late, before).await;
        let while_exiting =
            close_then_resume(accept.clone(), late, auto_prompt_in(STOP_TIMEOUT / 2)).await;
        let undone = close_then_resume(
            accept,
            Duration::ZERO,
            auto_prompt_in(Duration::from_secs(3_600)),
        )
        .await;

        assert_eq!(
            went_on,
            Choices {
                mode: Some(PermissionMode::Auto),
                ..Choices::default()
            }
        );
        assert_eq!(ended_here.mode, Some(PermissionMode::AcceptEdits));
        assert_eq!(while_exiting.mode, Some(PermissionMode::AcceptEdits));
        assert_eq!(undone, opus_in(PermissionMode::AcceptEdits));
    }

    /// An agent closed a moment ago comes back from its session with its choices, even after a
    /// resume that failed.
    #[tokio::test]
    async fn a_closed_agents_session_comes_back_with_its_choices() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = hub(temp.path());
        let first = start(&mut hub, &mut inputs, temp.path(), "First").await;
        if let Some(agent) = hub.agents.get_mut(&first) {
            agent.record.session = Some(ClaudeSessionId::new(SESSION.to_owned()));
            agent.record.model = Some("opus[1m]".to_owned());
            agent.record.effort = Some(Effort::High);
            agent.record.mode = Some(PermissionMode::Auto);
        }
        hub.close(first).unwrap();
        let claude = hub.config.claude_program.clone();
        hub.config.claude_program = temp.path().join("no-claude");
        let failed = resume_session(
            &mut hub,
            &mut inputs,
            temp.path(),
            Instant::now(),
            Recorded::default(),
        )
        .await;
        hub.config.claude_program = claude;

        resume_session(
            &mut hub,
            &mut inputs,
            temp.path(),
            Instant::now(),
            Recorded::default(),
        )
        .await
        .unwrap();
        assert!(failed.is_err(), "{failed:?}");
        let back = hub.agents.values().next().map(|agent| &agent.record);

        assert_eq!(
            back.map(|record| (record.model.as_deref(), record.effort, record.mode)),
            Some((
                Some("opus[1m]"),
                Some(Effort::High),
                Some(PermissionMode::Auto)
            ))
        );
    }

    /// A past session no agent of the daemon closed goes on in the mode its file last records,
    /// but for the bypass permissions and plan modes, and a closed agent's choice wins over the
    /// file's older one.
    #[tokio::test]
    async fn a_past_session_goes_on_in_the_mode_it_ended_in() {
        let mut started = Vec::new();
        for recorded in [
            PermissionMode::Auto,
            PermissionMode::Plan,
            PermissionMode::BypassPermissions,
            PermissionMode::Default,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (mut hub, mut inputs) = hub(temp.path());
            resume_session(
                &mut hub,
                &mut inputs,
                temp.path(),
                Instant::now(),
                Recorded::mode(recorded),
            )
            .await
            .unwrap();
            started.push(
                hub.agents
                    .values()
                    .next()
                    .and_then(|agent| agent.record.mode),
            );
        }
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = hub(temp.path());
        let first = start(&mut hub, &mut inputs, temp.path(), "First").await;
        if let Some(agent) = hub.agents.get_mut(&first) {
            agent.record.session = Some(ClaudeSessionId::new(SESSION.to_owned()));
            agent.record.mode = Some(PermissionMode::AcceptEdits);
        }
        hub.close(first).unwrap();
        resume_session(
            &mut hub,
            &mut inputs,
            temp.path(),
            Instant::now(),
            Recorded::mode(PermissionMode::Auto),
        )
        .await
        .unwrap();
        let undone = hub
            .agents
            .values()
            .next()
            .and_then(|agent| agent.record.mode);

        assert_eq!(
            started,
            [
                Some(PermissionMode::Auto),
                Some(PermissionMode::Default),
                Some(PermissionMode::Default),
                Some(PermissionMode::Default)
            ]
        );
        assert_eq!(undone, Some(PermissionMode::AcceptEdits));
    }

    /// A past session made into an agent is a new agent.
    #[tokio::test]
    async fn a_resumed_past_session_goes_on_top() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = hub(temp.path());
        start(&mut hub, &mut inputs, temp.path(), "First").await;

        let answer = resume_session(
            &mut hub,
            &mut inputs,
            temp.path(),
            Instant::now(),
            Recorded::default(),
        )
        .await;

        let started = ResponseBody::AgentStarted {
            agent: AgentId::new(2),
        };
        assert_eq!(answer, Ok(started));
        assert_eq!(order(&hub), [2, 1]);
    }
}

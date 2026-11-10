use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AccountId, AgentId, ClaudeSessionId, Effort, Mark, PermissionMode, Position, ProcessId, Seq,
    Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::store::atomic_file;
use crate::store::store_error::StoreError;

/// `agents.json`: the agents, and the counters that must survive a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentsFile {
    /// Ids are never reused, so a client that replays old events never mixes two agents.
    pub(crate) next_agent: AgentId,
    /// Events up to this one may be missing from the logs, since an agent's log goes with it:
    /// a client that saw less gets a snapshot instead of a replay.
    pub(crate) replay_floor: Option<Seq>,
    pub(crate) agents: Vec<AgentRecord>,
}

/// What the daemon needs to restart an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentRecord {
    pub(crate) id: AgentId,
    pub(crate) title: String,
    pub(crate) folder: PathBuf,
    pub(crate) session: Option<ClaudeSessionId>,
    /// `None` keeps Claude Code's default, as do `effort` and `mode`.
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<Effort>,
    pub(crate) mode: Option<PermissionMode>,
    /// False once a client stopped the agent: a restarted daemon resumes only running agents.
    pub(crate) running: bool,
    pub(crate) started_at: Timestamp,
    /// For an agent made from a past session: the part of the session file its transcript shows
    /// above its own items. A restarted daemon reads it again.
    #[serde(default)]
    pub(crate) past: Option<PastFile>,
    /// Where the agent shows in the list. A file saved before positions existed reads 0 for
    /// every agent, which lists them newest first.
    #[serde(default)]
    pub(crate) position: Position,
    /// The agent's `claude` while it runs, so that a daemon started after a crash stops the one
    /// its predecessor left. `None` once it exited, and in a file saved before it existed.
    #[serde(default)]
    pub(crate) process: Option<RecordedProcess>,
    /// The account the agent runs with. The default account in a file saved before accounts
    /// existed.
    #[serde(default)]
    pub(crate) account: AccountId,
    /// `Unmarked` in a file saved before marks existed.
    #[serde(default)]
    pub(crate) mark: Mark,
    /// `Fork` for an agent forked from another one's session, until its first start gives it a
    /// session of its own. `Same` in a file saved before forks existed.
    #[serde(default)]
    pub(crate) resume_as: ResumeAs,
}

/// What the next start of an agent with a session does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResumeAs {
    /// Carries on with the session.
    #[default]
    Same,
    /// Starts a new session from a copy of the conversation (`--fork-session`).
    Fork,
}

/// A process, told apart from a later one that reuses its id by when it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RecordedProcess {
    pub(crate) pid: ProcessId,
    /// In the system's own unit: clock ticks after boot on Linux, 100 ns units since 1601 on
    /// Windows.
    pub(crate) started: u64,
    /// The boot the process ran in, on Linux, where a start time counts from the boot. `None` on
    /// Windows, whose start times are dates, and in a file saved before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) boot: Option<BootId>,
}

/// One boot of the machine: Linux's `boot_id`, a random UUID the kernel draws at each boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct BootId(String);

impl BootId {
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "Windows records no boot")
    )]
    pub(crate) fn new(id: String) -> Self {
        Self(id)
    }
}

/// The start of a session file, as it was when an agent took the session over. Claude Code only
/// appends to the file, so these bytes do not change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PastFile {
    pub(crate) session: ClaudeSessionId,
    pub(crate) bytes: u64,
}

impl Default for AgentsFile {
    fn default() -> Self {
        Self {
            next_agent: AgentId::new(1),
            replay_floor: None,
            agents: Vec::new(),
        }
    }
}

impl AgentsFile {
    pub(crate) fn load(path: &Path) -> Result<Self, StoreError> {
        atomic_file::read_json(path)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<(), StoreError> {
        atomic_file::write_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// `agents.json` as the daemon writes it. No field is `null`, so a field that no longer reads
    /// back fails the test.
    const AGENTS_JSON: &str = r#"{
  "next_agent": 3,
  "replay_floor": 17,
  "agents": [
    {
      "id": 2,
      "title": "Fix the parser",
      "folder": "/srv/work/parser",
      "session": "session-a",
      "model": "opus",
      "effort": "high",
      "mode": "acceptEdits",
      "running": true,
      "started_at": 1000,
      "past": {
        "session": "session-a",
        "bytes": 52000
      },
      "position": 2048,
      "process": {
        "pid": 4242,
        "started": 987654,
        "boot": "6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b"
      },
      "account": 5,
      "mark": "to_handle",
      "resume_as": "fork"
    }
  ]
}
"#;

    fn sample() -> AgentsFile {
        AgentsFile {
            next_agent: AgentId::new(3),
            replay_floor: Some(Seq::new(17)),
            agents: vec![AgentRecord {
                id: AgentId::new(2),
                title: "Fix the parser".to_owned(),
                folder: PathBuf::from("/srv/work/parser"),
                session: Some(ClaudeSessionId::new("session-a".to_owned())),
                model: Some("opus".to_owned()),
                effort: Some(Effort::High),
                mode: Some(PermissionMode::AcceptEdits),
                running: true,
                started_at: Timestamp::from_unix_millis(1_000),
                past: Some(PastFile {
                    session: ClaudeSessionId::new("session-a".to_owned()),
                    bytes: 52_000,
                }),
                position: Position::new(2048),
                process: Some(RecordedProcess {
                    pid: ProcessId::new(4242),
                    started: 987_654,
                    boot: Some(BootId::new(
                        "6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b".to_owned(),
                    )),
                }),
                account: AccountId::new(5),
                mark: Mark::ToHandle,
                resume_as: ResumeAs::Fork,
            }],
        }
    }

    #[test]
    fn reads_the_saved_format() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        std::fs::write(&path, AGENTS_JSON).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        assert_eq!(file, sample());
    }

    #[test]
    fn an_agent_saved_without_a_past_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let without_past = AGENTS_JSON.replace(
            r#",
      "past": {
        "session": "session-a",
        "bytes": 52000
      }"#,
            "",
        );
        assert_ne!(without_past, AGENTS_JSON);
        std::fs::write(&path, without_past).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        assert_eq!(file.agents[0].past, None);
    }

    #[test]
    fn an_agent_saved_before_marks_loads_unmarked() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let without_mark = AGENTS_JSON.replace(",\n      \"mark\": \"to_handle\"", "");
        assert_ne!(without_mark, AGENTS_JSON);
        std::fs::write(&path, without_mark).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        assert_eq!(file.agents[0].mark, Mark::Unmarked);
    }

    #[test]
    fn an_agent_saved_without_a_process_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let without_process = AGENTS_JSON.replace(
            r#",
      "process": {
        "pid": 4242,
        "started": 987654,
        "boot": "6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b"
      }"#,
            "",
        );
        assert_ne!(without_process, AGENTS_JSON);
        std::fs::write(&path, without_process).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        assert_eq!(file.agents[0].process, None);
    }

    #[test]
    fn a_process_saved_without_its_boot_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let without_boot = AGENTS_JSON.replace(
            r#",
        "boot": "6e1f0c2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b""#,
            "",
        );
        assert_ne!(without_boot, AGENTS_JSON);
        std::fs::write(&path, without_boot).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        let process = file.agents[0].process.as_ref().unwrap();
        assert_eq!(process.started, 987_654);
        assert_eq!(process.boot, None);
    }

    #[test]
    fn an_agent_saved_without_an_account_runs_with_the_default_one() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let without_account = AGENTS_JSON.replace(
            r#",
      "account": 5"#,
            "",
        );
        assert_ne!(without_account, AGENTS_JSON);
        std::fs::write(&path, without_account).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        assert_eq!(file.agents[0].account, AccountId::DEFAULT);
    }

    #[test]
    fn an_agent_saved_without_a_position_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let without_position = AGENTS_JSON.replace(
            r#",
      "position": 2048,"#,
            ",",
        );
        assert_ne!(without_position, AGENTS_JSON);
        std::fs::write(&path, without_position).unwrap();

        let file = AgentsFile::load(&path).unwrap();

        assert_eq!(file.agents[0].position, Position::new(0));
    }

    #[test]
    fn writes_the_saved_format() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");

        sample().save(&path).unwrap();

        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let expected: Value = serde_json::from_str(AGENTS_JSON).unwrap();
        assert_eq!(written, expected);
    }
}

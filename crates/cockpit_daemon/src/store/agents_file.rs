use std::path::{Path, PathBuf};

use cockpit_protocol::{AgentId, ClaudeSessionId, Effort, PermissionMode, Seq, Timestamp};
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
      "started_at": 1000
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
    fn writes_the_saved_format() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");

        sample().save(&path).unwrap();

        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let expected: Value = serde_json::from_str(AGENTS_JSON).unwrap();
        assert_eq!(written, expected);
    }
}

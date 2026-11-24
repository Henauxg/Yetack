//! The model, effort and permission mode of the agents closed before, by session. A new agent on
//! the session gets them back: from the app's undo of a close, or from the past sessions. They
//! live in `closed-sessions.json` in the state folder, so they outlive the daemon.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cockpit_protocol::{AccountId, ClaudeSessionId, Effort, PermissionMode, Timestamp};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::hub::errors;
use crate::store::{self, AgentRecord, ResumeAs};

/// How long after a close a resume counts as an undo, which brings back even the bypass
/// permissions mode. The app's undo lasts 8 s.
pub(super) const UNDO_FOR: Duration = Duration::from_secs(30);

/// The most closed sessions whose choices the daemon keeps. The oldest goes first.
const MAX_KEPT: usize = 500;

/// What a client chose for an agent. `None` keeps Claude Code's default, as in [`AgentRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Choices {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kept {
    session: ClaudeSessionId,
    account: AccountId,
    #[serde(flatten)]
    choices: Choices,
    /// The close, on the wall clock: the file keeps it.
    closed_at: Timestamp,
    /// The close, on this daemon's monotonic clock, for an undo. A close an earlier daemon saved
    /// has none.
    #[serde(skip)]
    closed_instant: Option<Instant>,
}

/// The choices of a session's last closed agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub(super) choices: Choices,
    pub(super) closed_at: Timestamp,
    /// The resume came within [`UNDO_FOR`] of the close.
    pub(super) undo: bool,
}

/// The content of `closed-sessions.json`, oldest close first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ClosedFile {
    #[serde(default, deserialize_with = "readable_sessions")]
    sessions: VecDeque<Kept>,
}

/// The sessions that read. One that does not, as one a newer daemon wrote with a mode this one
/// does not know, is left out, so that the next save does not lose the others.
fn readable_sessions<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<VecDeque<Kept>, D::Error> {
    let sessions: Vec<Value> = Vec::deserialize(deserializer)?;
    Ok(sessions
        .into_iter()
        .filter_map(|session| serde_json::from_value(session).ok())
        .collect())
}

/// The choices of the agents closed last, by their session and account.
#[derive(Debug, Default)]
pub(crate) struct ClosedChoices {
    kept: ClosedFile,
    /// Where they are saved. None keeps them in memory only.
    file: Option<PathBuf>,
}

impl ClosedChoices {
    /// The choices `file` holds. A file that does not read gives none, and is written again at the
    /// next close.
    pub(crate) fn load(file: PathBuf) -> Self {
        let kept = store::read_json::<ClosedFile>(&file).unwrap_or_else(|error| {
            tracing::warn!(error = %errors::chain(&error), "cannot read the closed sessions");
            ClosedFile::default()
        });
        Self {
            kept,
            file: Some(file),
        }
    }

    /// Keeps the choices of an agent that closes. `now` is the close on the monotonic clock, and
    /// `now_utc` on the wall clock, which the file keeps. An agent with no session cannot come
    /// back.
    pub(super) fn keep(&mut self, record: &AgentRecord, now: Instant, now_utc: Timestamp) {
        let Some(session) = &record.session else {
            return;
        };
        // A fork that has not started its own session names its original's, whose choices these
        // are not.
        if record.resume_as == ResumeAs::Fork {
            return;
        }
        let sessions = &mut self.kept.sessions;
        sessions.retain(|kept| (&kept.session, kept.account) != (session, record.account));
        // A file another version wrote may hold more.
        while sessions.len() >= MAX_KEPT {
            sessions.pop_front();
        }
        sessions.push_back(Kept {
            session: session.clone(),
            account: record.account,
            choices: Choices {
                model: record.model.clone(),
                effort: record.effort,
                mode: record.mode,
            },
            closed_at: now_utc,
            closed_instant: Some(now),
        });
        self.save();
    }

    /// The choices of the session's last closed agent, for a resume asked at `asked_at`.
    pub(super) fn find(
        &self,
        session: &ClaudeSessionId,
        account: AccountId,
        asked_at: Instant,
    ) -> Option<Found> {
        let kept = self
            .kept
            .sessions
            .iter()
            .find(|kept| &kept.session == session && kept.account == account)?;
        let undo = kept
            .closed_instant
            .is_some_and(|closed| asked_at.saturating_duration_since(closed) <= UNDO_FOR);
        Some(Found {
            choices: kept.choices.clone(),
            closed_at: kept.closed_at,
            undo,
        })
    }

    /// Drops the session's choices, once a new agent took them.
    pub(super) fn forget(&mut self, session: &ClaudeSessionId, account: AccountId) {
        let sessions = &mut self.kept.sessions;
        let before = sessions.len();
        sessions.retain(|kept| (&kept.session, kept.account) != (session, account));
        if sessions.len() != before {
            self.save();
        }
    }

    /// Writes the file without waiting for the disk, at each close: a crash of the machine may
    /// lose the last closes, and their resumes then start with the defaults.
    fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let written = serde_json::to_vec(&self.kept)
            .map_err(io::Error::from)
            .and_then(|bytes| store::write_bytes_unsynced(file, &bytes));
        if let Err(error) = written {
            tracing::warn!(%error, "cannot save the closed sessions");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{AgentId, Mark, Position};

    use super::*;

    fn record(session: &str, account: AccountId, mode: PermissionMode) -> AgentRecord {
        AgentRecord {
            id: AgentId::new(1),
            title: "Fix the cart".to_owned(),
            folder: PathBuf::from("/srv/projects/shop"),
            session: Some(ClaudeSessionId::new(session.to_owned())),
            model: Some("opus[1m]".to_owned()),
            effort: Some(Effort::High),
            mode: Some(mode),
            running: true,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        }
    }

    fn session(name: &str) -> ClaudeSessionId {
        ClaudeSessionId::new(name.to_owned())
    }

    fn closed_at() -> Timestamp {
        Timestamp::from_unix_millis(1_000)
    }

    fn mode(found: Option<Found>) -> Option<PermissionMode> {
        found.and_then(|found| found.choices.mode)
    }

    #[test]
    fn a_closed_agents_choices_come_back_until_forgotten() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
            closed_at(),
        );

        let found = closed.find(&session("session-1"), AccountId::DEFAULT, now);
        closed.forget(&session("session-1"), AccountId::DEFAULT);
        let again = closed.find(&session("session-1"), AccountId::DEFAULT, now);

        assert_eq!(
            found,
            Some(Found {
                choices: Choices {
                    model: Some("opus[1m]".to_owned()),
                    effort: Some(Effort::High),
                    mode: Some(PermissionMode::Auto),
                },
                closed_at: closed_at(),
                undo: true,
            })
        );
        assert_eq!(again, None);
    }

    #[test]
    fn the_choices_stay_after_the_undo_time() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
            closed_at(),
        );
        let soon = now.checked_add(UNDO_FOR).unwrap_or(now);
        let late = soon.checked_add(Duration::from_secs(1)).unwrap_or(soon);

        let in_time = closed.find(&session("session-1"), AccountId::DEFAULT, soon);
        let later = closed.find(&session("session-1"), AccountId::DEFAULT, late);

        assert_eq!(in_time.map(|found| found.undo), Some(true));
        assert_eq!(
            later.map(|found| (found.undo, found.choices.mode)),
            Some((false, Some(PermissionMode::Auto)))
        );
    }

    #[test]
    fn the_choices_outlive_the_daemon() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("closed-sessions.json");
        let now = Instant::now();
        let mut closed = ClosedChoices::load(file.clone());
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
            closed_at(),
        );

        let next_daemon = ClosedChoices::load(file);
        let found = next_daemon.find(&session("session-1"), AccountId::DEFAULT, now);

        assert_eq!(
            found.map(|found| (found.undo, found.closed_at, found.choices.mode)),
            Some((false, closed_at(), Some(PermissionMode::Auto))),
            "a close an earlier daemon saw is no undo"
        );
    }

    /// A file an older daemon wrote still reads: this test pins its words.
    #[test]
    fn a_saved_file_reads_with_its_words() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("closed-sessions.json");
        let saved = r#"{
          "sessions": [
            {
              "session": "session-1",
              "account": 2,
              "model": "opus[1m]",
              "effort": "high",
              "mode": "acceptEdits",
              "closed_at": 1795182569059
            }
          ]
        }"#;
        std::fs::write(&file, saved).unwrap();

        let closed = ClosedChoices::load(file);

        assert_eq!(
            closed.find(&session("session-1"), AccountId::new(2), Instant::now()),
            Some(Found {
                choices: Choices {
                    model: Some("opus[1m]".to_owned()),
                    effort: Some(Effort::High),
                    mode: Some(PermissionMode::AcceptEdits),
                },
                closed_at: Timestamp::from_unix_millis(1_795_182_569_059),
                undo: false,
            })
        );
    }

    /// A session a newer daemon saved with a mode this one does not know is left out, and the
    /// next save keeps the others.
    #[test]
    fn a_session_that_does_not_read_leaves_the_others() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("closed-sessions.json");
        let saved = r#"{"sessions": [
            {"session": "session-1", "account": 0, "mode": "someNewMode", "closed_at": 1000},
            {"session": "session-2", "account": 0, "mode": "auto", "closed_at": 2000}
        ]}"#;
        std::fs::write(&file, saved).unwrap();
        let now = Instant::now();

        let mut closed = ClosedChoices::load(file.clone());
        closed.keep(
            &record("session-3", AccountId::DEFAULT, PermissionMode::Plan),
            now,
            closed_at(),
        );
        let next_daemon = ClosedChoices::load(file);

        assert_eq!(
            mode(next_daemon.find(&session("session-2"), AccountId::DEFAULT, now)),
            Some(PermissionMode::Auto)
        );
        assert_eq!(
            mode(next_daemon.find(&session("session-3"), AccountId::DEFAULT, now)),
            Some(PermissionMode::Plan)
        );
    }

    #[test]
    fn an_unreadable_file_gives_no_choices() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("closed-sessions.json");
        std::fs::write(&file, "not json").unwrap();

        let closed = ClosedChoices::load(file);

        assert_eq!(
            closed.find(&session("session-1"), AccountId::DEFAULT, Instant::now()),
            None
        );
    }

    #[test]
    fn each_account_and_the_latest_close_of_a_session_count() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        let other = AccountId::new(2);
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
            closed_at(),
        );
        closed.keep(
            &record("session-1", other, PermissionMode::Plan),
            now,
            closed_at(),
        );
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::AcceptEdits),
            now,
            closed_at(),
        );

        let default_account = closed.find(&session("session-1"), AccountId::DEFAULT, now);
        let other_account = closed.find(&session("session-1"), other, now);

        assert_eq!(mode(default_account), Some(PermissionMode::AcceptEdits));
        assert_eq!(mode(other_account), Some(PermissionMode::Plan));
    }

    #[test]
    fn a_fork_without_its_own_session_keeps_no_choices() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        let mut fork = record("session-1", AccountId::DEFAULT, PermissionMode::Plan);
        fork.resume_as = ResumeAs::Fork;

        closed.keep(&fork, now, closed_at());

        assert_eq!(
            closed.find(&session("session-1"), AccountId::DEFAULT, now),
            None,
            "the session is its original's"
        );
    }

    #[test]
    fn the_oldest_choices_go_past_the_limit() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        for index in 0..=MAX_KEPT {
            let name = format!("session-{index}");
            closed.keep(
                &record(&name, AccountId::DEFAULT, PermissionMode::Auto),
                now,
                closed_at(),
            );
        }

        let oldest = closed.find(&session("session-0"), AccountId::DEFAULT, now);
        let newest = closed.find(
            &session(&format!("session-{MAX_KEPT}")),
            AccountId::DEFAULT,
            now,
        );

        assert_eq!(oldest, None);
        assert_eq!(mode(newest), Some(PermissionMode::Auto));
    }
}

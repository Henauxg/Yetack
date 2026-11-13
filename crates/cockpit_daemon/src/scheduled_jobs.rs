//! The scheduled jobs an agent may hold. Claude Code keeps a job made with `CronCreate` in its
//! `claude` process, so a restart of the daemon loses it. The app reads the raw logs of this
//! computer's daemon before it restarts it, and `contrib/update-daemon.sh` applies the same rule
//! on a server.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::time::SystemTime;

use claude_protocol::{ContentBlock, OutputLine};
use cockpit_protocol::{AgentId, Timestamp};

use crate::store::{self, Direction, LoggedLine};

const CREATE_TOOL: &str = "CronCreate";
const DELETE_TOOL: &str = "CronDelete";
/// Both tools' names start with it: a line without it calls neither, and is not parsed.
const TOOLS_PREFIX: &str = "Cron";

/// Whether `agent` of the state folder `state_dir` may hold a scheduled job: Claude called
/// `CronCreate` more times than `CronDelete` in the agent's raw log since `since`, the start of
/// the daemon that runs it. An agent without a raw log holds none.
///
/// It can only say "may": a job that runs once ends by itself, with no `CronDelete`. It reads the
/// whole log: it blocks.
pub fn may_hold_scheduled_job(
    state_dir: &Path,
    agent: AgentId,
    since: SystemTime,
) -> io::Result<bool> {
    let file = match File::open(store::raw_log_path(state_dir, agent)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let since = Timestamp::from_system_time(since).unwrap_or(Timestamp::from_unix_millis(0));
    let mut held: i64 = 0;
    for line in BufReader::new(file).split(b'\n') {
        let bytes = line?;
        let line = String::from_utf8_lossy(&bytes);
        if !line.contains(TOOLS_PREFIX) {
            continue;
        }
        // A line cut short, such as the one the daemon is writing, says nothing.
        let Ok(logged) = serde_json::from_str::<LoggedLine>(&line) else {
            continue;
        };
        if logged.at < since || logged.direction != Direction::FromClaude {
            continue;
        }
        let Ok(OutputLine::Assistant(message)) = logged.line.parse() else {
            continue;
        };
        for block in &message.message.content {
            if let ContentBlock::ToolUse(call) = block {
                match call.name.as_str() {
                    CREATE_TOOL => held = held.saturating_add(1),
                    DELETE_TOOL => held = held.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }
    Ok(held > 0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::store::RawLog;

    fn agent() -> AgentId {
        AgentId::new(3)
    }

    /// A state folder with `agent()`'s raw log.
    struct Logged {
        temp: TempDir,
        log: RawLog,
    }

    impl Logged {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let path = store::raw_log_path(temp.path(), agent());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let log = RawLog::open(&path).unwrap();
            Self { temp, log }
        }

        fn call(&mut self, at: u64, direction: Direction, tool: &str) {
            let line = format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_made_up","name":"{tool}","input":{{}}}}]}}}}"#
            );
            self.log
                .append(Timestamp::from_unix_millis(at), direction, &line);
        }

        fn holds_since(&self, millis: u64) -> bool {
            let since = SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
            may_hold_scheduled_job(self.temp.path(), agent(), since).unwrap()
        }
    }

    #[test]
    fn a_job_created_and_not_deleted_may_be_held() {
        let mut logged = Logged::new();
        logged.call(1_000, Direction::FromClaude, CREATE_TOOL);
        assert!(logged.holds_since(0));

        logged.call(2_000, Direction::FromClaude, DELETE_TOOL);
        assert!(!logged.holds_since(0));
    }

    #[test]
    fn a_job_from_before_the_daemon_started_is_gone() {
        let mut logged = Logged::new();
        logged.call(1_000, Direction::FromClaude, CREATE_TOOL);
        logged.call(5_000, Direction::FromClaude, CREATE_TOOL);
        logged.call(6_000, Direction::FromClaude, DELETE_TOOL);

        assert!(!logged.holds_since(5_000));
        assert!(logged.holds_since(0));
    }

    #[test]
    fn only_claudes_own_calls_count() {
        let mut logged = Logged::new();
        logged.call(1_000, Direction::ToClaude, CREATE_TOOL);
        logged.call(1_000, Direction::Stderr, CREATE_TOOL);
        logged.call(1_000, Direction::FromClaude, "CronList");
        logged.log.append(
            Timestamp::from_unix_millis(1_000),
            Direction::FromClaude,
            r#"{"type":"control_response","response":{"toolCallsByType":[{"name":"CronCreate","callTokens":1}]}}"#,
        );

        assert!(!logged.holds_since(0));
    }

    #[test]
    fn a_line_cut_short_is_skipped() {
        let mut logged = Logged::new();
        logged.call(1_000, Direction::FromClaude, CREATE_TOOL);
        let path = store::raw_log_path(logged.temp.path(), agent());
        let mut text = std::fs::read(&path).unwrap();
        text.extend_from_slice(br#"{"at":2000,"direction":"from_claude","line":"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"CronDel"#);
        std::fs::write(&path, text).unwrap();

        assert!(logged.holds_since(0));
    }

    #[test]
    fn an_agent_without_a_log_holds_nothing() {
        let temp = tempfile::tempdir().unwrap();

        let held = may_hold_scheduled_job(temp.path(), agent(), SystemTime::UNIX_EPOCH);

        assert!(!held.unwrap());
    }
}

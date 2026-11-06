use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use claude_protocol::PermissionMode;
use cockpit_protocol::ClaudeSessionId;
use tokio::process::Command;

const STREAM_JSON: &str = "stream-json";
/// JSON in and out, approvals over stdio, and prompts echoed back.
const FIXED_ARGS: [&str; 8] = [
    "--output-format",
    STREAM_JSON,
    "--verbose",
    "--input-format",
    STREAM_JSON,
    "--permission-prompt-tool",
    "stdio",
    "--replay-user-messages",
];
const MODEL_FLAG: &str = "--model";
const PERMISSION_MODE_FLAG: &str = "--permission-mode";
const EFFORT_FLAG: &str = "--effort";
const RESUME_FLAG: &str = "--resume";

/// Set by a Claude Code session for the commands it runs: a `claude` that sees it thinks it runs
/// inside another session.
const NESTED_SESSION_ENV: &str = "CLAUDECODE";
const WORKING_FOLDER_ENV: &str = "PWD";
/// Makes a resumed session carry on with a turn the daemon's restart cut off.
const RESUME_INTERRUPTED_TURN_ENV: &str = "CLAUDE_CODE_RESUME_INTERRUPTED_TURN";
const ENABLED: &str = "1";

/// What a resumed session does with a turn that was cut off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterruptedTurn {
    /// The daemon resumes its agents as it starts: the turn its stop cut off carries on.
    Resume,
    /// A user resumes the agent, which a user's stop may have cut off: the turn stays cut.
    Leave,
}

/// How to start `claude` for one agent. A prompt never goes on the command line: other users
/// can read it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeCommand {
    pub(crate) program: PathBuf,
    pub(crate) folder: PathBuf,
    pub(crate) model: Option<String>,
    pub(crate) mode: Option<PermissionMode>,
    /// The level as `--effort` takes it.
    pub(crate) effort: Option<String>,
    /// The Claude session to resume.
    pub(crate) resume: Option<ClaudeSessionId>,
    pub(crate) interrupted_turn: InterruptedTurn,
}

impl ClaudeCommand {
    /// Values from clients go after `=`, so one starting with `-` cannot become an option.
    pub(crate) fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = FIXED_ARGS.iter().map(OsString::from).collect();
        if let Some(model) = &self.model {
            args.push(flag(MODEL_FLAG, model));
        }
        if let Some(mode) = self.mode {
            args.push(flag(PERMISSION_MODE_FLAG, &mode.to_string()));
        }
        if let Some(effort) = &self.effort {
            args.push(flag(EFFORT_FLAG, effort));
        }
        if let Some(session) = &self.resume {
            args.push(flag(RESUME_FLAG, session.as_str()));
        }
        args
    }

    /// On Unix, `claude` leads a process group of its own, so that killing the group also ends
    /// its MCP servers and the commands it started.
    pub(crate) fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(self.args())
            .current_dir(&self.folder)
            .env_remove(NESTED_SESSION_ENV)
            .env(WORKING_FOLDER_ENV, &self.folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        if self.resume.is_some() && self.interrupted_turn == InterruptedTurn::Resume {
            command.env(RESUME_INTERRUPTED_TURN_ENV, ENABLED);
        }
        command
    }
}

fn flag(name: &str, value: &str) -> OsString {
    OsString::from(format!("{name}={value}"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn command() -> ClaudeCommand {
        ClaudeCommand {
            program: PathBuf::from("claude"),
            folder: PathBuf::from("/srv/work/shop"),
            model: None,
            mode: None,
            effort: None,
            resume: None,
            interrupted_turn: InterruptedTurn::Leave,
        }
    }

    #[test]
    fn a_new_agent_gets_only_the_fixed_arguments() {
        let args = command().args();

        assert_eq!(args, FIXED_ARGS.map(OsString::from));
    }

    #[test]
    fn options_carry_their_value_after_an_equals_sign() {
        let command = ClaudeCommand {
            model: Some("opus".to_owned()),
            mode: Some(PermissionMode::AcceptEdits),
            effort: Some("high".to_owned()),
            resume: Some(ClaudeSessionId::new("session-1".to_owned())),
            ..command()
        };

        let args = command.args();

        assert_eq!(
            args.get(FIXED_ARGS.len()..).unwrap(),
            [
                "--model=opus",
                "--permission-mode=acceptEdits",
                "--effort=high",
                "--resume=session-1"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn a_value_that_looks_like_an_option_stays_a_value() {
        let command = ClaudeCommand {
            model: Some("-p".to_owned()),
            resume: Some(ClaudeSessionId::new(
                "--dangerously-skip-permissions".to_owned(),
            )),
            ..command()
        };

        let args = command.args();

        assert_eq!(
            args.get(FIXED_ARGS.len()..).unwrap(),
            ["--model=-p", "--resume=--dangerously-skip-permissions"].map(OsString::from)
        );
        assert!(!args.contains(&OsString::from("-p")));
    }

    #[test]
    fn only_the_daemons_own_resume_carries_on_with_a_cut_turn() {
        let session = Some(ClaudeSessionId::new("session-1".to_owned()));
        let carries_on = |resume: Option<ClaudeSessionId>, interrupted_turn| {
            let command = ClaudeCommand {
                resume,
                interrupted_turn,
                ..command()
            }
            .to_command();
            let envs: Vec<(&OsStr, Option<&OsStr>)> = command.as_std().get_envs().collect();
            envs.contains(&(
                OsStr::new(RESUME_INTERRUPTED_TURN_ENV),
                Some(OsStr::new(ENABLED)),
            ))
        };

        assert!(carries_on(session.clone(), InterruptedTurn::Resume));
        assert!(!carries_on(session, InterruptedTurn::Leave));
        assert!(!carries_on(None, InterruptedTurn::Resume));
    }
}

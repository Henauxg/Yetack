use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;

use claude_protocol::PermissionMode;
use cockpit_protocol::ClaudeSessionId;
use tokio::process::Command;

use crate::accounts::ClaudeEnv;
use crate::process::program::{self, ClaudeProgram};
use crate::store::ResumeAs;

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
const FORK_SESSION_FLAG: &str = "--fork-session";

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
    /// With `Fork`, the resumed conversation goes on under a new session.
    pub(crate) resume_as: ResumeAs,
    pub(crate) interrupted_turn: InterruptedTurn,
    /// What the agent's account sets in the environment.
    pub(crate) env: ClaudeEnv,
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
            if self.resume_as == ResumeAs::Fork {
                args.push(OsString::from(FORK_SESSION_FLAG));
            }
        }
        args
    }

    /// On Unix, `claude` leads a process group of its own, so that killing the group also ends
    /// its MCP servers and the commands it started.
    /// `program` is the one `self.program` names, from [`program::resolve`].
    pub(crate) fn to_command(&self, program: &ClaudeProgram) -> io::Result<Command> {
        let mut command = program::resolved_command(program, &self.args())?;
        command
            .current_dir(&self.folder)
            .env_remove(NESTED_SESSION_ENV)
            .env(WORKING_FOLDER_ENV, &self.folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        self.env.apply(&mut command);
        if self.resume.is_some() && self.interrupted_turn == InterruptedTurn::Resume {
            command.env(RESUME_INTERRUPTED_TURN_ENV, ENABLED);
        }
        Ok(command)
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
            resume_as: ResumeAs::Same,
            interrupted_turn: InterruptedTurn::Leave,
            env: ClaudeEnv::for_tests(),
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
    fn a_fork_resumes_under_a_new_session() {
        let command = ClaudeCommand {
            resume: Some(ClaudeSessionId::new("session-1".to_owned())),
            resume_as: ResumeAs::Fork,
            ..command()
        };

        let args = command.args();

        assert_eq!(
            args.get(FIXED_ARGS.len()..).unwrap(),
            ["--resume=session-1", "--fork-session"].map(OsString::from)
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
            let claude = ClaudeCommand {
                resume,
                interrupted_turn,
                ..command()
            };
            let command = claude
                .to_command(&program::resolve(&claude.program))
                .unwrap();
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

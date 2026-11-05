//! The command line's words.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use cockpit_client::{BRIDGE_SUBCOMMAND, Transport};
use cockpit_protocol::{Effort, PermissionMode, PlanDecision};

const DEFAULT_REMOTE_COMMAND: &str = "agents-cockpit";

#[derive(Debug, Parser)]
#[command(
    name = "agents-cockpit",
    version,
    about = "Run Claude Code agents in a daemon, and steer them"
)]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) target: Target,
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Which daemon a command serves or reaches.
#[derive(Debug, Args)]
pub(crate) struct Target {
    /// The daemon's state folder [default: $XDG_STATE_HOME/agents-cockpit]
    #[arg(long, global = true)]
    pub(crate) state_dir: Option<PathBuf>,
    /// Reach the daemon on this host over SSH, through its bridge
    #[arg(long, global = true, conflicts_with = "state_dir")]
    pub(crate) ssh: Option<String>,
    /// The absolute path of agents-cockpit on the SSH host
    #[arg(long, global = true, default_value = DEFAULT_REMOTE_COMMAND)]
    pub(crate) remote_command: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run the daemon in the foreground
    Serve,
    /// Copy bytes between stdin and stdout and the daemon's socket, for
    /// `ssh <host> agents-cockpit bridge`
    #[command(name = BRIDGE_SUBCOMMAND)]
    Bridge,
    /// List the agents
    List,
    /// Start an agent in a folder, and print its id
    Start {
        folder: PathBuf,
        /// The first prompt; `-` reads it from stdin
        prompt: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, value_enum)]
        mode: Option<ModeArg>,
        #[arg(long, value_enum)]
        effort: Option<EffortArg>,
    },
    /// Send a prompt; `-` reads it from stdin
    Prompt { agent: u32, text: String },
    /// Answer an approval, a question or a plan
    Answer {
        agent: u32,
        /// The pending id the approval, question or plan carries
        pending: String,
        #[command(subcommand)]
        answer: AnswerCommand,
    },
    /// Stop the current turn
    Interrupt { agent: u32 },
    /// End an agent's claude process
    Stop { agent: u32 },
    /// Print events as JSON lines, of one agent or of all
    Tail { agent: Option<u32> },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AnswerCommand {
    /// Let the tool call run
    Allow,
    /// Refuse the tool call
    Deny {
        /// What Claude should do instead
        #[arg(long)]
        message: Option<String>,
    },
    /// Answer the questions, one answer each, in order: an option's label or your own text
    Question {
        #[arg(required = true)]
        answers: Vec<String>,
    },
    /// Answer a plan
    Plan {
        #[arg(value_enum)]
        decision: PlanArg,
        /// With keep-planning, what to change in the plan
        #[arg(long)]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ModeArg {
    Default,
    AcceptEdits,
    Plan,
    Auto,
    DontAsk,
    BypassPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EffortArg {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PlanArg {
    AcceptEdits,
    AskBeforeEdits,
    KeepPlanning,
}

impl Target {
    pub(crate) fn is_remote(&self) -> bool {
        self.ssh.is_some()
    }

    /// The socket of the daemon on this machine.
    pub(crate) fn local_socket(&self) -> anyhow::Result<PathBuf> {
        let state_dir = match &self.state_dir {
            Some(state_dir) => state_dir.clone(),
            None => cockpit_daemon::default_state_dir().context("HOME is not set")?,
        };
        Ok(cockpit_daemon::socket_path(&state_dir))
    }

    pub(crate) fn transport(&self) -> anyhow::Result<Transport> {
        match &self.ssh {
            Some(host) => Ok(Transport::Ssh {
                host: host.clone(),
                remote_command: self.remote_command.clone(),
            }),
            None => Ok(Transport::Local {
                socket: self.local_socket()?,
            }),
        }
    }
}

impl ModeArg {
    pub(crate) fn to_protocol(self) -> PermissionMode {
        match self {
            Self::Default => PermissionMode::Default,
            Self::AcceptEdits => PermissionMode::AcceptEdits,
            Self::Plan => PermissionMode::Plan,
            Self::Auto => PermissionMode::Auto,
            Self::DontAsk => PermissionMode::DontAsk,
            Self::BypassPermissions => PermissionMode::BypassPermissions,
        }
    }
}

impl EffortArg {
    pub(crate) fn to_protocol(self) -> Effort {
        match self {
            Self::Low => Effort::Low,
            Self::Medium => Effort::Medium,
            Self::High => Effort::High,
            Self::Xhigh => Effort::Xhigh,
            Self::Max => Effort::Max,
        }
    }
}

impl PlanArg {
    pub(crate) fn to_protocol(self) -> PlanDecision {
        match self {
            Self::AcceptEdits => PlanDecision::AcceptEdits,
            Self::AskBeforeEdits => PlanDecision::AskBeforeEdits,
            Self::KeepPlanning => PlanDecision::KeepPlanning,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_bridge_subcommand_is_the_one_clients_run() {
        let cli = Cli::try_parse_from(["agents-cockpit", BRIDGE_SUBCOMMAND]).unwrap();

        assert!(matches!(cli.command, Command::Bridge));
        assert_eq!(BRIDGE_SUBCOMMAND, "bridge");
    }

    #[test]
    fn answers_parse() {
        let cli = Cli::try_parse_from([
            "agents-cockpit",
            "answer",
            "3",
            "perm-1",
            "plan",
            "keep-planning",
            "--message",
            "Add tests",
            "--ssh",
            "build-box",
        ])
        .unwrap();

        let Command::Answer {
            agent,
            pending,
            answer: AnswerCommand::Plan { decision, message },
        } = cli.command
        else {
            panic!("expected a plan answer, got {:?}", cli.command);
        };
        assert_eq!((agent, pending.as_str()), (3, "perm-1"));
        assert_eq!(decision, PlanArg::KeepPlanning);
        assert_eq!(message.as_deref(), Some("Add tests"));
        assert_eq!(cli.target.ssh.as_deref(), Some("build-box"));
    }
}

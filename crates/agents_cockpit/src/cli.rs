//! The command line's words.

use std::convert::Infallible;
use std::num::ParseIntError;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
#[cfg(unix)]
use cockpit_client::BRIDGE_SUBCOMMAND;
use cockpit_client::Transport;
use cockpit_daemon::{HOME_ENV, STATE_DIR_FLAG};
use cockpit_protocol::{AccountId, AgentId, Effort, PendingId, PermissionMode, PlanDecision};

/// The program's name, as the command line and its messages show it.
pub(crate) const PROGRAM: &str = "agents-cockpit";
#[cfg(unix)]
pub(crate) use cockpit_daemon::SERVE_SUBCOMMAND;
pub(crate) const SSH_FLAG: &str = "ssh";
const DEFAULT_REMOTE_COMMAND: &str = PROGRAM;

#[derive(Debug, Parser)]
#[command(
    name = PROGRAM,
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
    #[arg(long = STATE_DIR_FLAG, global = true)]
    pub(crate) state_dir: Option<PathBuf>,
    /// Reach the daemon on this host over SSH, through its bridge
    #[arg(long = SSH_FLAG, global = true, conflicts_with = "state_dir")]
    pub(crate) ssh: Option<String>,
    /// The absolute path of agents-cockpit on the SSH host
    #[arg(long, global = true, default_value = DEFAULT_REMOTE_COMMAND)]
    pub(crate) remote_command: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Run the daemon in the foreground
    #[cfg(unix)]
    #[command(name = SERVE_SUBCOMMAND)]
    Serve,
    /// Copy bytes between stdin and stdout and the daemon's socket, for
    /// `ssh <host> agents-cockpit bridge`
    #[cfg(unix)]
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
        /// The id of the Claude account to run with [default: the default account, 0]
        #[arg(long, value_parser = account_id)]
        account: Option<AccountId>,
    },
    /// Send a prompt; `-` reads it from stdin
    Prompt {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
        text: String,
    },
    /// Answer an approval, a question or a plan
    Answer {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
        /// The pending id the approval, question or plan carries
        #[arg(value_parser = pending_id)]
        pending: PendingId,
        #[command(subcommand)]
        answer: AnswerCommand,
    },
    /// Stop the current turn
    Interrupt {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
    },
    /// End an agent's claude process
    Stop {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
    },
    /// End an agent's claude process and take the agent off the list; its Claude session stays
    /// for a later resume
    Close {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
    },
    /// Rename an agent, and its Claude session while it runs
    Rename {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
        title: String,
    },
    /// Move an agent in the list, just above another one, or to the bottom
    Move {
        #[arg(value_parser = agent_id)]
        agent: AgentId,
        /// The agent to go just above; without it, the agent goes to the bottom
        #[arg(long, value_parser = agent_id)]
        before: Option<AgentId>,
    },
    /// Print events as JSON lines, of one agent or of all
    Tail {
        #[arg(value_parser = agent_id)]
        agent: Option<AgentId>,
    },
    /// List, add, change or remove the Claude accounts agents run with
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AccountCommand {
    /// List the accounts, whether each is signed in, and the command that signs in to it
    List,
    /// Add an account, and print the command that signs in to it, to run on the daemon's machine
    Add {
        name: String,
        /// Its Claude Code folder, absolute [default: .claude-<name> in the home folder]
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Give its agents the daemon's own ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN
        #[arg(long, alias = "use-environment-api-key")]
        use_environment_credentials: bool,
    },
    /// Rename an account, or change whether its agents get the daemon's ANTHROPIC_API_KEY and
    /// ANTHROPIC_AUTH_TOKEN
    Change {
        #[arg(value_parser = account_id)]
        account: AccountId,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, alias = "use-environment-api-key")]
        use_environment_credentials: Option<bool>,
    },
    /// Remove an account no agent uses; its folder stays
    Remove {
        #[arg(value_parser = account_id)]
        account: AccountId,
    },
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
        use anyhow::Context;

        let state_dir = match &self.state_dir {
            Some(state_dir) => state_dir.clone(),
            None => cockpit_daemon::default_state_dir()
                .with_context(|| format!("{HOME_ENV} is not set"))?,
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

impl From<ModeArg> for PermissionMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Default => Self::Default,
            ModeArg::AcceptEdits => Self::AcceptEdits,
            ModeArg::Plan => Self::Plan,
            ModeArg::Auto => Self::Auto,
            ModeArg::DontAsk => Self::DontAsk,
            ModeArg::BypassPermissions => Self::BypassPermissions,
        }
    }
}

impl From<EffortArg> for Effort {
    fn from(effort: EffortArg) -> Self {
        match effort {
            EffortArg::Low => Self::Low,
            EffortArg::Medium => Self::Medium,
            EffortArg::High => Self::High,
            EffortArg::Xhigh => Self::Xhigh,
            EffortArg::Max => Self::Max,
        }
    }
}

impl From<PlanArg> for PlanDecision {
    fn from(decision: PlanArg) -> Self {
        match decision {
            PlanArg::AcceptEdits => Self::AcceptEdits,
            PlanArg::AskBeforeEdits => Self::AskBeforeEdits,
            PlanArg::KeepPlanning => Self::KeepPlanning,
        }
    }
}

fn agent_id(text: &str) -> Result<AgentId, ParseIntError> {
    text.parse().map(AgentId::new)
}

fn account_id(text: &str) -> Result<AccountId, ParseIntError> {
    text.parse().map(AccountId::new)
}

fn pending_id(text: &str) -> Result<PendingId, Infallible> {
    Ok(PendingId::new(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[cfg(unix)]
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
        assert_eq!((agent, pending.as_str()), (AgentId::new(3), "perm-1"));
        assert_eq!(decision, PlanArg::KeepPlanning);
        assert_eq!(message.as_deref(), Some("Add tests"));
        assert_eq!(cli.target.ssh.as_deref(), Some("build-box"));
    }
}

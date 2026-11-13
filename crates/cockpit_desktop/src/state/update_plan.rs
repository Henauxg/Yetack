//! How an update of a machine's daemon runs: the update script on a server, or a restart of this
//! computer's background service with this app's own program. The daemon in the app needs no
//! update: it runs this app's code.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, BuildCommit};

/// The script's lines that move the steps on, as `say` in `contrib/update-daemon.sh` prints them.
pub const BUILDING_LINE: &str = "update-daemon: building";
pub const RESTARTING_LINE: &str = "update-daemon: restarting the daemon";
pub const DAEMON_RUNS_LINE: &str = "update-daemon: the daemon runs";

/// How an update of a machine's daemon runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePlan {
    /// A daemon reached over SSH: the update script builds this app's commit on its machine.
    Server(ServerPlan),
    /// This computer's background service: the app stops the daemon that holds its state folder,
    /// then starts the service with its own program.
    Service(ServicePlan),
}

/// What the update script runs on its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPlan {
    pub host: String,
    /// The absolute path of `agents-cockpit` on the host.
    pub remote_command: String,
    pub source_dir: Option<String>,
    /// The commit this app was built from.
    pub commit: BuildCommit,
}

/// The background service the app restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePlan {
    /// The state folder whose `serve.lock` the daemon holds.
    pub state_dir: PathBuf,
    /// The app's own state folder, which holds the service's log.
    pub log_folder: PathBuf,
}

impl UpdatePlan {
    /// The steps the update's screen lists, in order.
    pub fn steps(&self) -> &'static [UpdateStep] {
        match self {
            Self::Server(_) => &[
                UpdateStep::GetVersion,
                UpdateStep::Build,
                UpdateStep::Restart,
                UpdateStep::Reconnect,
            ],
            Self::Service(_) => &[UpdateStep::Stop, UpdateStep::Start, UpdateStep::Reconnect],
        }
    }

    /// The step an update of this plan starts at.
    pub fn first_step(&self) -> UpdateStep {
        match self {
            Self::Server(_) => UpdateStep::GetVersion,
            Self::Service(_) => UpdateStep::Stop,
        }
    }
}

/// A step of an update. Each plan's steps come in this order, so a step never goes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateStep {
    /// The script fetches this app's commit on the server.
    GetVersion,
    Build,
    /// The script installs the build and restarts the daemon.
    Restart,
    /// The app stops the daemon that holds the state folder.
    Stop,
    /// The app starts the service with its own program.
    Start,
    Reconnect,
}

impl UpdateStep {
    /// The step a line of the script's output starts, when it starts one.
    pub fn from_line(line: &str) -> Option<Self> {
        if line.starts_with(BUILDING_LINE) {
            Some(Self::Build)
        } else if line.starts_with(RESTARTING_LINE) {
            Some(Self::Restart)
        } else if line.starts_with(DAEMON_RUNS_LINE) {
            Some(Self::Reconnect)
        } else {
            None
        }
    }
}

/// What an update does about the machine's busy agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyAgents {
    /// The update refuses to restart while an agent is busy or may hold a scheduled job. A
    /// server's script checks both. Before it restarts this computer's service, the app reads the
    /// jobs again: it saw the running agents itself.
    Refuse,
    /// "Update now": the restart ends their turns, which the new daemon resumes, and loses their
    /// scheduled jobs.
    Interrupt,
}

/// The raw logs to read before an update of this computer's service, to tell which of its resting
/// agents may hold a scheduled job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCheck {
    pub plan: ServicePlan,
    /// The resting agents, by the app's ids.
    pub agents: Vec<AgentId>,
}

/// An update to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRun {
    pub plan: UpdatePlan,
    pub busy: BusyAgents,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> UpdatePlan {
        UpdatePlan::Server(ServerPlan {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
            commit: BuildCommit::new("3f9c2e1a7b4d5c6e".to_owned()),
        })
    }

    fn service() -> UpdatePlan {
        UpdatePlan::Service(ServicePlan {
            state_dir: PathBuf::from("/home/someone/.local/state/agents-cockpit"),
            log_folder: PathBuf::from("/home/someone/.local/state/agents-cockpit-desktop"),
        })
    }

    #[test]
    fn each_plan_lists_its_steps_in_order_from_its_first() {
        for plan in [server(), service()] {
            let steps = plan.steps();

            assert_eq!(steps.first(), Some(&plan.first_step()), "{plan:?}");
            assert!(steps.is_sorted(), "{plan:?}");
            assert_eq!(steps.last(), Some(&UpdateStep::Reconnect), "{plan:?}");
        }
    }

    #[test]
    fn the_scripts_lines_name_their_steps() {
        assert_eq!(
            UpdateStep::from_line("update-daemon: building 3f9c2e1"),
            Some(UpdateStep::Build)
        );
        assert_eq!(
            UpdateStep::from_line("update-daemon: restarting the daemon"),
            Some(UpdateStep::Restart)
        );
        assert_eq!(
            UpdateStep::from_line("update-daemon: the daemon runs 3f9c2e1"),
            Some(UpdateStep::Reconnect)
        );
        assert_eq!(
            UpdateStep::from_line("   Compiling cockpit_daemon v0.1.0"),
            None
        );
    }
}

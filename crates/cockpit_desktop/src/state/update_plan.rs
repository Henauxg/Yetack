//! How an update of a machine's daemon runs: the update script on a server, or a restart of this
//! computer's background service with this app's own program. The daemon in the app needs no
//! update: it runs this app's code.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, BuildCommit};

use super::daemon_probe::LinuxCpu;
use crate::format;

/// The script's lines that move the steps on, as `say` in `contrib/update-daemon.sh` prints them.
pub const BUILDING_LINE: &str = "update-daemon: building";
pub const CHECKING_LINE: &str = "update-daemon: checking";
pub const RESTARTING_LINE: &str = "update-daemon: restarting the daemon";
pub const DAEMON_RUNS_LINE: &str = "update-daemon: the daemon runs";

/// How an update of a machine's daemon runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePlan {
    /// A daemon reached over SSH: the app sends the Linux daemon it carries, or without one the
    /// update script builds this app's commit on the server.
    Server(ServerPlan),
    /// This computer's background service: the app stops the daemon that holds its state folder,
    /// then starts the service with its own program.
    Service(ServicePlan),
}

/// What the update script runs on its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPlan {
    pub host: String,
    /// The absolute path of `yetack` on the host.
    pub remote_command: String,
    pub source_dir: Option<String>,
    /// The commit this app was built from.
    pub commit: BuildCommit,
    /// The Linux daemons this app carries, one per CPU.
    pub carried: CarriedDaemons,
    /// The daemon of `carried` for the server's CPU, which the update sends, once the probe said
    /// the CPU (`ServerInstall::plan_for`). Without it, the script builds `commit` on the server.
    pub daemon: Option<DaemonSource>,
}

/// The static Linux daemons this app carries, by CPU, and whether it may download its release's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CarriedDaemons {
    pub x86_64: Option<PathBuf>,
    pub aarch64: Option<PathBuf>,
    /// The app knows the commit it was built from, so it may download the daemon of its release
    /// for a CPU it carries none for. The download checks that the release is this build.
    pub release: bool,
}

impl CarriedDaemons {
    /// The daemon for `cpu`: the one carried, else the release's when the app may download it.
    pub fn for_cpu(&self, cpu: LinuxCpu) -> Option<DaemonSource> {
        let carried = match cpu {
            LinuxCpu::X86_64 => self.x86_64.as_ref(),
            LinuxCpu::Aarch64 => self.aarch64.as_ref(),
        };
        match carried {
            Some(file) => Some(DaemonSource::Carried(file.clone())),
            None => self.release.then_some(DaemonSource::Release(cpu)),
        }
    }
}

/// Where the daemon that an install or an update sends comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonSource {
    /// A file this app carries.
    Carried(PathBuf),
    /// The daemon of this app's release for the CPU, which the app downloads before it sends it.
    Release(LinuxCpu),
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
            Self::Server(plan) if plan.daemon.is_some() => &[
                UpdateStep::Send,
                UpdateStep::Check,
                UpdateStep::Restart,
                UpdateStep::Reconnect,
            ],
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
            Self::Server(plan) if plan.daemon.is_some() => UpdateStep::Send,
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
    /// The app sends the daemon it carries to the server.
    Send,
    /// The script checks the sent daemon's checksum and version.
    Check,
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
        } else if line.starts_with(CHECKING_LINE) {
            Some(Self::Check)
        } else if line.starts_with(RESTARTING_LINE) {
            Some(Self::Restart)
        } else if line.starts_with(DAEMON_RUNS_LINE) {
            Some(Self::Reconnect)
        } else {
            None
        }
    }
}

/// How far the send of the daemon the app carries has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendProgress {
    /// The bytes sent so far.
    pub sent: u64,
    /// The daemon's size.
    pub total: u64,
}

impl SendProgress {
    /// The line under the send's step: "4.1 MB of 9.3 MB" while bytes are left to send, then the
    /// daemon's size.
    pub fn detail(self) -> String {
        if self.sent < self.total {
            format!(
                "{} of {}",
                format::bytes(self.sent),
                format::bytes(self.total)
            )
        } else {
            format::bytes(self.total)
        }
    }

    /// The share of the daemon sent, in percent.
    pub fn percent(self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        let percent = self.sent.min(self.total).saturating_mul(100) / self.total;
        u8::try_from(percent).unwrap_or(100)
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

    fn server_plan() -> ServerPlan {
        ServerPlan {
            host: "build-box".to_owned(),
            remote_command: "/opt/yetack".to_owned(),
            source_dir: None,
            commit: BuildCommit::new("3f9c2e1a7b4d5c6e".to_owned()),
            carried: CarriedDaemons::default(),
            daemon: None,
        }
    }

    fn server() -> UpdatePlan {
        UpdatePlan::Server(server_plan())
    }

    fn sending_server() -> UpdatePlan {
        UpdatePlan::Server(ServerPlan {
            daemon: Some(DaemonSource::Carried(PathBuf::from(
                "/opt/app/yetack-x86_64-linux",
            ))),
            ..server_plan()
        })
    }

    fn service() -> UpdatePlan {
        UpdatePlan::Service(ServicePlan {
            state_dir: PathBuf::from("/home/someone/.local/state/yetack"),
            log_folder: PathBuf::from("/home/someone/.local/state/yetack-desktop"),
        })
    }

    #[test]
    fn each_plan_lists_its_steps_in_order_from_its_first() {
        for plan in [server(), sending_server(), service()] {
            let steps = plan.steps();

            assert_eq!(steps.first(), Some(&plan.first_step()), "{plan:?}");
            assert!(steps.is_sorted(), "{plan:?}");
            assert_eq!(steps.last(), Some(&UpdateStep::Reconnect), "{plan:?}");
        }
    }

    #[test]
    fn a_send_counts_its_bytes_until_it_ends_then_gives_the_size() {
        let size = 9_800_000;
        let under_way = SendProgress {
            sent: 4_300_000,
            total: size,
        };
        let done = SendProgress {
            sent: size,
            total: size,
        };

        assert_eq!(under_way.detail(), "4.1 MB of 9.3 MB");
        assert_eq!(under_way.percent(), 43);
        assert_eq!(done.detail(), "9.3 MB");
        assert_eq!(done.percent(), 100);
        assert_eq!(SendProgress { sent: 0, total: 0 }.percent(), 0);
    }

    #[test]
    fn the_scripts_lines_name_their_steps() {
        assert_eq!(
            UpdateStep::from_line("update-daemon: building 3f9c2e1"),
            Some(UpdateStep::Build)
        );
        assert_eq!(
            UpdateStep::from_line("update-daemon: checking /home/someone/.cache/x"),
            Some(UpdateStep::Check)
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

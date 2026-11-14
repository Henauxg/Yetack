//! "Add a server" without a path: the app looks for agents-cockpit on the server. It adds the
//! server with the daemon it finds, or offers to install the daemon it carries, then says what else
//! the daemon needs. The setup lives beside the Machines dialog, so closing the dialog does not
//! stop an install: the dialog shows where it stands when it opens again.

use super::app_state::AppState;
use super::daemon_probe::ServerInstall;
use super::machines::{self, MachineChange};
use crate::config::MachineConfig;

/// The lines `contrib/install-daemon.sh` prints, as `say` writes them.
pub const CHECKING_LINE: &str = "install-daemon: checking";
pub const INSTALLED_LINE: &str = "install-daemon: installed ";
pub const STARTING_LINE: &str = "install-daemon: starting the service";
pub const RUNS_LINE: &str = "install-daemon: the daemon runs";
pub const LINGER_LINE: &str = "install-daemon: linger ";
pub const CLAUDE_LINE: &str = "install-daemon: claude ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSetup {
    /// The server as the form named it. Its `remote_command` is the daemon's path once the app
    /// found or installed it.
    pub config: MachineConfig,
    pub stage: SetupStage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupStage {
    /// The app reaches the server and looks for agents-cockpit there.
    Looking,
    /// The server, of `system`, has no agents-cockpit: the app asks before it installs one.
    Offer { system: String },
    /// The server has no agents-cockpit, and the app has none for it: its `system` is not the one
    /// of the daemon the app carries, or the app carries none.
    CannotInstall { system: String },
    Installing {
        step: InstallStep,
        /// The bytes of the daemon sent so far, and its size.
        sent: Option<(u64, u64)>,
    },
    /// The daemon runs, and the report says what else it needs.
    Installed(InstallReport),
    /// The step failed, as `message` says.
    Failed { step: InstallStep, message: String },
}

/// The steps of a setup, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstallStep {
    Reach,
    Send,
    Check,
    Start,
}

/// What the daemon needs besides itself, as the install script reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallReport {
    /// Whether systemd keeps the user's services running once they log out. `None` when
    /// `loginctl` did not say.
    pub linger: Option<bool>,
    /// Where the service finds `claude`, `None` when it finds none.
    pub claude: Option<String>,
    /// "Check again" runs.
    pub checking: bool,
}

impl InstallReport {
    /// Reads the report's lines among the script's output. A line it does not know is skipped.
    pub fn read<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut report = Self::default();
        for line in lines {
            if let Some(linger) = line.strip_prefix(LINGER_LINE) {
                report.linger = match linger.trim() {
                    "yes" => Some(true),
                    "no" => Some(false),
                    _ => None,
                };
            } else if let Some(claude) = line.strip_prefix(CLAUDE_LINE) {
                let claude = claude.trim();
                report.claude =
                    (claude != "missing" && !claude.is_empty()).then(|| claude.to_owned());
            }
        }
        report
    }

    /// How many things are left for the user to do on the server: turn linger on, install Claude
    /// Code.
    pub fn left_to_do(&self) -> usize {
        usize::from(self.linger != Some(true)) + usize::from(self.claude.is_none())
    }
}

/// What the connection task runs for a setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupRun {
    /// Looks for agents-cockpit on `host`.
    Look { host: String },
    /// Sends the daemon the app carries to `host`, and installs it there.
    Install { host: String },
    /// Reads the report again.
    Check { host: String },
}

/// Starts the setup of `config`, a server the form named without a path.
pub fn start(state: &mut AppState, config: MachineConfig) -> SetupRun {
    let host = config.host.clone();
    state.machines.setup = Some(ServerSetup {
        config,
        stage: SetupStage::Looking,
    });
    SetupRun::Look { host }
}

/// The look at the server ended. A daemon found there adds the server with its path. Otherwise
/// the setup offers to install, when `carries` says the app has a daemon for the server's system.
pub fn looked(
    state: &mut AppState,
    found: Result<ServerInstall, String>,
    carries: bool,
) -> Option<MachineChange> {
    let setup = state.machines.setup.as_mut()?;
    if setup.stage != SetupStage::Looking {
        return None;
    }
    let install = match found {
        Ok(install) => install,
        Err(message) => {
            setup.stage = SetupStage::Failed {
                step: InstallStep::Reach,
                message,
            };
            return None;
        }
    };
    if let Some(daemon) = install.daemon {
        let mut config = setup.config.clone();
        config.remote_command = daemon.path;
        state.machines.setup = None;
        machines::close_add_form(state);
        return Some(MachineChange::Add(config));
    }
    setup.stage = if carries && install.runs_carried_daemon() {
        SetupStage::Offer {
            system: install.system,
        }
    } else {
        SetupStage::CannotInstall {
            system: install.system,
        }
    };
    None
}

/// "Install on <server>".
pub fn install(state: &mut AppState) -> Option<SetupRun> {
    let setup = state.machines.setup.as_mut()?;
    if !matches!(setup.stage, SetupStage::Offer { .. }) {
        return None;
    }
    setup.stage = SetupStage::Installing {
        step: InstallStep::Send,
        sent: None,
    };
    Some(SetupRun::Install {
        host: setup.config.host.clone(),
    })
}

/// The app sent `sent` bytes of the daemon, of `total`.
pub fn sending(state: &mut AppState, sent_now: u64, total: u64) {
    if let Some(SetupStage::Installing { sent, .. }) = stage_mut(state) {
        *sent = Some((sent_now, total));
    }
}

/// A line of the install script, which moves the steps on and names the installed daemon.
pub fn output(state: &mut AppState, line: &str) {
    let Some(setup) = state.machines.setup.as_mut() else {
        return;
    };
    if let Some(path) = line.strip_prefix(INSTALLED_LINE) {
        path.trim().clone_into(&mut setup.config.remote_command);
    }
    let reached = if line.starts_with(CHECKING_LINE) {
        InstallStep::Check
    } else if line.starts_with(STARTING_LINE) {
        InstallStep::Start
    } else {
        return;
    };
    if let SetupStage::Installing { step, .. } = &mut setup.stage {
        *step = (*step).max(reached);
    }
}

/// The install ended with `code`, `None` when ssh lost it, and `lines`, what the script printed.
/// Once the daemon runs, the server joins the app with the installed path.
pub fn installed(
    state: &mut AppState,
    code: Option<i32>,
    lines: &[String],
) -> Option<MachineChange> {
    let setup = state.machines.setup.as_mut()?;
    let SetupStage::Installing { step, .. } = setup.stage else {
        return None;
    };
    if code == Some(0) && lines.iter().any(|line| line.starts_with(RUNS_LINE)) {
        setup.stage = SetupStage::Installed(InstallReport::read(lines.iter().map(String::as_str)));
        return Some(MachineChange::Add(setup.config.clone()));
    }
    let said: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("install-daemon: "))
        .collect();
    let message = match said.last() {
        Some(last) => (*last).to_owned(),
        None if code.is_none() => "The connection to the server was lost.".to_owned(),
        None => format!("The install ended with code {}.", code.unwrap_or_default()),
    };
    setup.stage = SetupStage::Failed { step, message };
    None
}

/// The send failed before the script ran.
pub fn send_failed(state: &mut AppState, message: String) {
    if let Some(setup) = state.machines.setup.as_mut() {
        setup.stage = SetupStage::Failed {
            step: InstallStep::Send,
            message,
        };
    }
}

/// "Check again", once the daemon runs.
pub fn check_again(state: &mut AppState) -> Option<SetupRun> {
    let setup = state.machines.setup.as_mut()?;
    let SetupStage::Installed(report) = &mut setup.stage else {
        return None;
    };
    if report.checking {
        return None;
    }
    report.checking = true;
    Some(SetupRun::Check {
        host: setup.config.host.clone(),
    })
}

/// The report read again, from the script's lines. A check that failed keeps the last report.
pub fn checked(state: &mut AppState, lines: Option<&[String]>) {
    if let Some(SetupStage::Installed(report)) = stage_mut(state) {
        *report = match lines {
            Some(lines) => InstallReport::read(lines.iter().map(String::as_str)),
            None => InstallReport {
                checking: false,
                ..report.clone()
            },
        };
    }
}

/// "Try again" after a failure: the setup looks at the server again, then offers again.
pub fn retry(state: &mut AppState) -> Option<SetupRun> {
    let setup = state.machines.setup.as_mut()?;
    if !matches!(setup.stage, SetupStage::Failed { .. }) {
        return None;
    }
    setup.stage = SetupStage::Looking;
    Some(SetupRun::Look {
        host: setup.config.host.clone(),
    })
}

/// "Done", "Cancel" or "Back to the form": the setup ends. An install under way goes on, and its
/// server joins the app once its daemon runs. Cancel and Back leave the form open.
pub fn close(state: &mut AppState) {
    let installing = matches!(
        state.machines.setup.as_ref().map(|setup| &setup.stage),
        Some(SetupStage::Installing { .. })
    );
    if !installing {
        state.machines.setup = None;
    }
}

fn stage_mut(state: &mut AppState) -> Option<&mut SetupStage> {
    state.machines.setup.as_mut().map(|setup| &mut setup.stage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state::tests::state_with;
    use crate::state::daemon_probe::{DaemonOwner, InstalledDaemon};

    fn lab() -> MachineConfig {
        MachineConfig {
            name: "lab".to_owned(),
            host: "lab".to_owned(),
            remote_command: String::new(),
            source_dir: None,
        }
    }

    fn server(daemon: Option<&str>, system: &str) -> ServerInstall {
        ServerInstall {
            system: system.to_owned(),
            daemon: daemon.map(|path| InstalledDaemon {
                path: path.to_owned(),
                owner: DaemonOwner::App,
                version: "agents-cockpit 0.1.0".to_owned(),
            }),
        }
    }

    fn stage(state: &AppState) -> Option<&SetupStage> {
        state.machines.setup.as_ref().map(|setup| &setup.stage)
    }

    fn lines(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| (*text).to_owned()).collect()
    }

    #[test]
    fn a_daemon_found_on_the_server_adds_it_with_its_path() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());

        let change = looked(
            &mut state,
            Ok(server(
                Some("/home/someone/.local/bin/agents-cockpit"),
                "Linux x86_64",
            )),
            true,
        );

        assert_eq!(
            change,
            Some(MachineChange::Add(MachineConfig {
                remote_command: "/home/someone/.local/bin/agents-cockpit".to_owned(),
                ..lab()
            }))
        );
        assert_eq!(state.machines.setup, None);
    }

    #[test]
    fn without_a_daemon_the_app_offers_to_install_only_the_one_it_has() {
        for (system, carries, offers) in [
            ("Linux x86_64", true, true),
            ("Linux aarch64", true, false),
            ("Linux x86_64", false, false),
        ] {
            let mut state = state_with(Vec::new());
            start(&mut state, lab());

            let change = looked(&mut state, Ok(server(None, system)), carries);

            assert_eq!(change, None);
            assert_eq!(
                matches!(stage(&state), Some(SetupStage::Offer { .. })),
                offers,
                "{system}, carries {carries}"
            );
        }
    }

    #[test]
    fn an_install_moves_through_its_steps_and_adds_the_server_with_the_installed_path() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        looked(&mut state, Ok(server(None, "Linux x86_64")), true);

        assert_eq!(
            install(&mut state),
            Some(SetupRun::Install {
                host: "lab".to_owned()
            })
        );
        sending(&mut state, 5, 10);
        assert_eq!(
            stage(&state),
            Some(&SetupStage::Installing {
                step: InstallStep::Send,
                sent: Some((5, 10))
            })
        );
        let said = lines(&[
            "install-daemon: checking /home/someone/.cache/agents-cockpit/sent/agents-cockpit.a1",
            "install-daemon: installed /home/someone/.local/share/agents-cockpit/bin/agents-cockpit",
            "install-daemon: starting the service",
            "install-daemon: the daemon runs",
            "install-daemon: linger no",
            "install-daemon: claude missing",
        ]);
        for line in &said {
            output(&mut state, line);
        }
        let change = installed(&mut state, Some(0), &said);

        assert_eq!(
            change,
            Some(MachineChange::Add(MachineConfig {
                remote_command: "/home/someone/.local/share/agents-cockpit/bin/agents-cockpit"
                    .to_owned(),
                ..lab()
            }))
        );
        let Some(SetupStage::Installed(report)) = stage(&state) else {
            panic!("expected the report, got {:?}", stage(&state));
        };
        assert_eq!((report.linger, report.claude.clone()), (Some(false), None));
        assert_eq!(report.left_to_do(), 2);
    }

    #[test]
    fn a_failed_install_names_its_step_and_the_scripts_last_word() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        looked(&mut state, Ok(server(None, "Linux x86_64")), true);
        install(&mut state);
        let said = lines(&[
            "install-daemon: checking /tmp/x",
            "install-daemon: the binary on this machine is not the one the app sent: its checksum differs",
        ]);
        output(&mut state, &said[0]);

        let change = installed(&mut state, Some(1), &said);

        assert_eq!(change, None);
        assert_eq!(
            stage(&state),
            Some(&SetupStage::Failed {
                step: InstallStep::Check,
                message:
                    "the binary on this machine is not the one the app sent: its checksum differs"
                        .to_owned()
            })
        );
    }

    #[test]
    fn check_again_runs_once_at_a_time_and_replaces_the_report() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        state.machines.setup = Some(ServerSetup {
            config: lab(),
            stage: SetupStage::Installed(InstallReport::default()),
        });

        assert_eq!(
            check_again(&mut state),
            Some(SetupRun::Check {
                host: "lab".to_owned()
            })
        );
        assert_eq!(check_again(&mut state), None, "one runs already");
        checked(
            &mut state,
            Some(&lines(&[
                "install-daemon: linger yes",
                "install-daemon: claude /usr/bin/claude",
            ])),
        );

        let Some(SetupStage::Installed(report)) = stage(&state) else {
            panic!("expected the report");
        };
        assert_eq!(report.left_to_do(), 0);
        assert!(!report.checking);
    }

    #[test]
    fn closing_leaves_an_install_under_way_to_finish() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        looked(&mut state, Ok(server(None, "Linux x86_64")), true);
        install(&mut state);

        close(&mut state);
        assert!(state.machines.setup.is_some(), "the install goes on");

        installed(&mut state, Some(1), &[]);
        close(&mut state);
        assert_eq!(state.machines.setup, None);
    }
}

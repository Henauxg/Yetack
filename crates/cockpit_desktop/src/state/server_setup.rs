//! "Add a server" without a path: the app looks for `yetack` on the server. It adds the
//! server with the daemon it finds, or offers to install the daemon it carries, then says what else
//! the daemon needs. The setup lives beside the Machines dialog, so closing the dialog does not
//! stop an install: the dialog shows where it stands when it opens again.

use strum::{EnumString, IntoStaticStr, VariantArray};

use super::app_state::AppState;
use super::daemon_probe::{LinuxCpu, ServerInstall};
use super::daemon_update::{OutputLine, OutputStream};
use super::machines::{self, MachineChange};
use super::update_plan::{CarriedDaemons, SendProgress};
use crate::config::MachineConfig;

/// What `say` in `contrib/install-daemon.sh` puts before each of its lines.
const SAY_PREFIX: &str = "install-daemon: ";

// The lines of `contrib/install-daemon.sh` that the setup reads, each starting with `SAY_PREFIX`.
pub const CHECKING_LINE: &str = "install-daemon: checking";
pub const INSTALLED_LINE: &str = "install-daemon: installed ";
pub const STARTING_LINE: &str = "install-daemon: starting the service";
pub const STARTING_ALONE_LINE: &str = "install-daemon: starting the daemon";
pub const RUNS_LINE: &str = "install-daemon: the daemon runs";
pub const LINGER_LINE: &str = "install-daemon: linger ";
pub const CLAUDE_LINE: &str = "install-daemon: claude ";
pub const SERVICE_LINE: &str = "install-daemon: service ";
pub const BOOT_LINE: &str = "install-daemon: boot ";
pub const START_LINE: &str = "install-daemon: start ";
pub const CRON_LINE: &str = "install-daemon: cron ";

// The words after `LINGER_LINE` and `CLAUDE_LINE`.
const LINGER_ON: &str = "yes";
const LINGER_OFF: &str = "no";
const CLAUDE_MISSING: &str = "missing";

const LOST_CONNECTION: &str = "The connection to the server was lost: the install may have ended there. Try again looks at the server again.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSetup {
    /// The server as the form named it. Its `remote_command` is the daemon's path once the app
    /// found or installed it.
    pub config: MachineConfig,
    pub stage: SetupStage,
}

/// Where a setup stands. The interface keys a stage's content by its name.
#[derive(Debug, Clone, PartialEq, Eq, IntoStaticStr)]
pub enum SetupStage {
    /// The app reaches the server and looks for `yetack` there.
    Looking,
    /// The server, of `system`, has no `yetack`: the app asks before it installs one.
    Offer { system: String },
    /// The server has no `yetack`, and the app has none for it: its `system` is not the one
    /// of the daemon the app carries, or the app carries none.
    CannotInstall { system: String },
    Installing {
        step: InstallStep,
        /// How far the send of the daemon has gone, once it started.
        sent: Option<SendProgress>,
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
    /// What starts the daemon. A script older than the start without systemd says nothing of it,
    /// and installed a service.
    pub starter: Starter,
    /// Whether systemd keeps the user's services running once they log out. `None` when
    /// `loginctl` did not say.
    pub linger: Option<bool>,
    /// Without systemd, whether the user's crontab starts the daemon when the server starts.
    pub boot: Option<BootStart>,
    /// Without systemd, the command that starts the daemon, which a crontab line or the server's
    /// own init system runs.
    pub start_command: Option<String>,
    /// Without systemd, the exact line "Add it to my crontab" puts in the user's crontab.
    pub cron_line: Option<String>,
    /// Where the service finds `claude`, `None` when it finds none.
    pub claude: Option<String>,
    /// "Check again" or "Add it to my crontab" runs.
    pub checking: bool,
    /// What the last "Check again" or "Add it to my crontab" said as it failed.
    pub failure: Option<String>,
}

/// What starts the daemon on the server, as the words after `SERVICE_LINE` say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum Starter {
    /// A systemd user service.
    #[default]
    Systemd,
    /// The install started it apart from its SSH session: nothing starts it after a restart,
    /// unless the crontab does.
    #[strum(serialize = "none")]
    Alone,
}

/// Whether the user's crontab starts the daemon when the server starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum BootStart {
    Yes,
    No,
    /// The server has no `crontab`.
    Unavailable,
}

impl InstallReport {
    /// Reads the report's lines among the script's output. A line it does not know is skipped.
    pub fn read<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut report = Self::default();
        for line in lines {
            if let Some(linger) = line.strip_prefix(LINGER_LINE) {
                report.linger = match linger.trim() {
                    LINGER_ON => Some(true),
                    LINGER_OFF => Some(false),
                    _ => None,
                };
            } else if let Some(claude) = line.strip_prefix(CLAUDE_LINE) {
                let claude = claude.trim();
                report.claude =
                    (claude != CLAUDE_MISSING && !claude.is_empty()).then(|| claude.to_owned());
            } else if let Some(service) = line.strip_prefix(SERVICE_LINE) {
                report.starter = service.trim().parse().unwrap_or_default();
            } else if let Some(boot) = line.strip_prefix(BOOT_LINE) {
                report.boot = boot.trim().parse().ok();
            } else if let Some(command) = line.strip_prefix(START_LINE) {
                report.start_command = Some(command.trim().to_owned());
            } else if let Some(cron) = line.strip_prefix(CRON_LINE) {
                report.cron_line = Some(cron.trim().to_owned());
            }
        }
        report
    }

    /// How many things are left for the user to do on the server: keep the daemon running (linger
    /// for a service, a start at boot without one), install Claude Code.
    pub fn left_to_do(&self) -> usize {
        let keeps_running = match self.starter {
            Starter::Systemd => self.linger == Some(true),
            Starter::Alone => self.boot == Some(BootStart::Yes),
        };
        usize::from(!keeps_running) + usize::from(self.claude.is_none())
    }

    /// How many of `left_to_do` the user does on the server: all but the crontab line, which "Add
    /// it to my crontab" adds from the app.
    pub fn left_on_server(&self) -> usize {
        let in_the_app = self.starter == Starter::Alone && self.boot == Some(BootStart::No);
        self.left_to_do() - usize::from(in_the_app)
    }
}

/// What the connection task runs for a setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupRun {
    /// Looks for `yetack` on `host`.
    Look { host: String },
    /// Sends the daemon the app carries for `cpu` to `host`, and installs it there.
    Install { host: String, cpu: LinuxCpu },
    /// Reads the report again.
    Check { host: String },
    /// Adds the start at boot to the user's crontab on `host`, then reads the report again.
    AddBootStart { host: String },
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
/// the setup offers to install the daemon this app carries for the server's CPU, when `carried`
/// has one.
pub fn looked(
    state: &mut AppState,
    host: &str,
    found: Result<ServerInstall, String>,
    carried: &CarriedDaemons,
) -> Option<MachineChange> {
    let setup = setup_of(state, host)?;
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
        config.remote_command = daemon.command;
        state.machines.setup = None;
        machines::close_add_form(state);
        return Some(MachineChange::Add(config));
    }
    let offers = install.cpu().and_then(|cpu| carried.for_cpu(cpu)).is_some();
    setup.stage = if offers {
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
    let SetupStage::Offer { system } = &setup.stage else {
        return None;
    };
    let cpu = LinuxCpu::of_system(system)?;
    setup.stage = SetupStage::Installing {
        step: InstallStep::Send,
        sent: None,
    };
    Some(SetupRun::Install {
        host: setup.config.host.clone(),
        cpu,
    })
}

/// The send of the daemon to `host` reached `progress`.
pub fn sending(state: &mut AppState, host: &str, progress: SendProgress) {
    if let Some(SetupStage::Installing { sent, .. }) =
        setup_of(state, host).map(|setup| &mut setup.stage)
    {
        *sent = Some(progress);
    }
}

/// A line of the install script on `host`, which moves the steps on and names the installed
/// daemon.
pub fn output(state: &mut AppState, host: &str, line: &str) {
    let Some(setup) = setup_of(state, host) else {
        return;
    };
    if let Some(path) = line.strip_prefix(INSTALLED_LINE) {
        path.trim().clone_into(&mut setup.config.remote_command);
    }
    let reached = if line.starts_with(CHECKING_LINE) {
        InstallStep::Check
    } else if line.starts_with(STARTING_LINE) || line.starts_with(STARTING_ALONE_LINE) {
        InstallStep::Start
    } else {
        return;
    };
    if let SetupStage::Installing { step, .. } = &mut setup.stage {
        *step = (*step).max(reached);
    }
}

/// The install on `host` ended with `code`, `None` when ssh lost it, and `lines`, what the
/// script printed. Once the daemon runs, the server joins the app with the installed path.
pub fn installed(
    state: &mut AppState,
    host: &str,
    code: Option<i32>,
    lines: &[String],
) -> Option<MachineChange> {
    let setup = setup_of(state, host)?;
    let SetupStage::Installing { step, .. } = setup.stage else {
        return None;
    };
    if code == Some(0) && lines.iter().any(|line| line.starts_with(RUNS_LINE)) {
        setup.stage = SetupStage::Installed(InstallReport::read(lines.iter().map(String::as_str)));
        return Some(MachineChange::Add(setup.config.clone()));
    }
    let said: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix(SAY_PREFIX))
        .collect();
    // The script exits with 1 or 4 after its last word. ssh says 255 when it lost the server,
    // where the install may have ended well.
    let message = match (code, said.last()) {
        (None | Some(255), _) => LOST_CONNECTION.to_owned(),
        (_, Some(last)) => (*last).to_owned(),
        (Some(code), None) => format!("The install ended with code {code}."),
    };
    setup.stage = SetupStage::Failed { step, message };
    None
}

/// The send to `host` failed before the script ran.
pub fn send_failed(state: &mut AppState, host: &str, message: String) {
    if let Some(setup) = setup_of(state, host) {
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
    report.failure = None;
    Some(SetupRun::Check {
        host: setup.config.host.clone(),
    })
}

/// "Add it to my crontab": only once the report said nothing starts the daemon at boot, and the
/// server has a crontab.
pub fn add_boot_start(state: &mut AppState) -> Option<SetupRun> {
    let setup = state.machines.setup.as_mut()?;
    let SetupStage::Installed(report) = &mut setup.stage else {
        return None;
    };
    if report.checking || report.boot != Some(BootStart::No) {
        return None;
    }
    report.checking = true;
    report.failure = None;
    Some(SetupRun::AddBootStart {
        host: setup.config.host.clone(),
    })
}

/// What a run of the script that ended with `code` gave: its lines once it ended well, else what
/// it said last as it failed.
pub fn script_result(code: Option<i32>, lines: Vec<OutputLine>) -> Result<Vec<String>, String> {
    if code == Some(0) {
        return Ok(lines.into_iter().map(|line| line.text).collect());
    }
    Err(last_word(&lines).unwrap_or_else(|| match code {
        Some(code) => format!("The script ended with code {code}."),
        None => "The script did not run.".to_owned(),
    }))
}

/// The last line the script, or ssh, wrote as an error, without the script's prefix.
pub fn last_word(lines: &[OutputLine]) -> Option<String> {
    let line = lines
        .iter()
        .rev()
        .find(|line| line.stream == OutputStream::Stderr && !line.text.trim().is_empty())?;
    let text = line.text.trim();
    Some(text.strip_prefix(SAY_PREFIX).unwrap_or(text).to_owned())
}

/// The report of `host` read again, from the script's lines. A check that failed keeps the last
/// report, with what the script said.
pub fn checked(state: &mut AppState, host: &str, result: Result<Vec<String>, String>) {
    if let Some(SetupStage::Installed(report)) = setup_of(state, host).map(|setup| &mut setup.stage)
    {
        *report = match result {
            Ok(lines) => InstallReport::read(lines.iter().map(String::as_str)),
            Err(message) => InstallReport {
                checking: false,
                failure: Some(message),
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

/// The setup of `host`. A result of another host's setup, which the user closed since, finds
/// none.
fn setup_of<'a>(state: &'a mut AppState, host: &str) -> Option<&'a mut ServerSetup> {
    state
        .machines
        .setup
        .as_mut()
        .filter(|setup| setup.config.host == host)
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
                command: path.to_owned(),
                owner: DaemonOwner::App,
                version: "yetack 0.1.0".to_owned(),
            }),
        }
    }

    fn stage(state: &AppState) -> Option<&SetupStage> {
        state.machines.setup.as_ref().map(|setup| &setup.stage)
    }

    fn lines(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| (*text).to_owned()).collect()
    }

    fn carried() -> CarriedDaemons {
        CarriedDaemons {
            x86_64: Some("/opt/app/yetack-x86_64-linux".into()),
            aarch64: None,
            release: false,
        }
    }

    /// The lines of an install on a server without systemd.
    fn alone(boot: &str) -> Vec<String> {
        lines(&[
            "install-daemon: the daemon runs",
            "install-daemon: service none",
            &format!("install-daemon: boot {boot}"),
            "install-daemon: start PATH=/usr/bin /home/someone/.local/share/yetack/bin/yetack serve",
            "install-daemon: cron @reboot PATH=/usr/bin /home/someone/.local/share/yetack/bin/yetack serve # yetack",
            "install-daemon: claude /usr/bin/claude",
        ])
    }

    #[test]
    fn without_systemd_the_report_counts_the_start_at_boot_and_shows_its_crontab_line() {
        let read = |boot: &str| InstallReport::read(alone(boot).iter().map(String::as_str));

        let no = read("no");
        let yes = read("yes");
        let without_cron = read("unavailable");

        assert_eq!(no.starter, Starter::Alone);
        assert_eq!(no.boot, Some(BootStart::No));
        assert_eq!((no.left_to_do(), no.left_on_server()), (1, 0));
        assert_eq!(
            no.cron_line.as_deref(),
            Some(
                "@reboot PATH=/usr/bin /home/someone/.local/share/yetack/bin/yetack serve # yetack"
            )
        );
        assert_eq!(yes.left_to_do(), 0);
        assert_eq!(without_cron.boot, Some(BootStart::Unavailable));
        assert_eq!(
            (without_cron.left_to_do(), without_cron.left_on_server()),
            (1, 1)
        );
        let service =
            InstallReport::read(["install-daemon: linger yes", "install-daemon: claude /c"]);
        assert_eq!(
            (service.starter, service.left_to_do()),
            (Starter::Systemd, 0)
        );
    }

    /// A setup whose install ended on a server without systemd, with `boot` from its report.
    fn installed_alone(boot: &str) -> AppState {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        if let Some(setup) = state.machines.setup.as_mut() {
            let report = InstallReport::read(alone(boot).iter().map(String::as_str));
            setup.stage = SetupStage::Installed(report);
        }
        state
    }

    fn report_of(state: &AppState) -> Option<&InstallReport> {
        match &state.machines.setup.as_ref()?.stage {
            SetupStage::Installed(report) => Some(report),
            _ => None,
        }
    }

    #[test]
    fn the_crontab_line_goes_in_only_once_the_report_says_nothing_starts_the_daemon_at_boot() {
        let mut without_start = installed_alone("no");

        let run = add_boot_start(&mut without_start);
        let twice = add_boot_start(&mut without_start);

        assert_eq!(
            run,
            Some(SetupRun::AddBootStart {
                host: "lab".to_owned()
            })
        );
        assert_eq!(twice, None, "one runs at a time");
        assert_eq!(add_boot_start(&mut installed_alone("yes")), None);
        assert_eq!(add_boot_start(&mut installed_alone("unavailable")), None);
    }

    fn stderr(text: &str) -> OutputLine {
        OutputLine {
            stream: OutputStream::Stderr,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_crontab_line_that_fails_says_why_until_the_next_try() {
        let mut state = installed_alone("no");
        add_boot_start(&mut state);
        let said = vec![stderr(
            "install-daemon: cannot read your crontab, so it stays as it is: denied",
        )];

        checked(&mut state, "lab", script_result(Some(1), said));
        let failed = report_of(&state).cloned();
        add_boot_start(&mut state);

        let failed = failed.unwrap();
        assert_eq!(
            failed.failure.as_deref(),
            Some("cannot read your crontab, so it stays as it is: denied")
        );
        assert!(!failed.checking);
        assert_eq!(failed.boot, Some(BootStart::No), "the report stays");
        assert_eq!(
            report_of(&state).and_then(|report| report.failure.clone()),
            None
        );
    }

    #[test]
    fn a_run_that_fails_without_a_word_says_how_it_ended() {
        let quiet = vec![OutputLine {
            stream: OutputStream::Stdout,
            text: "install-daemon: checking".to_owned(),
        }];

        assert_eq!(
            script_result(Some(4), quiet.clone()),
            Err("The script ended with code 4.".to_owned())
        );
        assert_eq!(
            script_result(Some(255), vec![stderr("ssh: connect to host lab: refused")]),
            Err("ssh: connect to host lab: refused".to_owned())
        );
        assert_eq!(
            script_result(Some(0), quiet),
            Ok(vec!["install-daemon: checking".to_owned()])
        );
    }

    #[test]
    fn the_words_after_service_and_boot_are_pinned() {
        let starters: Vec<&str> = Starter::VARIANTS.iter().map(|&word| word.into()).collect();
        let boots: Vec<&str> = BootStart::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();

        assert_eq!(starters, ["systemd", "none"]);
        assert_eq!(boots, ["yes", "no", "unavailable"]);
    }

    #[test]
    fn each_line_the_setup_reads_starts_as_say_writes_it() {
        let read = [
            CHECKING_LINE,
            INSTALLED_LINE,
            STARTING_LINE,
            STARTING_ALONE_LINE,
            RUNS_LINE,
            LINGER_LINE,
            CLAUDE_LINE,
            SERVICE_LINE,
            BOOT_LINE,
            START_LINE,
            CRON_LINE,
        ];
        for line in read {
            assert!(line.starts_with(SAY_PREFIX), "{line}");
        }
    }

    #[test]
    fn a_daemon_found_on_the_server_adds_it_with_its_path() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());

        let change = looked(
            &mut state,
            "lab",
            Ok(server(
                Some("/home/someone/.local/bin/yetack"),
                "Linux x86_64",
            )),
            &carried(),
        );

        assert_eq!(
            change,
            Some(MachineChange::Add(MachineConfig {
                remote_command: "/home/someone/.local/bin/yetack".to_owned(),
                ..lab()
            }))
        );
        assert_eq!(state.machines.setup, None);
    }

    #[test]
    fn without_a_daemon_the_app_offers_to_install_only_the_one_it_has() {
        let both = CarriedDaemons {
            aarch64: Some("/opt/app/yetack-aarch64-linux".into()),
            ..carried()
        };
        for (system, carried, offers) in [
            ("Linux x86_64", carried(), true),
            ("Linux aarch64", carried(), false),
            ("Linux aarch64", both, true),
            ("Linux x86_64", CarriedDaemons::default(), false),
        ] {
            let mut state = state_with(Vec::new());
            start(&mut state, lab());

            let change = looked(&mut state, "lab", Ok(server(None, system)), &carried);

            assert_eq!(change, None);
            assert_eq!(
                matches!(stage(&state), Some(SetupStage::Offer { .. })),
                offers,
                "{system}, carried {carried:?}"
            );
        }
    }

    #[test]
    fn a_result_for_a_setup_the_user_closed_does_not_reach_the_next_one() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        close(&mut state);
        let lab2 = MachineConfig {
            name: "lab2".to_owned(),
            host: "lab2".to_owned(),
            ..lab()
        };
        start(&mut state, lab2);

        let change = looked(
            &mut state,
            "lab",
            Ok(server(
                Some("/home/someone/.local/bin/yetack"),
                "Linux x86_64",
            )),
            &carried(),
        );

        assert_eq!(change, None);
        assert_eq!(
            stage(&state),
            Some(&SetupStage::Looking),
            "lab2 waits for its own look"
        );
    }

    #[test]
    fn a_lost_connection_during_the_install_says_so_rather_than_the_last_step() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        looked(
            &mut state,
            "lab",
            Ok(server(None, "Linux x86_64")),
            &carried(),
        );
        install(&mut state);
        let said = lines(&["install-daemon: starting the service"]);

        installed(&mut state, "lab", Some(255), &said);

        let Some(SetupStage::Failed { message, .. }) = stage(&state) else {
            panic!("expected a failure, got {:?}", stage(&state));
        };
        assert!(
            message.starts_with("The connection to the server was lost"),
            "{message}"
        );
    }

    #[test]
    fn an_install_moves_through_its_steps_and_adds_the_server_with_the_installed_path() {
        let mut state = state_with(Vec::new());
        start(&mut state, lab());
        looked(
            &mut state,
            "lab",
            Ok(server(None, "Linux x86_64")),
            &carried(),
        );

        assert_eq!(
            install(&mut state),
            Some(SetupRun::Install {
                host: "lab".to_owned(),
                cpu: LinuxCpu::X86_64,
            })
        );
        let half = SendProgress { sent: 5, total: 10 };
        sending(&mut state, "lab", half);
        assert_eq!(
            stage(&state),
            Some(&SetupStage::Installing {
                step: InstallStep::Send,
                sent: Some(half)
            })
        );
        let said = lines(&[
            "install-daemon: checking /home/someone/.cache/yetack/sent/yetack.a1",
            "install-daemon: installed /home/someone/.local/share/yetack/bin/yetack",
            "install-daemon: starting the service",
            "install-daemon: the daemon runs",
            "install-daemon: linger no",
            "install-daemon: claude missing",
        ]);
        for line in &said {
            output(&mut state, "lab", line);
        }
        let change = installed(&mut state, "lab", Some(0), &said);

        assert_eq!(
            change,
            Some(MachineChange::Add(MachineConfig {
                remote_command: "/home/someone/.local/share/yetack/bin/yetack".to_owned(),
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
        looked(
            &mut state,
            "lab",
            Ok(server(None, "Linux x86_64")),
            &carried(),
        );
        install(&mut state);
        let said = lines(&[
            "install-daemon: checking /tmp/x",
            "install-daemon: the binary on this machine is not the one the app sent: its checksum differs",
        ]);
        output(&mut state, "lab", &said[0]);

        let change = installed(&mut state, "lab", Some(1), &said);

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
            "lab",
            Ok(lines(&[
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
        looked(
            &mut state,
            "lab",
            Ok(server(None, "Linux x86_64")),
            &carried(),
        );
        install(&mut state);

        close(&mut state);
        assert!(state.machines.setup.is_some(), "the install goes on");

        installed(&mut state, "lab", Some(1), &[]);
        close(&mut state);
        assert_eq!(state.machines.setup, None);
    }
}

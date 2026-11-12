//! Updating a server's daemon from the app: how a daemon's protocol compares with the app's,
//! where an update stands, and what its screens show. The connection task runs the script and
//! reports here.

use std::cmp::Ordering;
use std::collections::VecDeque;

use cockpit_client::{ConnectError, SessionError};
use cockpit_protocol::{BuildCommit, PROTOCOL_VERSION};

use super::app_state::{AppState, MachineState};
use super::ui_state::Dialog;
use crate::config::MachineConfig;
use crate::daemon_label::DaemonPlace;
use crate::machine::MachineId;

/// The script's lines that move the steps on, as `say` in `contrib/update-daemon.sh` prints them.
pub const BUILDING_LINE: &str = "update-daemon: building";
pub const RESTARTING_LINE: &str = "update-daemon: restarting the daemon";
pub const DAEMON_RUNS_LINE: &str = "update-daemon: the daemon runs";

/// The most lines of output kept. A build prints a line per crate.
const OUTPUT_LINES: usize = 500;

/// How many of the last lines a failure shows.
const FAILURE_LINES: usize = 30;

/// How a daemon's protocol compares with this app's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolGap {
    Older,
    Newer,
}

/// What an update runs on its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlan {
    pub host: String,
    /// The absolute path of `agents-cockpit` on the host.
    pub remote_command: String,
    pub source_dir: Option<String>,
    /// The commit this app was built from.
    pub commit: BuildCommit,
}

/// A daemon that refused the app for its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionGap {
    pub kind: ProtocolGap,
    /// The refusal, as the daemon or the client worded it.
    pub details: String,
    /// `None` for a newer daemon, and when the app does not know the commit it was built from.
    pub plan: Option<UpdatePlan>,
}

/// The daemon's protocol, from a first connection that failed, when it differs from the app's. An
/// older daemon counts only over SSH, since the app updates no other: any other daemon keeps the
/// "No daemon answers" screen. `plan` stays `None` until the caller knows it.
pub fn protocol_gap(place: DaemonPlace, error: &SessionError) -> Option<VersionGap> {
    let SessionError::Connect(connect) = error else {
        return None;
    };
    let kind = match connect.daemon_protocol()?.cmp(&PROTOCOL_VERSION) {
        Ordering::Less if place == DaemonPlace::Ssh => ProtocolGap::Older,
        Ordering::Greater => ProtocolGap::Newer,
        Ordering::Less | Ordering::Equal => return None,
    };
    let details = match connect {
        ConnectError::Refused { reason } => reason.clone(),
        other => other.to_string(),
    };
    Some(VersionGap {
        kind,
        details,
        plan: None,
    })
}

/// The steps the running update's screen lists, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpdateStep {
    GetVersion,
    Build,
    Restart,
    Reconnect,
}

impl UpdateStep {
    /// Every step, as the screen lists them.
    pub const ALL: [Self; 4] = [
        Self::GetVersion,
        Self::Build,
        Self::Restart,
        Self::Reconnect,
    ];

    /// The step a line of the script's output starts, when it starts one.
    fn from_line(line: &str) -> Option<Self> {
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

/// Which of the script's streams a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub stream: OutputStream,
    pub text: String,
}

/// What the screen of an update under way shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProgress {
    pub plan: UpdatePlan,
    pub step: UpdateStep,
    /// The script's output, the oldest lines dropped.
    pub output: VecDeque<String>,
    /// What the script wrote to stderr, which says why it stopped, the oldest lines dropped.
    errors: Vec<String>,
    /// "Cancel the update" was pressed.
    pub cancel_asked: bool,
}

impl UpdateProgress {
    fn new(plan: UpdatePlan) -> Self {
        Self {
            plan,
            step: UpdateStep::GetVersion,
            output: VecDeque::new(),
            errors: Vec::new(),
            cancel_asked: false,
        }
    }

    /// Cancelling stops the update before it installs, which it does right before the restart.
    pub fn offers_cancel(&self) -> bool {
        self.step < UpdateStep::Restart
    }
}

/// How an update that did not succeed ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFailure {
    /// Agents are busy or may hold a scheduled job: nothing changed.
    Refused,
    /// The update failed before it changed anything.
    Failed,
    /// The user cancelled it, and it stopped before it changed anything.
    Cancelled,
    /// The install or the restart failed: the daemon may be down.
    InstallFailed,
    /// The new daemon did not start, and the previous version runs again.
    RolledBack,
    /// Another update holds the server's lock, such as one the user left running.
    AlreadyRunning,
    /// ssh lost the connection, or the exit status means nothing to the script: the app cannot
    /// tell how the update went.
    Unknown,
}

impl UpdateFailure {
    /// The failure an exit status means, `None` for success. See the script's header.
    pub fn from_exit_code(code: Option<i32>) -> Option<Self> {
        match code {
            Some(0) => None,
            Some(3) => Some(Self::Refused),
            Some(1) => Some(Self::Failed),
            Some(4) => Some(Self::InstallFailed),
            Some(5) => Some(Self::RolledBack),
            Some(6) => Some(Self::AlreadyRunning),
            Some(_) | None => Some(Self::Unknown),
        }
    }

    /// What the sidebar says of a machine whose update ended this way.
    fn sidebar_text(self) -> &'static str {
        match self {
            Self::Refused | Self::Failed | Self::Cancelled | Self::RolledBack => "Was not updated.",
            Self::InstallFailed => "Update failed during the restart.",
            Self::AlreadyRunning => "An update is already running.",
            Self::Unknown => "Update status unknown.",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedUpdate {
    pub plan: UpdatePlan,
    pub failure: UpdateFailure,
    /// What the screen's text block shows.
    pub message: String,
}

impl FailedUpdate {
    /// The agent the hint under a refusal names: the first row of the script's listing.
    pub fn first_agent(&self) -> Option<&str> {
        self.message.lines().find_map(|line| {
            let first = line.split_whitespace().next()?;
            first
                .chars()
                .all(|character| character.is_ascii_digit())
                .then_some(first)
        })
    }
}

/// An update and where it stands. It lives in the machine's state, so it outlasts the connection's
/// attempts, which an old daemon keeps refusing while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonUpdate {
    Running(UpdateProgress),
    /// The script ended well: the app connects to the new daemon.
    Reconnecting(UpdateProgress),
    Failed(FailedUpdate),
}

/// A failed attempt to connect. The gap, if any, replaces the last one. An attempt that fails once
/// the update is done ends its screen, so the machine's own one says why. A daemon that is now
/// newer ends a failure's screen, whose "Update again" would move the server backwards.
pub fn attempt_failed(
    state: &mut AppState,
    machine: MachineId,
    gap: Option<VersionGap>,
    plan: Option<UpdatePlan>,
) {
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    known.version_gap = gap.map(|mut gap| {
        if gap.kind == ProtocolGap::Older {
            gap.plan = plan;
        }
        gap
    });
    let newer = known
        .version_gap
        .as_ref()
        .is_some_and(|gap| gap.kind == ProtocolGap::Newer);
    known.update.take_if(|update| match update {
        DaemonUpdate::Running(_) => false,
        DaemonUpdate::Reconnecting(_) => true,
        DaemonUpdate::Failed(_) => newer,
    });
    close_empty_dialog(state, machine);
}

/// The first connection to the new daemon, or to one that was updated another way: the update's
/// screen closes.
pub fn connected(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine) {
        known.update = None;
        known.version_gap = None;
    }
    state
        .ui
        .dialog
        .take_if(|dialog| shows_machine(dialog, machine));
}

/// Starts an update from the older-version card or a failure's "Update again". Returns what to
/// run, `None` when one runs already or the app has nothing to run.
pub fn start(state: &mut AppState, machine: MachineId) -> Option<UpdatePlan> {
    let known = state.machines.get_mut(machine)?;
    let plan = match (&known.update, &known.version_gap) {
        (Some(DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_)), _) => return None,
        (Some(DaemonUpdate::Failed(failed)), _) => failed.plan.clone(),
        (None, Some(gap)) => gap.plan.clone()?,
        (None, None) => return None,
    };
    known.update = Some(DaemonUpdate::Running(UpdateProgress::new(plan.clone())));
    Some(plan)
}

/// Adds lines the script wrote, and moves the steps on.
pub fn output(state: &mut AppState, machine: MachineId, lines: Vec<OutputLine>) {
    let Some(progress) = running_mut(state, machine) else {
        return;
    };
    for line in lines {
        if let Some(step) = UpdateStep::from_line(&line.text) {
            progress.step = progress.step.max(step);
        }
        if line.stream == OutputStream::Stderr {
            progress.errors.push(line.text.clone());
        }
        progress.output.push_back(line.text);
    }
    while progress.output.len() > OUTPUT_LINES {
        progress.output.pop_front();
    }
    let dropped = progress.errors.len().saturating_sub(OUTPUT_LINES);
    progress.errors.drain(..dropped);
}

/// The script ended. Returns whether the app should try the machine again at once.
pub fn finished(state: &mut AppState, machine: MachineId, code: Option<i32>) -> bool {
    let Some(known) = state.machines.get_mut(machine) else {
        return false;
    };
    let Some(DaemonUpdate::Running(mut progress)) = known
        .update
        .take_if(|update| matches!(update, DaemonUpdate::Running(_)))
    else {
        return false;
    };
    match UpdateFailure::from_exit_code(code) {
        None => {
            progress.step = UpdateStep::Reconnect;
            known.update = Some(DaemonUpdate::Reconnecting(progress));
            true
        }
        Some(failure) => {
            known.update = Some(DaemonUpdate::Failed(failure_of(progress, failure)));
            false
        }
    }
}

/// A refusal or a rollback is what the script said. A failure also shows the end of the build's
/// output. Another update running needs no text beyond its card's.
fn failure_of(progress: UpdateProgress, failure: UpdateFailure) -> FailedUpdate {
    let failure = match failure {
        UpdateFailure::Failed if progress.cancel_asked => UpdateFailure::Cancelled,
        other => other,
    };
    let message = match failure {
        UpdateFailure::Refused | UpdateFailure::RolledBack => progress.errors.join("\n"),
        UpdateFailure::AlreadyRunning => String::new(),
        UpdateFailure::Failed
        | UpdateFailure::Cancelled
        | UpdateFailure::InstallFailed
        | UpdateFailure::Unknown => {
            let tail = progress.output.len().saturating_sub(FAILURE_LINES);
            let last: Vec<&str> = progress
                .output
                .iter()
                .skip(tail)
                .map(String::as_str)
                .collect();
            last.join("\n")
        }
    };
    FailedUpdate {
        plan: progress.plan,
        failure,
        message,
    }
}

/// "Cancel the update". Returns what to ask the server, once.
pub fn ask_cancel(state: &mut AppState, machine: MachineId) -> Option<UpdatePlan> {
    let progress = running_mut(state, machine)?;
    if progress.cancel_asked || !progress.offers_cancel() {
        return None;
    }
    progress.cancel_asked = true;
    Some(progress.plan.clone())
}

/// The server could not be asked to cancel: the button comes back, and the output says why.
pub fn cancel_failed(state: &mut AppState, machine: MachineId, error: String) {
    if let Some(progress) = running_mut(state, machine) {
        progress.cancel_asked = false;
        progress.output.push_back(error);
    }
}

/// The server's config changed: the next update runs with it. An update under way keeps its
/// plan, since its cancel must reach the server it runs on.
pub fn follow_config(known: &mut MachineState, config: &MachineConfig) {
    let gap_plan = known.version_gap.as_mut().and_then(|gap| gap.plan.as_mut());
    let failed_plan = match &mut known.update {
        Some(DaemonUpdate::Failed(failed)) => Some(&mut failed.plan),
        Some(DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_)) | None => None,
    };
    let plans = gap_plan.into_iter().chain(failed_plan);
    for plan in plans {
        config.host.clone_into(&mut plan.host);
        config.remote_command.clone_into(&mut plan.remote_command);
        plan.source_dir.clone_from(&config.source_dir);
    }
}

/// "Back" on a failure: the card that offers the update again, if the daemon still refuses.
pub fn back(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine)
        && matches!(known.update, Some(DaemonUpdate::Failed(_)))
    {
        known.update = None;
    }
    close_empty_dialog(state, machine);
}

fn running_mut(state: &mut AppState, machine: MachineId) -> Option<&mut UpdateProgress> {
    match &mut state.machines.get_mut(machine)?.update {
        Some(DaemonUpdate::Running(progress)) => Some(progress),
        Some(DaemonUpdate::Reconnecting(_) | DaemonUpdate::Failed(_)) | None => None,
    }
}

/// Whether a dialog shows `machine`'s update screens.
fn shows_machine(dialog: &Dialog, machine: MachineId) -> bool {
    match dialog {
        Dialog::DaemonUpdate(shown) => *shown == machine,
        Dialog::NoDaemon => machine == MachineId::THIS,
        Dialog::Switcher
        | Dialog::NewAgent(_)
        | Dialog::OpenFile(_)
        | Dialog::CloseAgent(_)
        | Dialog::StopAgent(_)
        | Dialog::AgentMap(_)
        | Dialog::Accounts(_)
        | Dialog::AddAccount(_)
        | Dialog::Machines(_)
        | Dialog::ImageView(_)
        | Dialog::WorkingFolder(_)
        | Dialog::Settings(_) => false,
    }
}

/// A server's dialog with nothing left to show, once the daemon no longer refuses the app.
fn close_empty_dialog(state: &mut AppState, machine: MachineId) {
    let nothing_to_show = state
        .machines
        .get(machine)
        .is_none_or(|known| known.update.is_none() && known.version_gap.is_none());
    if nothing_to_show {
        state
            .ui
            .dialog
            .take_if(|dialog| matches!(dialog, Dialog::DaemonUpdate(shown) if *shown == machine));
    }
}

/// Opens a machine's update screens: the question about this computer, or a server's dialog.
pub fn open(state: &mut AppState, machine: MachineId) {
    state.ui.popover = None;
    state.ui.dialog = Some(if machine == MachineId::THIS {
        Dialog::NoDaemon
    } else {
        Dialog::DaemonUpdate(machine)
    });
}

/// What the sidebar says of a machine with an update to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateLine {
    pub text: &'static str,
    /// The link that opens the update's screens.
    pub link: &'static str,
    /// The daemon answers no request, so the panel offers "Retry now".
    pub down: bool,
}

/// The sidebar's line for `known`, when it has an update to show.
pub fn update_line(known: &MachineState) -> Option<UpdateLine> {
    let line = |text, link, down| UpdateLine { text, link, down };
    match (&known.update, &known.version_gap) {
        (Some(DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_)), _) => {
            Some(line("Updating…", "Show", false))
        }
        (Some(DaemonUpdate::Failed(failed)), _) => {
            Some(line(failed.failure.sidebar_text(), "Show", true))
        }
        (None, Some(gap)) => Some(match (gap.kind, &gap.plan) {
            (ProtocolGap::Older, Some(_)) => line("Runs an older version.", "Update", true),
            (ProtocolGap::Older, None) => line("Runs an older version.", "Details", true),
            (ProtocolGap::Newer, _) => line("Runs a newer version.", "Details", true),
        }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::app_state::tests::state_with;
    use crate::state::{ConnectionState, Drafts, MachineState, machines};

    fn build_box() -> MachineId {
        MachineId::new(1)
    }

    fn plan() -> UpdatePlan {
        UpdatePlan {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
            commit: BuildCommit::new("3f9c2e1a7b4d5c6e".to_owned()),
        }
    }

    fn refused(reason: &str) -> SessionError {
        SessionError::Connect(ConnectError::Refused {
            reason: reason.to_owned(),
        })
    }

    fn gap_over_ssh(reason: &str) -> Option<VersionGap> {
        protocol_gap(DaemonPlace::Ssh, &refused(reason))
    }

    fn older() -> VersionGap {
        VersionGap {
            kind: ProtocolGap::Older,
            details: "the daemon speaks protocol 1, the client 4".to_owned(),
            plan: Some(plan()),
        }
    }

    fn with_server() -> AppState {
        let mut state = state_with(Vec::new());
        state.machines.others.insert(
            build_box(),
            MachineState::new(DaemonLabel::ssh("build-box".to_owned())),
        );
        state
    }

    fn with_older_server() -> AppState {
        let mut state = with_server();
        state
            .machines
            .others
            .get_mut(&build_box())
            .unwrap()
            .version_gap = Some(older());
        state
    }

    fn known(state: &AppState) -> &MachineState {
        state.machines.get(build_box()).unwrap()
    }

    fn update(state: &AppState) -> Option<&DaemonUpdate> {
        known(state).update.as_ref()
    }

    fn progress(state: &AppState) -> &UpdateProgress {
        match update(state) {
            Some(DaemonUpdate::Running(progress) | DaemonUpdate::Reconnecting(progress)) => {
                progress
            }
            other => panic!("expected an update under way, got {other:?}"),
        }
    }

    fn line(stream: OutputStream, text: &str) -> OutputLine {
        OutputLine {
            stream,
            text: text.to_owned(),
        }
    }

    fn stdout(state: &mut AppState, text: &str) {
        output(state, build_box(), vec![line(OutputStream::Stdout, text)]);
    }

    #[test]
    fn an_older_daemon_over_ssh_is_older_and_a_newer_one_is_newer() {
        let older = gap_over_ssh("the daemon speaks protocol 1, the client 4").unwrap();
        let newer = gap_over_ssh(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ))
        .unwrap();

        assert_eq!(older.kind, ProtocolGap::Older);
        assert_eq!(older.details, "the daemon speaks protocol 1, the client 4");
        assert_eq!(newer.kind, ProtocolGap::Newer);
    }

    #[test]
    fn a_version_mismatch_counts_the_same_way() {
        let mismatch = SessionError::Connect(ConnectError::VersionMismatch {
            daemon: PROTOCOL_VERSION + 1,
        });

        let gap = protocol_gap(DaemonPlace::Ssh, &mismatch).unwrap();

        assert_eq!(gap.kind, ProtocolGap::Newer);
        assert_eq!(
            gap.details,
            format!(
                "the daemon speaks protocol {}, this client {PROTOCOL_VERSION}",
                PROTOCOL_VERSION + 1
            )
        );
    }

    #[test]
    fn only_a_daemon_over_ssh_is_offered_an_update() {
        let older_daemon = refused("the daemon speaks protocol 1, the client 4");
        let newer_daemon = refused(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ));

        for place in [
            DaemonPlace::Socket,
            DaemonPlace::Service,
            DaemonPlace::InApp,
        ] {
            assert_eq!(protocol_gap(place, &older_daemon), None, "{place:?}");
            assert_eq!(
                protocol_gap(place, &newer_daemon).map(|gap| gap.kind),
                Some(ProtocolGap::Newer),
                "{place:?}"
            );
        }
    }

    #[test]
    fn a_daemon_of_the_same_or_an_unknown_protocol_keeps_todays_screen() {
        let same = refused(&format!(
            "the daemon speaks protocol {PROTOCOL_VERSION}, the client {PROTOCOL_VERSION}"
        ));
        let other_error = SessionError::Connect(ConnectError::Ssh {
            stderr: "Connection timed out".to_owned(),
        });
        let socket_error = SessionError::Connect(ConnectError::Socket {
            path: "/run/agents-cockpit.sock".into(),
            source: io::Error::from(io::ErrorKind::NotFound),
        });

        assert_eq!(protocol_gap(DaemonPlace::Ssh, &same), None);
        assert_eq!(gap_over_ssh("the first message must be a hello"), None);
        assert_eq!(protocol_gap(DaemonPlace::Ssh, &other_error), None);
        assert_eq!(protocol_gap(DaemonPlace::Socket, &socket_error), None);
    }

    #[test]
    fn the_scripts_lines_move_the_steps_and_never_back() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        assert_eq!(progress(&state).step, UpdateStep::GetVersion);

        stdout(&mut state, "update-daemon: building 3f9c2e1");
        assert_eq!(progress(&state).step, UpdateStep::Build);
        stdout(&mut state, "   Compiling cockpit_daemon v0.1.0");
        assert_eq!(progress(&state).step, UpdateStep::Build);

        stdout(&mut state, "update-daemon: restarting the daemon");
        assert_eq!(progress(&state).step, UpdateStep::Restart);
        stdout(&mut state, "update-daemon: building again?");
        assert_eq!(progress(&state).step, UpdateStep::Restart, "never back");

        stdout(&mut state, "update-daemon: the daemon runs 3f9c2e1");
        assert_eq!(progress(&state).step, UpdateStep::Reconnect);
    }

    #[test]
    fn an_exit_status_means_one_failure() {
        assert_eq!(UpdateFailure::from_exit_code(Some(0)), None);
        assert_eq!(
            UpdateFailure::from_exit_code(Some(3)),
            Some(UpdateFailure::Refused)
        );
        assert_eq!(
            UpdateFailure::from_exit_code(Some(1)),
            Some(UpdateFailure::Failed)
        );
        assert_eq!(
            UpdateFailure::from_exit_code(Some(4)),
            Some(UpdateFailure::InstallFailed)
        );
        assert_eq!(
            UpdateFailure::from_exit_code(Some(5)),
            Some(UpdateFailure::RolledBack)
        );
        assert_eq!(
            UpdateFailure::from_exit_code(Some(6)),
            Some(UpdateFailure::AlreadyRunning)
        );
        assert_eq!(
            UpdateFailure::from_exit_code(Some(255)),
            Some(UpdateFailure::Unknown)
        );
        assert_eq!(
            UpdateFailure::from_exit_code(None),
            Some(UpdateFailure::Unknown)
        );
    }

    #[test]
    fn the_update_survives_the_refused_attempts_of_the_old_daemon() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: building 3f9c2e1");
        let shown = update(&state).cloned();

        for _ in 0..3 {
            state
                .machines
                .others
                .get_mut(&build_box())
                .unwrap()
                .connection = ConnectionState::Connecting { attempt: 1 };
            attempt_failed(&mut state, build_box(), Some(older()), Some(plan()));
        }

        assert_eq!(update(&state).cloned(), shown);
        assert_eq!(known(&state).version_gap, Some(older()));
    }

    #[test]
    fn an_update_survives_the_daemon_being_down_during_the_restart() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: restarting the daemon");

        attempt_failed(&mut state, build_box(), None, Some(plan()));

        assert_eq!(progress(&state).step, UpdateStep::Restart);
        assert_eq!(known(&state).version_gap, None);
    }

    #[test]
    fn a_failed_attempt_after_the_script_ended_ends_the_screen() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: the daemon runs 3f9c2e1");
        assert!(finished(&mut state, build_box(), Some(0)));
        assert_eq!(progress(&state).step, UpdateStep::Reconnect);

        attempt_failed(&mut state, build_box(), None, Some(plan()));

        assert_eq!(update(&state), None);
        assert_eq!(state.ui.dialog, None, "nothing is left to show");
    }

    #[test]
    fn a_refusal_after_the_script_ended_offers_the_update_again() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        assert!(finished(&mut state, build_box(), Some(0)));

        attempt_failed(&mut state, build_box(), Some(older()), Some(plan()));

        assert_eq!(update(&state), None);
        assert_eq!(known(&state).version_gap, Some(older()));
    }

    #[test]
    fn a_failed_update_gives_way_to_a_daemon_that_is_now_newer() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        finished(&mut state, build_box(), Some(1));
        let newer = gap_over_ssh(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ));

        attempt_failed(&mut state, build_box(), newer, Some(plan()));

        assert_eq!(update(&state), None);
        assert_eq!(
            start(&mut state, build_box()),
            None,
            "it would move the server backwards"
        );
    }

    #[test]
    fn the_first_connection_closes_the_screen() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();
        assert!(finished(&mut state, build_box(), Some(0)));

        connected(&mut state, build_box());

        assert_eq!(update(&state), None);
        assert_eq!(known(&state).version_gap, None);
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn a_connection_elsewhere_leaves_this_machines_dialog_open() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));

        connected(&mut state, MachineId::THIS);

        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(build_box())));
        assert_eq!(known(&state).version_gap, Some(older()));
    }

    #[test]
    fn a_refusal_shows_what_the_script_said_and_a_failure_the_end_of_the_output() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "   Compiling cockpit_daemon v0.1.0");
        output(
            &mut state,
            build_box(),
            vec![
                line(
                    OutputStream::Stderr,
                    "update-daemon: not restarting the daemon, these agents are busy:",
                ),
                line(
                    OutputStream::Stderr,
                    "3    working   0    12    Fix the parser",
                ),
            ],
        );
        let mut other = state.clone();

        assert!(!finished(&mut state, build_box(), Some(3)));
        let Some(DaemonUpdate::Failed(refused)) = update(&state).cloned() else {
            panic!("expected a failure");
        };
        assert_eq!(refused.failure, UpdateFailure::Refused);
        assert_eq!(
            refused.message,
            "update-daemon: not restarting the daemon, these agents are busy:\n3    working   0    12    Fix the parser"
        );
        assert_eq!(refused.first_agent(), Some("3"));

        assert!(!finished(&mut other, build_box(), Some(1)));
        let Some(DaemonUpdate::Failed(failed)) = update(&other).cloned() else {
            panic!("expected a failure");
        };
        assert_eq!(failed.failure, UpdateFailure::Failed);
        assert!(failed.message.starts_with("   Compiling cockpit_daemon"));
    }

    #[test]
    fn a_rollback_shows_what_the_script_said_and_another_update_nothing() {
        let ended = |code| {
            let mut state = with_older_server();
            start(&mut state, build_box()).unwrap();
            stdout(&mut state, "   Compiling cockpit_daemon v0.1.0");
            output(
                &mut state,
                build_box(),
                vec![line(OutputStream::Stderr, "update-daemon: made-up message")],
            );
            finished(&mut state, build_box(), Some(code));
            match update(&state) {
                Some(DaemonUpdate::Failed(failed)) => failed.message.clone(),
                other => panic!("expected a failure, got {other:?}"),
            }
        };

        assert_eq!(ended(5), "update-daemon: made-up message");
        assert_eq!(ended(6), "");
    }

    #[test]
    fn back_returns_to_the_older_card_and_update_again_reruns_the_plan() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        finished(&mut state, build_box(), Some(1));

        assert_eq!(start(&mut state, build_box()), Some(plan()));
        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));

        finished(&mut state, build_box(), Some(1));
        back(&mut state, build_box());

        assert_eq!(update(&state), None);
        assert_eq!(known(&state).version_gap, Some(older()));
    }

    #[test]
    fn back_with_no_refusal_left_closes_the_dialog() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();
        finished(&mut state, build_box(), Some(4));
        attempt_failed(&mut state, build_box(), None, Some(plan()));

        back(&mut state, build_box());

        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn with_no_commit_known_nothing_starts() {
        let mut state = with_server();
        attempt_failed(
            &mut state,
            build_box(),
            gap_over_ssh("the daemon speaks protocol 1, the client 4"),
            None,
        );

        assert_eq!(start(&mut state, build_box()), None);
        assert_eq!(update(&state), None);
    }

    #[test]
    fn with_no_commit_known_the_sidebar_offers_no_update() {
        let mut state = with_server();
        attempt_failed(
            &mut state,
            build_box(),
            gap_over_ssh("the daemon speaks protocol 1, the client 4"),
            None,
        );

        let shown = update_line(known(&state)).unwrap();

        assert_eq!(
            (shown.text, shown.link),
            ("Runs an older version.", "Details")
        );
        assert!(shown.down);
    }

    #[test]
    fn a_second_start_while_one_runs_or_reconnects_is_refused() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();

        assert_eq!(start(&mut state, build_box()), None);

        finished(&mut state, build_box(), Some(0));
        assert_eq!(start(&mut state, build_box()), None);
    }

    #[test]
    fn what_happens_to_one_machines_update_leaves_the_others_alone() {
        let gpu_box = MachineId::new(2);
        let mut state = with_older_server();
        let mut second = MachineState::new(DaemonLabel::ssh("gpu-box".to_owned()));
        second.version_gap = Some(older());
        state.machines.others.insert(gpu_box, second);
        state.ui.dialog = Some(Dialog::DaemonUpdate(gpu_box));
        start(&mut state, build_box()).unwrap();
        start(&mut state, gpu_box).unwrap();
        let untouched = state.machines.get(gpu_box).unwrap().clone();

        stdout(&mut state, "update-daemon: building 3f9c2e1");
        ask_cancel(&mut state, build_box()).unwrap();
        cancel_failed(&mut state, build_box(), "ssh: connect failed".to_owned());
        attempt_failed(&mut state, build_box(), None, Some(plan()));
        finished(&mut state, build_box(), Some(1));
        back(&mut state, build_box());
        connected(&mut state, build_box());

        assert_eq!(state.machines.get(gpu_box), Some(&untouched));
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(gpu_box)));
    }

    #[test]
    fn a_machine_removed_during_its_update_leaves_nothing_behind() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();

        machines::forget(&mut state, &mut Drafts::default(), build_box());
        stdout(&mut state, "update-daemon: building 3f9c2e1");
        cancel_failed(&mut state, build_box(), "ssh: connect failed".to_owned());

        assert_eq!(state.ui.dialog, None);
        assert_eq!(ask_cancel(&mut state, build_box()), None);
        assert!(
            !finished(&mut state, build_box(), Some(0)),
            "nothing to try again"
        );
        assert!(state.machines.get(build_box()).is_none());
    }

    #[test]
    fn a_connection_during_a_run_ends_it_and_its_last_lines_change_nothing() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: restarting the daemon");

        connected(&mut state, build_box());
        stdout(&mut state, "update-daemon: the daemon runs 3f9c2e1");

        assert_eq!(update(&state), None);
        assert_eq!(state.ui.dialog, None);
        assert!(
            !finished(&mut state, build_box(), Some(0)),
            "the app is connected already"
        );
        assert_eq!(update(&state), None);
    }

    #[test]
    fn back_during_a_run_changes_nothing() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();

        back(&mut state, build_box());

        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(build_box())));
    }

    #[test]
    fn closing_the_dialog_during_a_run_keeps_the_update() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();

        state.ui.close_dialog();
        stdout(&mut state, "update-daemon: building 3f9c2e1");

        assert_eq!(progress(&state).step, UpdateStep::Build);
        let shown = update_line(known(&state)).unwrap();
        assert_eq!((shown.text, shown.link), ("Updating…", "Show"));
    }

    #[test]
    fn a_run_keeps_its_screen_when_the_daemon_is_newer() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        let newer = gap_over_ssh(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ));

        attempt_failed(&mut state, build_box(), newer, Some(plan()));

        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));
    }

    #[test]
    fn a_newer_daemon_never_holds_a_plan() {
        let mut state = with_server();
        let newer = gap_over_ssh(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ));

        attempt_failed(&mut state, build_box(), newer, Some(plan()));

        assert_eq!(known(&state).version_gap.as_ref().unwrap().plan, None);
        assert_eq!(start(&mut state, build_box()), None);
    }

    #[test]
    fn cancel_is_asked_once_and_only_until_the_restart_starts() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: building 3f9c2e1");

        assert_eq!(ask_cancel(&mut state, build_box()), Some(plan()));
        assert_eq!(ask_cancel(&mut state, build_box()), None);

        cancel_failed(&mut state, build_box(), "ssh: connect failed".to_owned());
        assert!(!progress(&state).cancel_asked);
        assert_eq!(
            progress(&state).output.back().unwrap(),
            "ssh: connect failed"
        );

        stdout(&mut state, "update-daemon: restarting the daemon");
        assert!(!progress(&state).offers_cancel());
        assert_eq!(ask_cancel(&mut state, build_box()), None);
    }

    fn failure(state: &AppState) -> UpdateFailure {
        match update(state) {
            Some(DaemonUpdate::Failed(failed)) => failed.failure,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_cancel_the_user_asked_for_ends_as_cancelled() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: building 3f9c2e1");
        ask_cancel(&mut state, build_box()).unwrap();

        assert!(!finished(&mut state, build_box(), Some(1)));

        assert_eq!(failure(&state), UpdateFailure::Cancelled);
    }

    #[test]
    fn a_cancel_that_never_reached_the_server_ends_as_a_failure() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        ask_cancel(&mut state, build_box()).unwrap();
        cancel_failed(&mut state, build_box(), "ssh: connect failed".to_owned());

        finished(&mut state, build_box(), Some(1));

        assert_eq!(failure(&state), UpdateFailure::Failed);
    }

    #[test]
    fn a_cancel_does_not_hide_any_other_ending() {
        for (code, ending) in [
            (Some(3), UpdateFailure::Refused),
            (Some(4), UpdateFailure::InstallFailed),
            (Some(5), UpdateFailure::RolledBack),
            (Some(6), UpdateFailure::AlreadyRunning),
            (None, UpdateFailure::Unknown),
        ] {
            let mut state = with_older_server();
            start(&mut state, build_box()).unwrap();
            ask_cancel(&mut state, build_box()).unwrap();

            finished(&mut state, build_box(), code);

            assert_eq!(failure(&state), ending, "{code:?}");
        }
    }

    #[test]
    fn a_late_finish_leaves_what_the_screen_shows() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        finished(&mut state, build_box(), Some(4));
        let shown = update(&state).cloned();

        assert!(!finished(&mut state, build_box(), Some(0)));

        assert_eq!(update(&state).cloned(), shown);
    }

    #[test]
    fn a_long_stderr_keeps_its_last_lines() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        let warnings: Vec<OutputLine> = (0..=OUTPUT_LINES)
            .map(|number| line(OutputStream::Stderr, &format!("warning {number}")))
            .collect();
        output(&mut state, build_box(), warnings);
        output(
            &mut state,
            build_box(),
            vec![line(
                OutputStream::Stderr,
                "3    working   0    12    Fix it",
            )],
        );

        finished(&mut state, build_box(), Some(3));

        let Some(DaemonUpdate::Failed(refused)) = update(&state) else {
            panic!("expected a failure");
        };
        assert_eq!(refused.message.lines().count(), OUTPUT_LINES);
        assert_eq!(refused.first_agent(), Some("3"));
    }

    #[test]
    fn the_output_keeps_its_last_lines() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();

        for number in 0..=OUTPUT_LINES {
            stdout(&mut state, &format!("line {number}"));
        }

        let kept = &progress(&state).output;
        assert_eq!(kept.len(), OUTPUT_LINES);
        assert_eq!(kept.front().unwrap(), "line 1");
    }

    #[test]
    fn the_sidebar_line_follows_the_update() {
        let mut state = with_older_server();
        let text = |state: &AppState| update_line(known(state)).map(|line| (line.text, line.link));

        assert_eq!(text(&state), Some(("Runs an older version.", "Update")));
        start(&mut state, build_box()).unwrap();
        assert_eq!(text(&state), Some(("Updating…", "Show")));
        finished(&mut state, build_box(), Some(1));
        assert_eq!(text(&state), Some(("Was not updated.", "Show")));

        state.machines.others.get_mut(&build_box()).unwrap().update = None;
        state
            .machines
            .others
            .get_mut(&build_box())
            .unwrap()
            .version_gap = Some(VersionGap {
            kind: ProtocolGap::Newer,
            details: String::new(),
            plan: None,
        });
        assert_eq!(text(&state), Some(("Runs a newer version.", "Details")));

        state
            .machines
            .others
            .get_mut(&build_box())
            .unwrap()
            .version_gap = None;
        assert_eq!(text(&state), None);
    }

    #[test]
    fn the_sidebar_says_how_the_update_ended() {
        for (code, expected) in [
            (Some(1), "Was not updated."),
            (Some(3), "Was not updated."),
            (Some(4), "Update failed during the restart."),
            (Some(5), "Was not updated."),
            (Some(6), "An update is already running."),
            (Some(255), "Update status unknown."),
            (Some(42), "Update status unknown."),
            (None, "Update status unknown."),
        ] {
            let mut state = with_older_server();
            start(&mut state, build_box()).unwrap();

            finished(&mut state, build_box(), code);

            let shown = update_line(known(&state)).unwrap();
            assert_eq!((shown.text, shown.link), (expected, "Show"), "{code:?}");
            assert!(shown.down, "{code:?}");
        }
    }

    #[test]
    fn this_computers_screen_opens_as_its_question() {
        let mut state = with_server();

        open(&mut state, MachineId::THIS);
        assert_eq!(state.ui.dialog, Some(Dialog::NoDaemon));

        open(&mut state, build_box());
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(build_box())));
    }
}

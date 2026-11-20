//! Updating a machine's daemon from the app: where an update stands, what its screens say, and
//! how it ends. The connection task runs the update, the script on a server or the restart of
//! this computer's background service, and reports here.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use cockpit_daemon::{PROGRAM, SERVE_SUBCOMMAND};
use cockpit_protocol::{AgentId, ProcessId};

use super::app_state::{AppState, MachineState};
use super::daemon_probe::{DaemonProbe, ProbeTarget, ServerInstall};
use super::ui_state::Dialog;
use super::update_offer::{self, GapKind, ProgramBuild, VersionGap};
use super::update_plan::{
    BusyAgents, CarriedDaemons, JobCheck, SendProgress, ServerPlan, UpdatePlan, UpdateRun,
    UpdateStep,
};
use crate::config::MachineConfig;
use crate::machine::MachineId;

/// The most lines of output kept. A build prints a line per crate.
const OUTPUT_LINES: usize = 500;

/// The number of the next probe of a server's daemon.
static NEXT_PROBE: AtomicU64 = AtomicU64::new(1);

/// How many of the last lines a failure shows.
const FAILURE_LINES: usize = 30;

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
    /// The daemon's build when the update started, while the app was connected to it.
    pub replaces: Option<ProgramBuild>,
    /// The script's output, the oldest lines dropped.
    pub output: VecDeque<String>,
    /// What the script wrote to stderr, which says why it stopped, the oldest lines dropped.
    errors: Vec<String>,
    /// How far the send of the daemon has gone, once the app started to send it.
    pub sent: Option<SendProgress>,
    /// "Cancel the update" was pressed.
    pub cancel_asked: bool,
    /// The machine's count of connections (`MachineState::connections`) as the stop of this
    /// computer's daemon started.
    connections_at_stop: Option<u32>,
}

impl UpdateProgress {
    fn new(plan: UpdatePlan, replaces: Option<ProgramBuild>) -> Self {
        Self {
            step: plan.first_step(),
            plan,
            replaces,
            output: VecDeque::new(),
            errors: Vec::new(),
            sent: None,
            cancel_asked: false,
            connections_at_stop: None,
        }
    }

    /// Whether the app connected after the stop of this computer's daemon started, `connections`
    /// being the machine's count now. Such a connection reaches a daemon that started since.
    fn connected_since_the_stop(&self, connections: u32) -> bool {
        self.connections_at_stop
            .is_some_and(|at_stop| connections > at_stop)
    }

    /// A server's update stops before it installs, which it does right before the restart. The
    /// restart of this computer's service has nothing to cancel, and neither has a send: no
    /// script runs on the server yet, and the send takes seconds.
    pub fn offers_cancel(&self) -> bool {
        match self.plan {
            UpdatePlan::Server(_) => {
                self.step != UpdateStep::Send && self.step < UpdateStep::Restart
            }
            UpdatePlan::Service(_) => false,
        }
    }
}

/// How an update that did not succeed ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateFailure {
    /// Agents are busy, or on a server may hold a scheduled job: nothing changed. On this
    /// computer, an agent started to run something since the update began.
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
    /// The daemon that holds this computer's state folder did not stop: it runs on.
    NotStopped { by_hand: Option<HandStop> },
    /// The daemon stopped, and the service did not start: no daemon runs.
    NotStarted,
    /// The service started, and the daemon that answers runs another build: another program
    /// restarted the old one, such as a systemd unit that restarts it whenever it stops.
    OtherBuildAnswers,
    /// Resting agents of this computer's daemon may hold scheduled jobs, which a restart would
    /// lose (`MachineState::job_holders`): nothing changed.
    HoldsJobs,
}

impl UpdateFailure {
    /// The failure an exit status of the script means, `None` for success. See the script's
    /// header.
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
            Self::Refused
            | Self::Failed
            | Self::Cancelled
            | Self::RolledBack
            | Self::NotStopped { .. }
            | Self::OtherBuildAnswers
            | Self::HoldsJobs => "Was not updated.",
            Self::InstallFailed | Self::NotStarted => "Update failed during the restart.",
            Self::AlreadyRunning => "An update is already running.",
            Self::Unknown => "Update status unknown.",
        }
    }
}

/// What the user stops by hand before a retry, when the app could not stop the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandStop {
    /// The daemon's process, which did not stop, took no signal, or could not be waited for.
    Process(ProcessId),
    /// A daemon older than the record of its process holds the state folder, so the app cannot
    /// tell which process it is. Once it is gone, the next daemon records its own.
    Unrecorded,
    /// The record names a process that no longer runs, or may be a later one with its pid: the
    /// app signals no process it cannot tell is the daemon.
    Unverified,
}

/// Why the daemon that holds this computer's state folder did not stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopFailure {
    /// The error, as the daemon's code words it.
    pub reason: String,
    pub by_hand: Option<HandStop>,
}

/// How the restart of this computer's background service failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartFailure {
    /// An agent started to run something since the update began: nothing was stopped.
    Busy,
    /// These resting agents may hold scheduled jobs: nothing was stopped.
    HoldsJobs(Vec<AgentId>),
    NotStopped(StopFailure),
    NotStarted {
        error: String,
        /// The lines the service wrote to its log as it failed to start.
        log: Option<String>,
    },
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

    /// The agent that the hint under a server's refusal tells how to stop, `<id>` when the
    /// listing names none. A refusal on this computer lists its agents instead.
    pub fn stop_hint(&self) -> Option<&str> {
        (self.failure == UpdateFailure::Refused && matches!(self.plan, UpdatePlan::Server(_)))
            .then(|| self.first_agent().unwrap_or("<id>"))
    }

    /// Whether the screen offers "Update now": the update refused for busy agents or scheduled
    /// jobs.
    pub fn offers_update_now(&self) -> bool {
        matches!(
            self.failure,
            UpdateFailure::Refused | UpdateFailure::HoldsJobs
        )
    }
}

/// An update and where it stands. It lives in the machine's state, so it outlasts the connection's
/// attempts, which an old daemon keeps refusing while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonUpdate {
    /// "Update when they are idle": the update starts once none of the machine's agents is busy.
    Waiting,
    Running(UpdateProgress),
    /// The update ended well: the app connects to the new daemon.
    Reconnecting(UpdateProgress),
    Failed(FailedUpdate),
}

impl DaemonUpdate {
    /// Whether the update waits for idle agents, which leaves the machine in use meanwhile.
    pub fn is_waiting(&self) -> bool {
        matches!(self, Self::Waiting)
    }
}

/// A failed attempt to connect. The gap, if any, replaces the last one. An attempt that fails once
/// a restart is done ends the update's screen, so the machine's own one says why, unless this
/// computer's restarted service is refused for an older protocol: another program restarted the
/// old daemon. A daemon that is now newer ends a failure's screen, whose "Update again" would move
/// it backwards, and a wait for idle agents.
pub fn attempt_failed(state: &mut AppState, machine: MachineId, gap: Option<VersionGap>) {
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    known.version_gap = gap;
    known.update = match (known.update.take(), known.version_gap.as_ref()) {
        (Some(DaemonUpdate::Reconnecting(progress)), Some(gap)) => after_restart(progress, gap),
        (Some(DaemonUpdate::Reconnecting(_)), None) => None,
        (Some(DaemonUpdate::Failed(_) | DaemonUpdate::Waiting), Some(gap))
            if gap.kind.is_newer() =>
        {
            None
        }
        (kept, _) => kept,
    };
    close_empty_dialog(state, machine);
}

/// The first connection to `machine`'s daemon after a refusal or a drop. Its build, already in the
/// machine's state, tells whether it still needs an update.
///
/// A daemon of this app's build ends any update: a wait, a failure, or one under way, which
/// reached its end or was overtaken. Otherwise a server's update runs on until its script ends,
/// since the app may have reconnected to the old daemon. Once this computer's service stopped, the
/// daemon that answers is a new one: of another build, another program restarted the old daemon.
/// A connection made during the stop is judged once the restart ends ([`restart_ended`]).
pub fn connected(state: &mut AppState, machine: MachineId) {
    let app = state.app_build.clone();
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    known.connections = known.connections.saturating_add(1);
    known.version_gap = known.daemon_version.clone().and_then(|version| {
        let daemon = ProgramBuild {
            version,
            commit: known.daemon_commit.clone(),
        };
        update_offer::build_gap(&daemon, &app)
    });
    known.update = match (known.update.take(), known.version_gap.as_ref()) {
        (None, _) | (Some(_), None) => None,
        (Some(DaemonUpdate::Waiting), Some(gap)) => {
            (gap.kind == GapKind::OtherBuild).then_some(DaemonUpdate::Waiting)
        }
        (Some(DaemonUpdate::Running(progress)), Some(gap)) => match progress.plan {
            UpdatePlan::Service(_) if progress.step >= UpdateStep::Start => {
                after_restart(progress, gap)
            }
            UpdatePlan::Server(_) | UpdatePlan::Service(_) => Some(DaemonUpdate::Running(progress)),
        },
        (Some(DaemonUpdate::Reconnecting(progress)), Some(gap)) => after_restart(progress, gap),
        (Some(DaemonUpdate::Failed(_)), Some(_)) => None,
    };
    if machine == MachineId::THIS {
        state
            .ui
            .dialog
            .take_if(|dialog| matches!(dialog, Dialog::NoDaemon));
    }
    close_empty_dialog(state, machine);
}

/// What follows a restart once the daemon that answers is not of this app's build. A server's
/// update is over: its script said so. This computer's service runs this app's own program, so a
/// daemon of another, older build is another program's.
fn after_restart(progress: UpdateProgress, gap: &VersionGap) -> Option<DaemonUpdate> {
    match progress.plan {
        UpdatePlan::Service(_) if !gap.kind.is_newer() => {
            Some(DaemonUpdate::Failed(FailedUpdate {
                plan: progress.plan,
                failure: UpdateFailure::OtherBuildAnswers,
                message: gap.details.clone(),
            }))
        }
        UpdatePlan::Server(_) | UpdatePlan::Service(_) => None,
    }
}

/// "Update" on a card, or "Update again" or "Retry" on a failure. Returns the update to run:
/// `None` when one waits or runs already, when the app reads the jobs first or has no update to
/// run, or when the user answers first, which the confirmation asks:
/// - a machine the app is connected to has busy agents;
/// - this computer's service runs a daemon the app cannot list the agents of ([`agents_unseen`]).
///
/// A server the app does not reach shows no agents to ask about: its script checks them itself.
/// Before a restart of this computer's service while the app is connected, the connection task
/// reads the raw logs ([`jobs_to_check`]), then calls [`jobs_checked`], which calls this.
pub fn start(state: &mut AppState, machine: MachineId) -> Option<UpdateRun> {
    let known = state.machines.get(machine)?;
    if known.checking_jobs {
        return None;
    }
    plan_to_run(known)?;
    let ask = if known.is_connected() {
        !update_offer::busy_agents(state, machine).is_empty()
    } else {
        agents_unseen(known)
    };
    if ask {
        state.ui.popover = None;
        state.ui.dialog = Some(Dialog::ConfirmUpdate(machine));
        return None;
    }
    begin(state, machine, BusyAgents::Refuse)
}

/// Whether `known` is this computer's service while the app is not connected to its daemon, which
/// last answered: it refuses the app, or the connection dropped. The app cannot list its agents,
/// and nothing else checks them before the restart.
pub fn agents_unseen(known: &MachineState) -> bool {
    matches!(known.update_plan, Some(UpdatePlan::Service(_)))
        && !known.is_connected()
        && known.version_gap.is_some()
}

/// The raw logs to read before [`start`] restarts this computer's service, while the app is
/// connected to it: its resting agents may hold scheduled jobs. The machine shows the read as
/// under way until [`jobs_checked`] or [`job_check_dropped`]. `None` when `start` needs no read,
/// or when a read runs already: `start` then does nothing.
pub fn jobs_to_check(state: &mut AppState, machine: MachineId) -> Option<JobCheck> {
    let known = state.machines.get(machine)?;
    if known.checking_jobs || !known.is_connected() {
        return None;
    }
    let UpdatePlan::Service(plan) = plan_to_run(known)? else {
        return None;
    };
    let agents = update_offer::resting_agents(state, machine);
    state.machines.get_mut(machine)?.checking_jobs = true;
    Some(JobCheck { plan, agents })
}

/// The raw logs of [`jobs_to_check`] were read: `holders` may hold scheduled jobs, so they count
/// as busy. Returns what [`start`] returns. A read that ends once an update waits or runs, such as
/// one "Update now" started meanwhile, changes nothing.
pub fn jobs_checked(
    state: &mut AppState,
    machine: MachineId,
    holders: Vec<AgentId>,
) -> Option<UpdateRun> {
    let known = state.machines.get_mut(machine)?;
    known.checking_jobs = false;
    plan_to_run(known)?;
    known.job_holders = holders;
    start(state, machine)
}

/// Whether one of `machine`'s agents runs something, as the restart of this computer's service
/// asks once it read the raw logs. The app sees the agents only while it is connected: the ones
/// it saw before the daemon went run nothing.
pub fn runs_something_now(state: &AppState, machine: MachineId) -> bool {
    state
        .machines
        .get(machine)
        .is_some_and(MachineState::is_connected)
        && !update_offer::running_agents(state, machine).is_empty()
}

/// The read of [`jobs_to_check`] will not run, as when the machine's connection task has ended.
pub fn job_check_dropped(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine) {
        known.checking_jobs = false;
    }
}

/// "Update when they are idle", on the confirmation. Returns the update to run when none of the
/// machine's agents is busy any more.
pub fn wait_for_idle(state: &mut AppState, machine: MachineId) -> Option<UpdateRun> {
    let known = state.machines.get_mut(machine)?;
    plan_to_run(known)?;
    known.update = Some(DaemonUpdate::Waiting);
    show_progress(state, machine);
    start_when_idle(state, machine)
}

/// Starts a waiting update once none of the machine's agents runs something. The app sees them
/// only while it is connected: a machine it does not reach keeps waiting.
///
/// An agent that may hold a scheduled job does not hold the wait: its job may never end, since
/// one that runs once ends with no `CronDelete`. The update itself checks the jobs again, as it
/// starts, and refuses while one may be held (`UpdateFailure::Refused` from a server's script,
/// `UpdateFailure::HoldsJobs` on this computer), with "Update now" on its screen.
pub fn start_when_idle(state: &mut AppState, machine: MachineId) -> Option<UpdateRun> {
    let known = state.machines.get(machine)?;
    let idle = known.update.as_ref().is_some_and(DaemonUpdate::is_waiting)
        && known.is_connected()
        && update_offer::running_agents(state, machine).is_empty();
    if !idle {
        return None;
    }
    // A daemon the app may not replace ends the wait: the card says how to update it.
    let blocked = owned_by_another(known) || matches!(known.daemon_probe, DaemonProbe::Failed(_));
    if blocked {
        state.machines.get_mut(machine)?.update = None;
        return None;
    }
    // After an edit or a retry, the wait holds until the probe says the update can run.
    runnable(known)?;
    state.machines.get_mut(machine)?.update = None;
    begin(state, machine, BusyAgents::Refuse)
}

/// "Update now", on the confirmation, a waiting update, or a server's refusal for busy agents: the
/// restart ends their turns, which the new daemon resumes.
pub fn start_now(state: &mut AppState, machine: MachineId) -> Option<UpdateRun> {
    let known = state.machines.get_mut(machine)?;
    known.update.take_if(|update| update.is_waiting());
    begin(state, machine, BusyAgents::Interrupt)
}

/// "Cancel" on a waiting update: nothing starts.
pub fn cancel_wait(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine) {
        known.update.take_if(|update| update.is_waiting());
    }
    close_empty_dialog(state, machine);
}

/// The plan an update of `known` runs now: `None` when one waits or runs already, when the daemon
/// needs none or is newer, or when the app knows no way to update it.
fn plan_to_run(known: &MachineState) -> Option<UpdatePlan> {
    let gap = known.version_gap.as_ref().map(|gap| gap.kind);
    let wanted = match &known.update {
        Some(DaemonUpdate::Waiting | DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_)) => {
            false
        }
        Some(DaemonUpdate::Failed(_)) => !gap.is_some_and(GapKind::is_newer),
        None => matches!(gap, Some(GapKind::OlderProtocol | GapKind::OtherBuild)),
    };
    if wanted { runnable(known) } else { None }
}

/// The machine's plan as an update runs it. A server's waits for the probe: the app replaces only
/// a daemon it owns, at the path the probe resolved, and sends the daemon it carries only to a
/// system it runs on. Elsewhere the script builds it.
fn runnable(known: &MachineState) -> Option<UpdatePlan> {
    match known.update_plan.clone()? {
        UpdatePlan::Server(plan) => {
            let DaemonProbe::Read(install) = &known.daemon_probe else {
                return None;
            };
            install.replaceable()?;
            Some(UpdatePlan::Server(install.plan_for(plan)))
        }
        service @ UpdatePlan::Service(_) => Some(service),
    }
}

/// Whether "Update" can start now: a server's needs the probe's word that the app owns its daemon.
pub fn update_ready(known: &MachineState) -> bool {
    runnable(known).is_some()
}

/// Whether the probe found a daemon the app must not replace, which the screens then say how to
/// update by hand.
pub fn owned_by_another(known: &MachineState) -> bool {
    matches!(&known.daemon_probe, DaemonProbe::Read(install) if install.replaceable().is_none())
}

/// Whether `machine` is a server whose daemon the app has not looked at yet.
pub fn probe_due(state: &AppState, machine: MachineId) -> bool {
    state.machines.get(machine).is_some_and(|known| {
        matches!(known.update_plan, Some(UpdatePlan::Server(_)))
            && known.daemon_probe == DaemonProbe::Unknown
    })
}

/// The server and the daemon's path to probe, when `machine`'s update screens need the probe and
/// the app has not looked yet. The probe counts as under way from here.
pub fn probe_to_run(state: &mut AppState, machine: MachineId) -> Option<ProbeTarget> {
    let known = state.machines.get_mut(machine)?;
    let Some(UpdatePlan::Server(plan)) = &known.update_plan else {
        return None;
    };
    if known.daemon_probe != DaemonProbe::Unknown {
        return None;
    }
    let number = NEXT_PROBE.fetch_add(1, Ordering::Relaxed);
    let target = ProbeTarget {
        host: plan.host.clone(),
        binary: plan.remote_command.clone(),
        number,
    };
    known.daemon_probe = DaemonProbe::Probing(number);
    Some(target)
}

/// The probe a wait for idle agents needs before its update can start, once its agents are idle:
/// an edit or a retry made the app forget the last one.
pub fn probe_for_wait(state: &mut AppState, machine: MachineId) -> Option<ProbeTarget> {
    let known = state.machines.get(machine)?;
    let waits = known.update.as_ref().is_some_and(DaemonUpdate::is_waiting)
        && known.is_connected()
        && update_offer::running_agents(state, machine).is_empty();
    if !waits {
        return None;
    }
    probe_to_run(state, machine)
}

/// The daemons the app has for the machine's server plan, as it knows them once a probe ended:
/// GitHub may have said by then whether the app can download its release's.
pub fn carry(state: &mut AppState, machine: MachineId, carried: CarriedDaemons) {
    let plan = state
        .machines
        .get_mut(machine)
        .and_then(|known| known.update_plan.as_mut());
    if let Some(UpdatePlan::Server(plan)) = plan {
        plan.carried = carried;
    }
}

/// The probe of `target` ended. It counts only while it is the probe under way: an edit or a
/// retry started another since.
pub fn probed(
    state: &mut AppState,
    machine: MachineId,
    target: &ProbeTarget,
    read: Result<ServerInstall, String>,
) {
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    if known.daemon_probe != DaemonProbe::Probing(target.number) {
        return;
    }
    known.daemon_probe = match read {
        Ok(install) => DaemonProbe::Read(install),
        Err(error) => DaemonProbe::Failed(error),
    };
}

/// The daemon may have moved, as after an update by hand: the next screen probes again.
pub fn forget_probe(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine) {
        known.daemon_probe = DaemonProbe::Unknown;
    }
}

/// Starts the update `machine` calls for.
fn begin(state: &mut AppState, machine: MachineId, busy: BusyAgents) -> Option<UpdateRun> {
    let known = state.machines.get_mut(machine)?;
    let plan = plan_to_run(known)?;
    let replaces = known
        .version_gap
        .as_ref()
        .and_then(|gap| gap.daemon.clone())
        .or_else(|| ProgramBuild::daemon_of(known));
    known.job_holders.clear();
    known.update = Some(DaemonUpdate::Running(UpdateProgress::new(
        plan.clone(),
        replaces,
    )));
    hide_progress(state, machine);
    Some(UpdateRun { plan, busy })
}

/// The confirmation gives way to the machine's update screens, which show the wait.
fn show_progress(state: &mut AppState, machine: MachineId) {
    if state.ui.dialog == Some(Dialog::ConfirmUpdate(machine)) {
        state.ui.dialog = Some(Dialog::DaemonUpdate(machine));
    }
}

/// An update that starts runs beside the window: its card or the confirmation closes, and the
/// sidebar's line shows the steps until "Show" opens the card again.
fn hide_progress(state: &mut AppState, machine: MachineId) {
    state.ui.dialog.take_if(|dialog| {
        matches!(dialog, Dialog::DaemonUpdate(shown) | Dialog::ConfirmUpdate(shown) if *shown == machine)
    });
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

/// The send of the daemon reached `sent`.
pub fn sending(state: &mut AppState, machine: MachineId, sent: SendProgress) {
    if let Some(progress) = running_mut(state, machine) {
        progress.sent = Some(sent);
    }
}

/// The app has no daemon to send after all, since its download failed: the script builds on the
/// server, and the screen lists the build's steps.
pub fn builds_instead(state: &mut AppState, machine: MachineId) {
    let Some(progress) = running_mut(state, machine) else {
        return;
    };
    if let UpdatePlan::Server(plan) = &mut progress.plan {
        plan.daemon = None;
        progress.step = progress.plan.first_step();
    }
}

/// The restart of this computer's service reached `step`.
pub fn step_started(state: &mut AppState, machine: MachineId, step: UpdateStep) {
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    let connections = known.connections;
    let Some(DaemonUpdate::Running(progress)) = &mut known.update else {
        return;
    };
    if step == UpdateStep::Stop {
        progress.connections_at_stop.get_or_insert(connections);
    }
    progress.step = progress.step.max(step);
}

/// The script ended with `code`, `None` when ssh lost it. Returns whether the app should try the
/// machine again at once.
pub fn finished(state: &mut AppState, machine: MachineId, code: Option<i32>) -> bool {
    ended(state, machine, UpdateFailure::from_exit_code(code))
}

/// The send of the daemon failed, so the script never ran: nothing changed on the server.
pub fn send_failed(state: &mut AppState, machine: MachineId) {
    ended(state, machine, Some(UpdateFailure::Failed));
}

/// The update ended, with `failure` unless it succeeded. Returns whether the app should try the
/// machine again at once.
fn ended(state: &mut AppState, machine: MachineId, failure: Option<UpdateFailure>) -> bool {
    let Some(known) = state.machines.get_mut(machine) else {
        return false;
    };
    let Some(DaemonUpdate::Running(mut progress)) = known
        .update
        .take_if(|update| matches!(update, DaemonUpdate::Running(_)))
    else {
        return false;
    };
    match failure {
        // The script waits for the new daemon to answer: a connection the app has now reaches it.
        None if known.is_connected() => {
            close_empty_dialog(state, machine);
            false
        }
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

/// The restart of this computer's service ended. Returns whether the app should try the machine
/// again at once.
///
/// A connection the app made before the stop is the stopped daemon's, which it has not seen end
/// yet: the next one reaches the daemon that runs now. A connection made since the stop started
/// reaches a daemon that started since, such as the old one another program restarted at once:
/// it is judged now, since no later connection comes.
pub fn restart_ended(
    state: &mut AppState,
    machine: MachineId,
    restarted: Result<(), RestartFailure>,
) -> bool {
    let Some(known) = state.machines.get_mut(machine) else {
        return false;
    };
    let Some(DaemonUpdate::Running(mut progress)) = known
        .update
        .take_if(|update| matches!(update, DaemonUpdate::Running(_)))
    else {
        return false;
    };
    match restarted {
        Ok(()) if known.is_connected() && progress.connected_since_the_stop(known.connections) => {
            progress.step = UpdateStep::Reconnect;
            // A daemon of this app's build would have ended the update as it connected.
            known.update = known
                .version_gap
                .as_ref()
                .and_then(|gap| after_restart(progress, gap));
            close_empty_dialog(state, machine);
            false
        }
        Ok(()) => {
            progress.step = UpdateStep::Reconnect;
            known.update = Some(DaemonUpdate::Reconnecting(progress));
            true
        }
        Err(failure) => {
            if let RestartFailure::HoldsJobs(holders) = &failure {
                known.job_holders.clone_from(holders);
            }
            known.update = Some(DaemonUpdate::Failed(restart_failure(progress, failure)));
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
        UpdateFailure::AlreadyRunning | UpdateFailure::HoldsJobs => String::new(),
        UpdateFailure::Failed
        | UpdateFailure::Cancelled
        | UpdateFailure::InstallFailed
        | UpdateFailure::Unknown
        | UpdateFailure::NotStopped { .. }
        | UpdateFailure::NotStarted
        | UpdateFailure::OtherBuildAnswers => {
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

/// The error, and for a service that did not start, the end of its log. Agents that may hold
/// scheduled jobs show in a list of their own.
fn restart_failure(progress: UpdateProgress, failure: RestartFailure) -> FailedUpdate {
    let (failure, message) = match failure {
        RestartFailure::Busy => (UpdateFailure::Refused, String::new()),
        RestartFailure::HoldsJobs(_) => (UpdateFailure::HoldsJobs, String::new()),
        RestartFailure::NotStopped(StopFailure { reason, by_hand }) => {
            (UpdateFailure::NotStopped { by_hand }, reason)
        }
        RestartFailure::NotStarted { error, log: None } => (UpdateFailure::NotStarted, error),
        RestartFailure::NotStarted {
            error,
            log: Some(log),
        } => (UpdateFailure::NotStarted, format!("{error}\n\n{log}")),
    };
    FailedUpdate {
        plan: progress.plan,
        failure,
        message,
    }
}

/// "Cancel the update". Returns what to ask the server, once.
pub fn ask_cancel(state: &mut AppState, machine: MachineId) -> Option<ServerPlan> {
    let progress = running_mut(state, machine)?;
    if progress.cancel_asked || !progress.offers_cancel() {
        return None;
    }
    let UpdatePlan::Server(plan) = &progress.plan else {
        return None;
    };
    let plan = plan.clone();
    progress.cancel_asked = true;
    Some(plan)
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
    let failed_plan = match &mut known.update {
        Some(DaemonUpdate::Failed(failed)) => Some(&mut failed.plan),
        Some(DaemonUpdate::Waiting | DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_))
        | None => None,
    };
    known.daemon_probe = DaemonProbe::Unknown;
    let plans = known.update_plan.as_mut().into_iter().chain(failed_plan);
    for plan in plans {
        if let UpdatePlan::Server(plan) = plan {
            config.host.clone_into(&mut plan.host);
            config.remote_command.clone_into(&mut plan.remote_command);
            plan.source_dir.clone_from(&config.source_dir);
        }
    }
}

/// "Back" on a failure: the card that offers the update again, if the daemon still needs one.
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
        Some(DaemonUpdate::Waiting | DaemonUpdate::Reconnecting(_) | DaemonUpdate::Failed(_))
        | None => None,
    }
}

/// Closes `machine`'s update dialogs once they have nothing left to show: its daemon runs this
/// app's build, and no update waits, runs or failed.
fn close_empty_dialog(state: &mut AppState, machine: MachineId) {
    let nothing_to_show = state
        .machines
        .get(machine)
        .is_none_or(|known| known.update.is_none() && known.version_gap.is_none());
    if nothing_to_show {
        state.ui.dialog.take_if(|dialog| {
            matches!(dialog, Dialog::DaemonUpdate(shown) | Dialog::ConfirmUpdate(shown) if *shown == machine)
        });
    }
}

/// Whether this computer's update screens take the place of the window's panes, while the app
/// reaches no server: an update runs or failed, and the app cannot use the daemon meanwhile. A
/// wait for idle agents, or a daemon that still answers, leaves the panes in use.
pub fn takes_the_window(known: &MachineState) -> bool {
    known
        .update
        .as_ref()
        .is_some_and(|update| !update.is_waiting())
        && !known.is_connected()
}

/// Opens a machine's update screens, this computer's as any server's.
pub fn open(state: &mut AppState, machine: MachineId) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::DaemonUpdate(machine));
}

/// The link of a machine's line that opens its update's screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateLink {
    /// The app can update the daemon.
    Update,
    /// The app cannot update the daemon: the screens say why.
    Details,
    /// An update waits, runs or failed.
    Show,
}

impl UpdateLink {
    pub fn label(self) -> &'static str {
        match self {
            Self::Update => "Update",
            Self::Details => "Details",
            Self::Show => "Show",
        }
    }
}

/// What the sidebar says of a machine with an update to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateLine {
    pub text: &'static str,
    /// The step of an update under way, which the line shows in place of `text`.
    pub progress: Option<LineProgress>,
    /// The link that opens the update's screens.
    pub link: UpdateLink,
    /// The daemon answers no request: the panel's edge turns red.
    pub down: bool,
    /// Work under way, such as the update itself: a spinner, not a warning.
    pub under_way: bool,
    /// The line offers "Retry now" too. A daemon the app can update needs the update, not
    /// another try.
    pub retry: bool,
}

/// The sidebar's line for `known`, when it has an update to show. A machine that stays in use
/// during its wait or with another build shows its lost connection instead.
pub fn update_line(known: &MachineState) -> Option<UpdateLine> {
    let line = |text, link, down| UpdateLine {
        text,
        progress: None,
        link,
        down,
        under_way: false,
        retry: down,
    };
    let connected = known.is_connected();
    // A daemon that Nix, a package or the user owns gets the screens that say how to update it.
    let update = if owned_by_another(known) {
        UpdateLink::Details
    } else {
        UpdateLink::Update
    };
    if known.checking_jobs {
        return Some(UpdateLine {
            under_way: true,
            ..line("Checking scheduled jobs…", UpdateLink::Show, false)
        });
    }
    match (&known.update, &known.version_gap) {
        (Some(DaemonUpdate::Running(progress) | DaemonUpdate::Reconnecting(progress)), _) => {
            Some(UpdateLine {
                progress: Some(LineProgress::from(progress)),
                under_way: true,
                ..line("Updating…", UpdateLink::Show, false)
            })
        }
        (Some(DaemonUpdate::Waiting), _) if !connected => None,
        (Some(DaemonUpdate::Waiting), _) => Some(line(
            "Updates once its agents are idle.",
            UpdateLink::Show,
            false,
        )),
        (Some(DaemonUpdate::Failed(failed)), _) => Some(line(
            failed.failure.sidebar_text(),
            UpdateLink::Show,
            !connected,
        )),
        (None, Some(gap)) => match (gap.kind, &known.update_plan) {
            (GapKind::OlderProtocol, Some(_)) => Some(UpdateLine {
                retry: false,
                ..line("Runs an older version.", update, true)
            }),
            (GapKind::OlderProtocol, None) => {
                Some(line("Runs an older version.", UpdateLink::Details, true))
            }
            (GapKind::NewerProtocol, _) => {
                Some(line("Runs a newer version.", UpdateLink::Details, true))
            }
            (GapKind::OtherBuild | GapKind::NewerRelease, _) if !connected => None,
            (GapKind::OtherBuild, Some(_)) if gap.is_other_commit() => {
                Some(line("Runs another build.", update, false))
            }
            (GapKind::OtherBuild, Some(_)) => Some(line("Update available.", update, false)),
            (GapKind::OtherBuild, None) => None,
            (GapKind::NewerRelease, _) => {
                Some(line("Runs a newer version.", UpdateLink::Details, false))
            }
        },
        (None, None) => None,
    }
}

/// An update under way, as the sidebar's line shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineProgress {
    /// The step under way: "Sending 0.2.0", "Restarting".
    pub text: String,
    /// The share of the update done, in percent: the steps done, and the bytes of a send.
    pub percent: u8,
}

impl From<&UpdateProgress> for LineProgress {
    fn from(progress: &UpdateProgress) -> Self {
        let release = &ProgramBuild::this_program().version;
        let text = match progress.step {
            UpdateStep::GetVersion | UpdateStep::Build => format!("Building {release}"),
            UpdateStep::Send => format!("Sending {release}"),
            UpdateStep::Check => format!("Checking {release}"),
            UpdateStep::Restart => "Restarting".to_owned(),
            UpdateStep::Stop => "Stopping".to_owned(),
            UpdateStep::Start => "Starting".to_owned(),
            UpdateStep::Reconnect => "Reconnecting".to_owned(),
        };
        let steps = progress.plan.steps();
        let done = steps
            .iter()
            .position(|step| *step == progress.step)
            .unwrap_or_default();
        let within = match (progress.step, progress.sent) {
            (UpdateStep::Send, Some(sent)) => u64::from(sent.percent()),
            _ => 0,
        };
        let count = u64::try_from(steps.len()).unwrap_or(1).max(1);
        let done = u64::try_from(done).unwrap_or_default();
        let percent = (done.saturating_mul(100).saturating_add(within)) / count;
        Self {
            text,
            percent: u8::try_from(percent).unwrap_or(100),
        }
    }
}

/// What the confirmation before an update says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub text: String,
    /// The line above the list, when the list shows the agents as the app last saw them.
    pub list_note: Option<&'static str>,
    /// The agents it lists, in the sidebar's order.
    pub agents: Vec<AgentId>,
    /// It offers "Update when they are idle".
    pub offers_wait: bool,
}

/// The confirmation before `machine`'s update, for the machine the screens call `name`.
pub fn confirmation(state: &AppState, machine: MachineId, name: &str) -> Option<Confirmation> {
    let known = state.machines.get(machine)?;
    let app = &state.app_build.version;
    let running = update_offer::running_agents(state, machine);
    let agents = update_offer::busy_agents(state, machine);
    let unseen = agents_unseen(known);
    let effect = if unseen {
        let why = match known.version_gap.as_ref().map(|gap| gap.kind) {
            Some(GapKind::OlderProtocol) => "while its daemon refuses the app",
            Some(GapKind::OtherBuild | GapKind::NewerRelease | GapKind::NewerProtocol) | None => {
                "while the app is not connected to it"
            }
        };
        format!(
            "The app cannot list {name}'s agents {why}. The restart interrupts any agent that works, which resumes its conversation after it, and loses every scheduled job."
        )
    } else {
        match (running.is_empty(), agents.len() > running.len()) {
            (false, true) => "It interrupts the busy agents, which resume their conversations after it, and loses the scheduled jobs the idle ones may hold.".to_owned(),
            (true, true) => "These agents may hold scheduled jobs, which it loses.".to_owned(),
            (_, false) => "It interrupts these agents, which resume their conversations after it.".to_owned(),
        }
    };
    // The probe says which daemon the update sends, if any.
    let plan = runnable(known).or_else(|| known.update_plan.clone());
    let text = match &plan {
        Some(UpdatePlan::Server(plan)) if plan.daemon.is_some() => format!(
            "The update sends this app's version, {app}, to {name} and restarts its daemon. {effect}"
        ),
        Some(UpdatePlan::Server(_)) => format!(
            "The update builds this app's version, {app}, on {name} and restarts its daemon. {effect}"
        ),
        Some(UpdatePlan::Service(_)) | None => format!(
            "The update restarts the background service with this app's version, {app}. {effect}"
        ),
    };
    Some(Confirmation {
        text,
        list_note: (unseen && !agents.is_empty())
            .then_some("These agents were busy when the app last saw them:"),
        agents,
        // The wait ends once no agent runs something: a scheduled job may never end.
        offers_wait: !unseen && !running.is_empty(),
    })
}

/// The agents a failure's screen lists: for `HoldsJobs`, those that may hold a scheduled job; for
/// a refusal on this computer, those that run something. A server's script lists its own.
pub fn failure_agents(state: &AppState, machine: MachineId, failed: &FailedUpdate) -> Vec<AgentId> {
    match (failed.failure, &failed.plan) {
        (UpdateFailure::HoldsJobs, _) => update_offer::job_holding_agents(state, machine),
        (UpdateFailure::Refused, UpdatePlan::Service(_)) => {
            update_offer::running_agents(state, machine)
        }
        (
            UpdateFailure::Refused
            | UpdateFailure::Failed
            | UpdateFailure::Cancelled
            | UpdateFailure::InstallFailed
            | UpdateFailure::RolledBack
            | UpdateFailure::AlreadyRunning
            | UpdateFailure::Unknown
            | UpdateFailure::NotStopped { .. }
            | UpdateFailure::NotStarted
            | UpdateFailure::OtherBuildAnswers,
            _,
        ) => Vec::new(),
    }
}

/// What a failure's screen says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureText {
    pub title: String,
    pub lead: String,
    /// What the user does before a retry, when the app cannot do it.
    pub hint: Option<String>,
}

/// The texts of `failed`'s screen, for the machine the screens call `name`.
pub fn failure_text(failed: &FailedUpdate, name: &str) -> FailureText {
    let not_updated = format!("{name} was not updated");
    let text = |title: String, lead: String| FailureText {
        title,
        lead,
        hint: None,
    };
    let state_dir = match &failed.plan {
        UpdatePlan::Service(plan) => plan.state_dir.display().to_string(),
        UpdatePlan::Server(_) => String::new(),
    };
    match failed.failure {
        UpdateFailure::Refused => text(
            not_updated,
            "These agents are busy, and a restart would interrupt them. Let them finish, then update again.".to_owned(),
        ),
        UpdateFailure::Failed => text(
            not_updated,
            format!("The update failed, so {name} still runs its old version."),
        ),
        UpdateFailure::Cancelled => text(
            not_updated,
            format!("The update was cancelled, so {name} still runs its old version."),
        ),
        UpdateFailure::InstallFailed => text(
            not_updated,
            format!("The update failed during the install or the restart, so the daemon on {name} may not have come back. Check the server."),
        ),
        UpdateFailure::RolledBack => text(
            not_updated,
            format!("The new version did not start, so {name} runs its old version again."),
        ),
        UpdateFailure::AlreadyRunning => text(
            format!("An update is already running on {name}"),
            "Try again in a few minutes.".to_owned(),
        ),
        UpdateFailure::Unknown => text(
            "The app lost track of the update".to_owned(),
            format!("The app cannot tell how the update of {name} went. If it started, it goes on on the server: try again in a few minutes."),
        ),
        UpdateFailure::NotStopped { by_hand } => {
            let (lead, hint) = match by_hand {
                Some(HandStop::Process(pid)) => (
                    format!("The app could not stop the daemon, so {name} still runs its old version."),
                    Some(format!("Stop process {} yourself, then retry.", pid.get())),
                ),
                Some(HandStop::Unrecorded) => (
                    "The daemon that runs now comes from an older version, which does not record its process, so the app cannot tell which process to stop.".to_owned(),
                    Some(format!("Stop the {PROGRAM} {SERVE_SUBCOMMAND} of {state_dir} yourself, once, then retry. From then on, the app stops the daemon itself.")),
                ),
                Some(HandStop::Unverified) => (
                    "The app could not tell which process runs the daemon, so it stopped none.".to_owned(),
                    Some(format!("Stop the {PROGRAM} {SERVE_SUBCOMMAND} of {state_dir} yourself, then retry.")),
                ),
                None => (
                    format!("The app could not stop the daemon, so {name} still runs its old version."),
                    None,
                ),
            };
            FailureText {
                title: not_updated,
                lead,
                hint,
            }
        }
        UpdateFailure::NotStarted => text(
            "The new daemon did not start".to_owned(),
            format!("The app stopped the old daemon on {name}, but the new one did not start, so no daemon runs there. The error and the end of the service's log are below."),
        ),
        UpdateFailure::OtherBuildAnswers => FailureText {
            title: not_updated,
            lead: "The app started this app's version, but the daemon that answers still runs another build: another program restarted the old one, such as a systemd unit that restarts it whenever it stops.".to_owned(),
            hint: Some("Stop that program, or point it at this app's version, then retry.".to_owned()),
        },
        UpdateFailure::HoldsJobs => text(
            not_updated,
            "These agents may hold scheduled jobs, which a restart would lose. Delete the jobs or stop the agents, then retry.".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use cockpit_client::{ConnectError, SessionError};
    use cockpit_protocol::{AgentId, AgentStatus, BuildCommit, PROTOCOL_VERSION, ReleaseVersion};

    use super::*;
    use crate::daemon_label::{DaemonLabel, DaemonPlace};
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::daemon_probe::LinuxCpu;
    use crate::state::update_plan::{CarriedDaemons, DaemonSource, ServicePlan};
    use crate::state::{AgentEntry, ConnectionState, Drafts, MachineState, machines};

    fn build_box() -> MachineId {
        MachineId::new(1)
    }

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

    fn plan() -> UpdatePlan {
        UpdatePlan::Server(server_plan())
    }

    fn service_plan() -> UpdatePlan {
        UpdatePlan::Service(ServicePlan {
            state_dir: PathBuf::from("/home/someone/.local/state/yetack"),
            log_folder: PathBuf::from("/home/someone/.local/state/yetack-desktop"),
        })
    }

    fn run(plan: UpdatePlan) -> UpdateRun {
        UpdateRun {
            plan,
            busy: BusyAgents::Refuse,
        }
    }

    fn refused(reason: &str) -> SessionError {
        SessionError::Connect(ConnectError::Refused {
            reason: reason.to_owned(),
            daemon_version: None,
            daemon_commit: None,
        })
    }

    fn gap_over_ssh(reason: &str) -> Option<VersionGap> {
        update_offer::protocol_gap(DaemonPlace::Ssh, &refused(reason))
    }

    fn newer_over_ssh() -> Option<VersionGap> {
        gap_over_ssh(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ))
    }

    fn older() -> VersionGap {
        VersionGap {
            kind: GapKind::OlderProtocol,
            details: "the daemon speaks protocol 1, the client 4".to_owned(),
            daemon: None,
            protocol: PROTOCOL_VERSION - 1,
        }
    }

    /// The probe of a server whose daemon the app owns, at the plan's path.
    fn owned_probe() -> DaemonProbe {
        DaemonProbe::owned_at("/opt/yetack")
    }

    fn with_server() -> AppState {
        let mut state = state_with(Vec::new());
        let mut known = MachineState::new(DaemonLabel::ssh("build-box".to_owned()));
        known.update_plan = Some(plan());
        known.daemon_probe = owned_probe();
        state.machines.others.insert(build_box(), known);
        state
    }

    fn known_mut(state: &mut AppState) -> &mut MachineState {
        state.machines.others.get_mut(&build_box()).unwrap()
    }

    fn with_older_server() -> AppState {
        let mut state = with_server();
        known_mut(&mut state).version_gap = Some(older());
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

    /// The build the app has in these tests, from `state_with`.
    fn app_build(state: &AppState) -> ProgramBuild {
        state.app_build.clone()
    }

    fn other_build() -> ProgramBuild {
        ProgramBuild {
            version: ReleaseVersion::new("0.4.2".to_owned()),
            commit: Some(BuildCommit::new(
                "9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c".to_owned(),
            )),
        }
    }

    fn newer_build() -> ProgramBuild {
        ProgramBuild {
            version: ReleaseVersion::new("9.0.0".to_owned()),
            commit: None,
        }
    }

    /// The app connects to `machine`'s daemon, which runs `daemon`.
    fn connect(state: &mut AppState, machine: MachineId, daemon: &ProgramBuild) {
        let known = state.machines.get_mut(machine).unwrap();
        known.connection = ConnectionState::Connected;
        known.daemon_version = Some(daemon.version.clone());
        known.daemon_commit.clone_from(&daemon.commit);
        connected(state, machine);
    }

    /// The machine's connection drops.
    fn drop_connection(state: &mut AppState, machine: MachineId) {
        state.machines.get_mut(machine).unwrap().connection = ConnectionState::Disconnected {
            error: "connection reset".to_owned(),
            retry_at: Instant::now(),
        };
    }

    /// A server the app is connected to, whose daemon runs another build.
    fn with_server_of_another_build(agents: &[AgentStatus]) -> AppState {
        let mut state = with_server();
        for (number, status) in (1..).zip(agents) {
            let mut agent = summary(number, *status);
            agent.id = build_box().app_agent(AgentId::new(number));
            state.agents.insert(agent.id, AgentEntry::new(agent));
        }
        connect(&mut state, build_box(), &other_build());
        state
    }

    /// This computer, connected to its background service, which runs another build.
    fn with_local_service(agents: &[AgentStatus]) -> AppState {
        let summaries = (1..).zip(agents).map(|(id, status)| summary(id, *status));
        let mut state = state_with(summaries.collect());
        state.machines.this.daemon = DaemonLabel::service();
        state.machines.this.update_plan = Some(service_plan());
        connect(&mut state, MachineId::THIS, &other_build());
        state
    }

    fn this_update(state: &AppState) -> Option<&DaemonUpdate> {
        state.machines.this.update.as_ref()
    }

    fn set_status(state: &mut AppState, agent: AgentId, status: AgentStatus) {
        state.agents.get_mut(&agent).unwrap().summary.status = status;
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
            known_mut(&mut state).connection = ConnectionState::Connecting { attempt: 1 };
            attempt_failed(&mut state, build_box(), Some(older()));
        }

        assert_eq!(update(&state).cloned(), shown);
        assert_eq!(known(&state).version_gap, Some(older()));
    }

    #[test]
    fn an_update_survives_the_daemon_being_down_during_the_restart() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: restarting the daemon");

        attempt_failed(&mut state, build_box(), None);

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

        attempt_failed(&mut state, build_box(), None);

        assert_eq!(update(&state), None);
        assert_eq!(state.ui.dialog, None, "nothing is left to show");
    }

    #[test]
    fn a_refusal_after_the_script_ended_offers_the_update_again() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        assert!(finished(&mut state, build_box(), Some(0)));

        attempt_failed(&mut state, build_box(), Some(older()));

        assert_eq!(update(&state), None);
        assert_eq!(known(&state).version_gap, Some(older()));
    }

    #[test]
    fn a_failed_update_gives_way_to_a_daemon_that_is_now_newer() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        finished(&mut state, build_box(), Some(1));

        attempt_failed(&mut state, build_box(), newer_over_ssh());

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
    fn a_connection_to_this_computer_answers_its_question() {
        let mut state = with_server();
        state.ui.dialog = Some(Dialog::NoDaemon);

        connected(&mut state, build_box());
        assert_eq!(state.ui.dialog, Some(Dialog::NoDaemon));

        connected(&mut state, MachineId::THIS);
        assert_eq!(state.ui.dialog, None);
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

        assert_eq!(start(&mut state, build_box()), Some(run(plan())));
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
        attempt_failed(&mut state, build_box(), None);

        back(&mut state, build_box());

        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn with_no_commit_known_nothing_starts() {
        let mut state = with_server();
        known_mut(&mut state).update_plan = None;
        attempt_failed(
            &mut state,
            build_box(),
            gap_over_ssh("the daemon speaks protocol 1, the client 4"),
        );

        assert_eq!(start(&mut state, build_box()), None);
        assert_eq!(update(&state), None);
    }

    #[test]
    fn with_no_commit_known_the_sidebar_offers_no_update() {
        let mut state = with_server();
        known_mut(&mut state).update_plan = None;
        attempt_failed(
            &mut state,
            build_box(),
            gap_over_ssh("the daemon speaks protocol 1, the client 4"),
        );

        let shown = update_line(known(&state)).unwrap();

        assert_eq!(
            (shown.text, shown.link),
            ("Runs an older version.", UpdateLink::Details)
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
        second.update_plan = Some(plan());
        second.daemon_probe = owned_probe();
        state.machines.others.insert(gpu_box, second);
        start(&mut state, build_box()).unwrap();
        start(&mut state, gpu_box).unwrap();
        state.ui.dialog = Some(Dialog::DaemonUpdate(gpu_box));
        let untouched = state.machines.get(gpu_box).unwrap().clone();

        stdout(&mut state, "update-daemon: building 3f9c2e1");
        ask_cancel(&mut state, build_box()).unwrap();
        cancel_failed(&mut state, build_box(), "ssh: connect failed".to_owned());
        attempt_failed(&mut state, build_box(), None);
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
    fn a_machine_removed_while_its_busy_agents_are_asked_about_closes_the_question() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        assert_eq!(state.ui.dialog, Some(Dialog::ConfirmUpdate(build_box())));

        machines::forget(&mut state, &mut Drafts::default(), build_box());

        assert_eq!(state.ui.dialog, None);
        assert_eq!(wait_for_idle(&mut state, build_box()), None);
        assert_eq!(start_now(&mut state, build_box()), None);
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
        start(&mut state, build_box()).unwrap();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));

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
        assert_eq!((shown.text, shown.link), ("Updating…", UpdateLink::Show));
    }

    #[test]
    fn a_run_keeps_its_screen_when_the_daemon_is_newer() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();

        attempt_failed(&mut state, build_box(), newer_over_ssh());

        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));
    }

    #[test]
    fn a_newer_daemon_is_offered_no_plan() {
        let mut state = with_server();

        attempt_failed(&mut state, build_box(), newer_over_ssh());

        assert_eq!(update_offer::offer(&state, build_box()).unwrap().plan, None);
        assert_eq!(start(&mut state, build_box()), None);
    }

    #[test]
    fn cancel_is_asked_once_and_only_until_the_restart_starts() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: building 3f9c2e1");

        assert_eq!(ask_cancel(&mut state, build_box()), Some(server_plan()));
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

        assert_eq!(
            text(&state),
            Some(("Runs an older version.", UpdateLink::Update))
        );
        start(&mut state, build_box()).unwrap();
        assert_eq!(text(&state), Some(("Updating…", UpdateLink::Show)));
        finished(&mut state, build_box(), Some(1));
        assert_eq!(text(&state), Some(("Was not updated.", UpdateLink::Show)));

        known_mut(&mut state).update = None;
        known_mut(&mut state).version_gap = Some(VersionGap {
            kind: GapKind::NewerProtocol,
            details: String::new(),
            daemon: None,
            protocol: PROTOCOL_VERSION + 1,
        });
        assert_eq!(
            text(&state),
            Some(("Runs a newer version.", UpdateLink::Details))
        );

        known_mut(&mut state).version_gap = None;
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
            assert_eq!(
                (shown.text, shown.link),
                (expected, UpdateLink::Show),
                "{code:?}"
            );
            assert!(shown.down, "{code:?}");
        }
    }

    #[test]
    fn a_daemon_of_this_release_from_another_commit_runs_another_build_and_still_offers_it() {
        let mut state = with_server();
        let same_release = ProgramBuild {
            version: ProgramBuild::this_program().version,
            ..other_build()
        };
        connect(&mut state, build_box(), &same_release);

        let shown = update_line(known(&state)).unwrap();

        assert_eq!(
            (shown.text, shown.link, shown.down),
            ("Runs another build.", UpdateLink::Update, false)
        );
    }

    #[test]
    fn a_connected_machine_of_another_build_offers_an_update_and_stays_in_use() {
        let mut state = with_server_of_another_build(&[]);

        let shown = update_line(known(&state)).unwrap();

        assert_eq!(
            (shown.text, shown.link, shown.down),
            ("Update available.", UpdateLink::Update, false)
        );
        assert_eq!(start(&mut state, build_box()), Some(run(plan())));
        assert_eq!(progress(&state).replaces, Some(other_build()));
    }

    #[test]
    fn a_connected_machine_of_a_newer_release_suggests_updating_the_app() {
        let mut state = with_server();
        connect(&mut state, build_box(), &newer_build());

        let shown = update_line(known(&state)).unwrap();

        assert_eq!(
            (shown.text, shown.link, shown.down),
            ("Runs a newer version.", UpdateLink::Details, false)
        );
        assert_eq!(start(&mut state, build_box()), None);
    }

    #[test]
    fn a_connected_machine_of_this_apps_build_shows_nothing() {
        let mut state = with_server();
        let app = app_build(&state);

        connect(&mut state, build_box(), &app);

        assert_eq!(known(&state).version_gap, None);
        assert_eq!(update_line(known(&state)), None);
        assert_eq!(start(&mut state, build_box()), None);
    }

    #[test]
    fn another_build_the_app_knows_no_way_to_update_shows_nothing() {
        let mut state = with_server_of_another_build(&[]);
        known_mut(&mut state).update_plan = None;

        assert_eq!(update_line(known(&state)), None);
        assert_eq!(start(&mut state, build_box()), None);
    }

    #[test]
    fn busy_agents_of_a_connected_machine_are_asked_about_first() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working, AgentStatus::Idle]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));

        assert_eq!(start(&mut state, build_box()), None);

        assert_eq!(state.ui.dialog, Some(Dialog::ConfirmUpdate(build_box())));
        assert_eq!(update(&state), None, "nothing starts before the answer");
    }

    #[test]
    fn an_agent_that_waits_for_an_answer_is_busy_too() {
        let mut state = with_server_of_another_build(&[AgentStatus::NeedsYou]);

        assert_eq!(start(&mut state, build_box()), None);

        assert_eq!(state.ui.dialog, Some(Dialog::ConfirmUpdate(build_box())));
    }

    #[test]
    fn idle_and_exited_agents_let_the_update_start_at_once() {
        let mut state = with_server_of_another_build(&[AgentStatus::Idle, AgentStatus::Exited]);

        assert_eq!(start(&mut state, build_box()), Some(run(plan())));
    }

    #[test]
    fn a_machine_the_app_does_not_reach_updates_at_once_and_its_script_checks() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        drop_connection(&mut state, build_box());
        known_mut(&mut state).version_gap = Some(older());

        assert_eq!(start(&mut state, build_box()), Some(run(plan())));
    }

    #[test]
    fn update_now_ends_the_busy_agents_turns_and_runs_beside_the_window() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());

        let now = start_now(&mut state, build_box());

        assert_eq!(
            now,
            Some(UpdateRun {
                plan: plan(),
                busy: BusyAgents::Interrupt
            })
        );
        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));
        assert_eq!(state.ui.dialog, None, "the confirmation closed");
    }

    #[test]
    fn an_update_that_starts_closes_its_card_and_leaves_another_machines_open() {
        let mut state = with_older_server();
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));

        start(&mut state, build_box()).unwrap();

        assert_eq!(state.ui.dialog, None);
        let line = update_line(state.machines.get(build_box()).unwrap()).unwrap();
        assert_eq!(line.link, UpdateLink::Show, "the sidebar opens it again");

        state.ui.dialog = Some(Dialog::DaemonUpdate(MachineId::THIS));
        finished(&mut state, build_box(), Some(1));
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(MachineId::THIS)));
    }

    #[test]
    fn a_probe_that_a_retry_replaced_is_dropped() {
        let mut state = with_older_server();
        known_mut(&mut state).daemon_probe = DaemonProbe::Unknown;
        let before = probe_to_run(&mut state, build_box()).unwrap();
        forget_probe(&mut state, build_box());
        let after = probe_to_run(&mut state, build_box()).unwrap();
        let nothing = || {
            Ok(ServerInstall {
                system: "Linux x86_64".to_owned(),
                daemon: None,
            })
        };

        probed(&mut state, build_box(), &before, nothing());
        assert_eq!(
            known(&state).daemon_probe,
            DaemonProbe::Probing(after.number)
        );

        probed(&mut state, build_box(), &after, nothing());
        assert!(matches!(known(&state).daemon_probe, DaemonProbe::Read(_)));
    }

    #[test]
    fn a_wait_whose_probe_was_forgotten_probes_once_its_agents_are_idle() {
        let mut state = with_server_of_another_build(&[AgentStatus::Idle]);
        known_mut(&mut state).update = Some(DaemonUpdate::Waiting);
        known_mut(&mut state).daemon_probe = DaemonProbe::Unknown;

        let probe = probe_for_wait(&mut state, build_box());

        assert!(probe.is_some());
        assert!(
            probe_for_wait(&mut state, build_box()).is_none(),
            "one runs already"
        );
    }

    #[test]
    fn a_wait_ends_when_the_daemon_is_not_the_apps_to_replace() {
        let mut state = with_server_of_another_build(&[AgentStatus::Idle]);
        known_mut(&mut state).update = Some(DaemonUpdate::Waiting);
        known_mut(&mut state).daemon_probe = DaemonProbe::Failed("ssh: timed out".to_owned());

        assert_eq!(start_when_idle(&mut state, build_box()), None);
        assert_eq!(update(&state), None);
    }

    #[test]
    fn only_a_server_waits_for_a_probe() {
        let service = with_local_service(&[]);
        let mut server = with_older_server();
        known_mut(&mut server).daemon_probe = DaemonProbe::Unknown;

        assert!(
            !probe_due(&service, MachineId::THIS),
            "the service has nothing to probe"
        );
        assert!(probe_due(&server, build_box()));
    }

    #[test]
    fn a_wait_holds_until_the_probe_says_the_update_can_run() {
        let mut state = with_server_of_another_build(&[AgentStatus::Idle]);
        known_mut(&mut state).update = Some(DaemonUpdate::Waiting);
        known_mut(&mut state).daemon_probe = DaemonProbe::Unknown;

        assert_eq!(start_when_idle(&mut state, build_box()), None);
        assert_eq!(update(&state), Some(&DaemonUpdate::Waiting));

        known_mut(&mut state).daemon_probe = owned_probe();
        assert!(start_when_idle(&mut state, build_box()).is_some());
    }

    /// The plan came before GitHub said the app is its release's build: the probe brings it.
    #[test]
    fn the_daemons_known_after_a_probe_reach_the_servers_plan() {
        let mut state = with_older_server();
        known_mut(&mut state).update_plan = Some(UpdatePlan::Server(server_plan()));
        let downloads = CarriedDaemons {
            release: true,
            ..CarriedDaemons::default()
        };

        carry(&mut state, build_box(), downloads.clone());

        assert!(matches!(
            &known_mut(&mut state).update_plan,
            Some(UpdatePlan::Server(plan)) if plan.carried == downloads
        ));
    }

    #[test]
    fn the_confirmation_says_the_update_sends_the_daemon_carried_for_the_servers_cpu() {
        let mut state = with_older_server();
        known_mut(&mut state).update_plan = Some(UpdatePlan::Server(ServerPlan {
            carried: CarriedDaemons {
                x86_64: Some("/opt/app/yetack-x86_64-linux".into()),
                ..CarriedDaemons::default()
            },
            ..server_plan()
        }));

        let asked = confirmation(&state, build_box(), "build-box").unwrap();

        assert!(asked.text.starts_with("The update sends"), "{}", asked.text);
    }

    #[test]
    fn a_failed_send_ends_the_update_as_failed_with_what_the_app_said() {
        let mut state = with_older_server();
        let sends = UpdatePlan::Server(ServerPlan {
            carried: CarriedDaemons {
                x86_64: Some("/opt/app/yetack-x86_64-linux".into()),
                aarch64: None,
                release: false,
            },
            daemon: Some(DaemonSource::Carried("/opt/app/yetack-x86_64-linux".into())),
            ..server_plan()
        });
        known_mut(&mut state).update_plan = Some(sends.clone());
        start(&mut state, build_box()).unwrap();
        let said = "Could not send the daemon: Connection refused";
        output(
            &mut state,
            build_box(),
            vec![line(OutputStream::Stderr, said)],
        );

        send_failed(&mut state, build_box());

        assert_eq!(
            update(&state),
            Some(&DaemonUpdate::Failed(FailedUpdate {
                plan: sends,
                failure: UpdateFailure::Failed,
                message: said.to_owned(),
            }))
        );
    }

    #[test]
    fn a_failed_download_shows_the_build_and_lets_it_be_cancelled() {
        let mut state = with_older_server();
        let downloads = ServerPlan {
            carried: CarriedDaemons {
                release: true,
                ..CarriedDaemons::default()
            },
            ..server_plan()
        };
        known_mut(&mut state).update_plan = Some(UpdatePlan::Server(downloads.clone()));
        start(&mut state, build_box()).unwrap();
        assert_eq!(
            progress(&state).plan,
            UpdatePlan::Server(ServerPlan {
                daemon: Some(DaemonSource::Release(LinuxCpu::X86_64)),
                ..downloads.clone()
            })
        );

        builds_instead(&mut state, build_box());
        stdout(&mut state, "update-daemon: building 3f9c2e1");

        let progress = progress(&state);
        assert_eq!(progress.plan, UpdatePlan::Server(downloads));
        assert_eq!(progress.step, UpdateStep::Build);
        assert!(progress.offers_cancel());
    }

    #[test]
    fn the_sidebar_line_names_the_step_and_counts_the_bytes_of_a_send() {
        let mut state = with_older_server();
        let sends = UpdatePlan::Server(ServerPlan {
            carried: CarriedDaemons {
                x86_64: Some("/opt/app/yetack-x86_64-linux".into()),
                aarch64: None,
                release: false,
            },
            daemon: Some(DaemonSource::Carried("/opt/app/yetack-x86_64-linux".into())),
            ..server_plan()
        });
        state.machines.get_mut(build_box()).unwrap().update_plan = Some(sends);
        start(&mut state, build_box()).unwrap();
        let release = ProgramBuild::this_program().version;
        let progress = |state: &AppState| {
            update_line(state.machines.get(build_box()).unwrap())
                .and_then(|line| line.progress)
                .map(|progress| (progress.text, progress.percent))
        };

        sending(&mut state, build_box(), SendProgress { sent: 2, total: 4 });
        assert_eq!(
            progress(&state),
            Some((format!("Sending {release}"), 12)),
            "half of the first of four steps"
        );

        stdout(&mut state, "update-daemon: checking /home/someone/.cache/x");
        assert_eq!(progress(&state), Some((format!("Checking {release}"), 25)));

        stdout(&mut state, "update-daemon: restarting the daemon");
        assert_eq!(progress(&state), Some(("Restarting".to_owned(), 50)));
    }

    #[test]
    fn update_when_idle_waits_then_starts_once_the_last_busy_agent_is_idle() {
        let mut state =
            with_server_of_another_build(&[AgentStatus::Working, AgentStatus::NeedsYou]);
        start(&mut state, build_box());
        let (first, second) = (
            build_box().app_agent(AgentId::new(1)),
            build_box().app_agent(AgentId::new(2)),
        );

        assert_eq!(wait_for_idle(&mut state, build_box()), None);
        assert_eq!(update(&state), Some(&DaemonUpdate::Waiting));
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(build_box())));
        let shown = update_line(known(&state)).unwrap();
        assert_eq!(
            (shown.text, shown.link, shown.down),
            ("Updates once its agents are idle.", UpdateLink::Show, false)
        );

        set_status(&mut state, first, AgentStatus::Idle);
        assert_eq!(start_when_idle(&mut state, build_box()), None);
        set_status(&mut state, second, AgentStatus::Idle);
        assert_eq!(start_when_idle(&mut state, build_box()), Some(run(plan())));
        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));
        assert_eq!(start_when_idle(&mut state, build_box()), None, "once");
    }

    #[test]
    fn update_when_idle_with_no_busy_agent_left_starts_at_once() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        set_status(
            &mut state,
            build_box().app_agent(AgentId::new(1)),
            AgentStatus::Idle,
        );

        assert_eq!(wait_for_idle(&mut state, build_box()), Some(run(plan())));
    }

    #[test]
    fn a_wait_holds_while_the_app_does_not_see_the_agents() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        wait_for_idle(&mut state, build_box());
        drop_connection(&mut state, build_box());
        set_status(
            &mut state,
            build_box().app_agent(AgentId::new(1)),
            AgentStatus::Idle,
        );

        assert_eq!(start_when_idle(&mut state, build_box()), None);
        assert_eq!(update(&state), Some(&DaemonUpdate::Waiting));

        connect(&mut state, build_box(), &other_build());
        assert_eq!(update(&state), Some(&DaemonUpdate::Waiting));
        assert_eq!(start_when_idle(&mut state, build_box()), Some(run(plan())));
    }

    #[test]
    fn a_daemon_updated_another_way_ends_the_wait() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        wait_for_idle(&mut state, build_box());
        let app = app_build(&state);

        connect(&mut state, build_box(), &app);

        assert_eq!(update(&state), None);
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn a_wait_ends_for_a_daemon_that_is_now_newer() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        wait_for_idle(&mut state, build_box());

        attempt_failed(&mut state, build_box(), newer_over_ssh());

        assert_eq!(update(&state), None);
    }

    #[test]
    fn cancel_ends_the_wait_and_update_now_skips_it() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        wait_for_idle(&mut state, build_box());

        cancel_wait(&mut state, build_box());
        assert_eq!(update(&state), None);
        assert_eq!(
            state.ui.dialog,
            Some(Dialog::DaemonUpdate(build_box())),
            "the offer stays"
        );

        start(&mut state, build_box());
        wait_for_idle(&mut state, build_box());
        assert!(start_now(&mut state, build_box()).is_some());
        assert!(matches!(update(&state), Some(DaemonUpdate::Running(_))));
    }

    #[test]
    fn a_waiting_or_running_update_cannot_start_again() {
        let mut state = with_server_of_another_build(&[AgentStatus::Working]);
        start(&mut state, build_box());
        wait_for_idle(&mut state, build_box());

        assert_eq!(start(&mut state, build_box()), None);
        assert_eq!(wait_for_idle(&mut state, build_box()), None);

        start_now(&mut state, build_box()).unwrap();
        assert_eq!(start_now(&mut state, build_box()), None);
        assert_eq!(wait_for_idle(&mut state, build_box()), None);
    }

    #[test]
    fn a_servers_refusal_for_busy_agents_offers_update_now() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        finished(&mut state, build_box(), Some(3));
        let Some(DaemonUpdate::Failed(refused)) = update(&state).cloned() else {
            panic!("expected a failure");
        };
        assert!(refused.offers_update_now());

        let now = start_now(&mut state, build_box()).unwrap();

        assert_eq!(now.busy, BusyAgents::Interrupt);
        finished(&mut state, build_box(), Some(1));
        let Some(DaemonUpdate::Failed(failed)) = update(&state) else {
            panic!("expected a failure");
        };
        assert!(!failed.offers_update_now());
    }

    #[test]
    fn a_servers_run_survives_a_reconnection_to_its_old_daemon() {
        let mut state = with_server_of_another_build(&[]);
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: building 3f9c2e1");
        drop_connection(&mut state, build_box());

        connect(&mut state, build_box(), &other_build());

        assert_eq!(progress(&state).step, UpdateStep::Build);
    }

    #[test]
    fn a_script_that_ends_once_the_new_daemon_answers_needs_no_retry() {
        let mut state = with_server_of_another_build(&[]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(build_box()));
        start(&mut state, build_box()).unwrap();
        stdout(&mut state, "update-daemon: restarting the daemon");
        drop_connection(&mut state, build_box());
        connect(&mut state, build_box(), &other_build());

        assert!(!finished(&mut state, build_box(), Some(0)));

        assert_eq!(update(&state), None);
        assert_eq!(
            update_line(known(&state)).map(|line| line.text),
            Some("Update available."),
            "the build still differs"
        );
    }

    #[test]
    fn the_services_restart_steps_through_stop_start_and_reconnect() {
        let mut state = with_local_service(&[]);

        let started = start(&mut state, MachineId::THIS).unwrap();

        assert_eq!(started, run(service_plan()));
        let step = |state: &AppState| match this_update(state) {
            Some(DaemonUpdate::Running(progress) | DaemonUpdate::Reconnecting(progress)) => {
                (progress.step, progress.offers_cancel())
            }
            other => panic!("expected an update under way, got {other:?}"),
        };
        assert_eq!(step(&state), (UpdateStep::Stop, false));
        assert_eq!(ask_cancel(&mut state, MachineId::THIS), None);

        step_started(&mut state, MachineId::THIS, UpdateStep::Start);
        assert_eq!(step(&state), (UpdateStep::Start, false));

        assert!(restart_ended(&mut state, MachineId::THIS, Ok(())));
        assert!(matches!(
            this_update(&state),
            Some(DaemonUpdate::Reconnecting(_))
        ));
        assert_eq!(step(&state), (UpdateStep::Reconnect, false));
    }

    #[test]
    fn the_restarted_service_of_this_apps_build_ends_the_update() {
        let mut state = with_local_service(&[]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(MachineId::THIS));
        start(&mut state, MachineId::THIS).unwrap();
        restart_ended(&mut state, MachineId::THIS, Ok(()));
        drop_connection(&mut state, MachineId::THIS);
        let app = app_build(&state);

        connect(&mut state, MachineId::THIS, &app);

        assert_eq!(this_update(&state), None);
        assert_eq!(state.machines.this.version_gap, None);
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn a_service_another_program_restarted_is_another_build_that_answers() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        restart_ended(&mut state, MachineId::THIS, Ok(()));
        drop_connection(&mut state, MachineId::THIS);

        connect(&mut state, MachineId::THIS, &other_build());

        let Some(DaemonUpdate::Failed(failed)) = this_update(&state) else {
            panic!("expected a failure, got {:?}", this_update(&state));
        };
        assert_eq!(failed.failure, UpdateFailure::OtherBuildAnswers);
        assert!(failed.message.contains("9d8c7b6"), "{}", failed.message);
        assert_eq!(
            update_line(&state.machines.this).map(|line| (line.text, line.down)),
            Some(("Was not updated.", false))
        );
    }

    #[test]
    fn a_connection_once_the_daemon_stopped_is_judged_before_the_start_ends() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        step_started(&mut state, MachineId::THIS, UpdateStep::Start);
        drop_connection(&mut state, MachineId::THIS);

        connect(&mut state, MachineId::THIS, &other_build());

        assert!(matches!(
            this_update(&state),
            Some(DaemonUpdate::Failed(FailedUpdate {
                failure: UpdateFailure::OtherBuildAnswers,
                ..
            }))
        ));
        assert!(
            !restart_ended(&mut state, MachineId::THIS, Ok(())),
            "the failure stays"
        );
    }

    #[test]
    fn a_connection_before_the_stop_reaches_the_old_daemon() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        drop_connection(&mut state, MachineId::THIS);

        connect(&mut state, MachineId::THIS, &other_build());

        assert!(matches!(
            this_update(&state),
            Some(DaemonUpdate::Running(UpdateProgress {
                step: UpdateStep::Stop,
                ..
            }))
        ));
    }

    /// Another program, such as a systemd unit with `Restart=always`, restarts the old daemon
    /// the moment the stop ends it: the app reconnects to it before the stop returns, then the
    /// start finds a daemon that answers.
    #[test]
    fn an_old_daemon_restarted_during_the_stop_fails_the_update_once_the_restart_ends() {
        let mut state = with_local_service(&[]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(MachineId::THIS));
        start(&mut state, MachineId::THIS).unwrap();
        step_started(&mut state, MachineId::THIS, UpdateStep::Stop);
        drop_connection(&mut state, MachineId::THIS);
        connect(&mut state, MachineId::THIS, &other_build());
        assert!(
            matches!(this_update(&state), Some(DaemonUpdate::Running(_))),
            "the stop has not ended yet"
        );
        step_started(&mut state, MachineId::THIS, UpdateStep::Start);

        let retry = restart_ended(&mut state, MachineId::THIS, Ok(()));

        assert!(!retry, "the connection the app has is the daemon to judge");
        let Some(DaemonUpdate::Failed(failed)) = this_update(&state) else {
            panic!("expected a failure, got {:?}", this_update(&state));
        };
        assert_eq!(failed.failure, UpdateFailure::OtherBuildAnswers);
        assert_eq!(state.ui.dialog, None, "the update ran beside the window");
        assert_eq!(
            update_line(&state.machines.this).map(|line| line.link),
            Some(UpdateLink::Show),
            "the sidebar's line shows the failure"
        );
        assert_eq!(
            start(&mut state, MachineId::THIS),
            Some(run(service_plan())),
            "Retry runs again"
        );
    }

    #[test]
    fn this_apps_build_reached_during_the_stop_ends_the_update() {
        let mut state = with_local_service(&[]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(MachineId::THIS));
        start(&mut state, MachineId::THIS).unwrap();
        step_started(&mut state, MachineId::THIS, UpdateStep::Stop);
        drop_connection(&mut state, MachineId::THIS);
        let app = app_build(&state);
        connect(&mut state, MachineId::THIS, &app);

        let retry = restart_ended(&mut state, MachineId::THIS, Ok(()));

        assert!(!retry);
        assert_eq!(this_update(&state), None);
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn a_connection_made_before_the_stop_waits_for_the_next_one() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        drop_connection(&mut state, MachineId::THIS);
        connect(&mut state, MachineId::THIS, &other_build());
        step_started(&mut state, MachineId::THIS, UpdateStep::Stop);
        step_started(&mut state, MachineId::THIS, UpdateStep::Start);

        let retry = restart_ended(&mut state, MachineId::THIS, Ok(()));

        assert!(retry);
        assert!(matches!(
            this_update(&state),
            Some(DaemonUpdate::Reconnecting(_))
        ));
    }

    #[test]
    fn a_daemon_of_an_older_protocol_after_the_services_restart_is_another_programs() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        restart_ended(&mut state, MachineId::THIS, Ok(()));

        attempt_failed(&mut state, MachineId::THIS, Some(older()));

        assert!(matches!(
            this_update(&state),
            Some(DaemonUpdate::Failed(FailedUpdate {
                failure: UpdateFailure::OtherBuildAnswers,
                ..
            }))
        ));
    }

    #[test]
    fn a_stop_that_failed_says_why_and_which_process_to_stop() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        let reason = "the daemon, process 4242, did not stop within 10 seconds".to_owned();

        let ended = restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::NotStopped(StopFailure {
                reason: reason.clone(),
                by_hand: Some(HandStop::Process(ProcessId::new(4242))),
            })),
        );

        assert!(!ended);
        let Some(DaemonUpdate::Failed(failed)) = this_update(&state).cloned() else {
            panic!("expected a failure");
        };
        assert_eq!(failed.message, reason);
        let text = failure_text(&failed, "This computer");
        assert_eq!(
            text.hint.as_deref(),
            Some("Stop process 4242 yourself, then retry.")
        );
        assert_eq!(
            start(&mut state, MachineId::THIS),
            Some(run(service_plan())),
            "retry"
        );
    }

    #[test]
    fn a_service_too_old_to_record_its_process_is_stopped_by_hand_once() {
        let failed = FailedUpdate {
            plan: service_plan(),
            failure: UpdateFailure::NotStopped {
                by_hand: Some(HandStop::Unrecorded),
            },
            message: "/home/someone/.local/state/yetack/serve.lock names no process".to_owned(),
        };

        let text = failure_text(&failed, "This computer");

        assert_eq!(
            text.hint.as_deref(),
            Some(
                "Stop the yetack serve of /home/someone/.local/state/yetack yourself, once, then retry. From then on, the app stops the daemon itself."
            )
        );
    }

    #[test]
    fn a_start_that_failed_shows_the_error_and_the_end_of_the_log() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        step_started(&mut state, MachineId::THIS, UpdateStep::Start);

        restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::NotStarted {
                error: "the service stopped as it started (exit status: 1): made-up".to_owned(),
                log: Some("yetack: made-up".to_owned()),
            }),
        );

        let Some(DaemonUpdate::Failed(failed)) = this_update(&state) else {
            panic!("expected a failure");
        };
        assert_eq!(failed.failure, UpdateFailure::NotStarted);
        assert_eq!(
            failed.message,
            "the service stopped as it started (exit status: 1): made-up\n\nyetack: made-up"
        );
        assert_eq!(
            update_line(&state.machines.this).map(|line| line.text),
            Some("Update failed during the restart.")
        );
    }

    /// This computer's service, whose daemon refuses the app for an older protocol.
    fn with_local_service_that_refuses(agents: &[AgentStatus]) -> AppState {
        let mut state = with_local_service(agents);
        drop_connection(&mut state, MachineId::THIS);
        let refusal = update_offer::protocol_gap(
            DaemonPlace::Service,
            &refused("the daemon speaks protocol 1, the client 4"),
        );
        attempt_failed(&mut state, MachineId::THIS, refusal.or(Some(older())));
        state
    }

    #[test]
    fn a_service_that_refuses_the_app_is_asked_about_since_its_agents_cannot_be_seen() {
        let mut state = with_local_service_that_refuses(&[AgentStatus::Working]);
        assert_eq!(
            state.machines.this.version_gap.as_ref().map(|gap| gap.kind),
            Some(GapKind::OlderProtocol)
        );

        assert_eq!(
            jobs_to_check(&mut state, MachineId::THIS),
            None,
            "no log to read first"
        );
        assert_eq!(start(&mut state, MachineId::THIS), None);

        assert_eq!(
            state.ui.dialog,
            Some(Dialog::ConfirmUpdate(MachineId::THIS))
        );
        assert_eq!(this_update(&state), None, "nothing stops before the answer");
        assert!(agents_unseen(&state.machines.this));
        assert_eq!(
            start_now(&mut state, MachineId::THIS),
            Some(UpdateRun {
                plan: service_plan(),
                busy: BusyAgents::Interrupt
            })
        );
    }

    #[test]
    fn the_confirmation_for_unseen_agents_says_the_list_is_from_before() {
        let mut state = with_local_service_that_refuses(&[AgentStatus::Working, AgentStatus::Idle]);
        start(&mut state, MachineId::THIS);

        let asked = confirmation(&state, MachineId::THIS, "Local").unwrap();

        assert!(
            asked
                .text
                .contains("The app cannot list Local's agents while its daemon refuses the app."),
            "{}",
            asked.text
        );
        assert_eq!(
            asked.list_note,
            Some("These agents were busy when the app last saw them:")
        );
        assert_eq!(asked.agents, [AgentId::new(1)]);
        assert!(!asked.offers_wait);
    }

    #[test]
    fn the_confirmation_for_unseen_agents_lists_none_the_app_never_saw() {
        let mut state = with_local_service_that_refuses(&[]);
        start(&mut state, MachineId::THIS);

        let asked = confirmation(&state, MachineId::THIS, "Local").unwrap();

        assert_eq!(asked.list_note, None);
        assert_eq!(asked.agents, []);
    }

    #[test]
    fn the_confirmation_of_seen_agents_needs_no_note() {
        let mut state = with_local_service(&[AgentStatus::Working, AgentStatus::Idle]);
        jobs_checked(&mut state, MachineId::THIS, vec![AgentId::new(2)]);

        let mut asked = confirmation(&state, MachineId::THIS, "Local").unwrap();
        asked.agents.sort();

        assert_eq!(asked.list_note, None);
        assert_eq!(asked.agents, [AgentId::new(1), AgentId::new(2)]);
        assert!(asked.offers_wait);
    }

    #[test]
    fn a_refusal_for_scheduled_jobs_lists_only_the_agents_that_may_hold_one() {
        let mut state = with_local_service(&[
            AgentStatus::Working,
            AgentStatus::NeedsYou,
            AgentStatus::Idle,
        ]);
        start_now(&mut state, MachineId::THIS).unwrap();
        restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::HoldsJobs(vec![AgentId::new(3)])),
        );
        let Some(DaemonUpdate::Failed(failed)) = this_update(&state).cloned() else {
            panic!("expected a failure, got {:?}", this_update(&state));
        };

        assert_eq!(
            failure_agents(&state, MachineId::THIS, &failed),
            [AgentId::new(3)]
        );
        let other = FailedUpdate {
            failure: UpdateFailure::NotStarted,
            ..failed
        };
        assert_eq!(failure_agents(&state, MachineId::THIS, &other), []);
    }

    #[test]
    fn a_server_that_refuses_the_app_updates_at_once_and_its_script_checks() {
        let state = with_older_server();

        assert!(!agents_unseen(known(&state)));
    }

    #[test]
    fn a_service_with_no_daemon_left_restarts_at_once() {
        let mut state = with_local_service(&[]);
        start(&mut state, MachineId::THIS).unwrap();
        restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::NotStarted {
                error: "made-up".to_owned(),
                log: None,
            }),
        );
        drop_connection(&mut state, MachineId::THIS);
        attempt_failed(&mut state, MachineId::THIS, None);

        assert_eq!(
            start(&mut state, MachineId::THIS),
            Some(run(service_plan()))
        );
    }

    /// "Update now" interrupted a working agent, and the new service did not start: the app
    /// still shows the agent as it last saw it.
    #[test]
    fn an_agent_seen_working_before_the_daemon_went_runs_nothing_now() {
        let mut state = with_local_service(&[AgentStatus::Working]);
        assert!(runs_something_now(&state, MachineId::THIS));
        start_now(&mut state, MachineId::THIS).unwrap();
        restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::NotStarted {
                error: "made-up".to_owned(),
                log: None,
            }),
        );
        drop_connection(&mut state, MachineId::THIS);
        attempt_failed(&mut state, MachineId::THIS, None);

        assert_eq!(
            start(&mut state, MachineId::THIS),
            Some(run(service_plan()))
        );
        assert!(!runs_something_now(&state, MachineId::THIS));
    }

    #[test]
    fn a_connected_service_reads_the_jobs_of_its_resting_agents_first() {
        let mut state =
            with_local_service(&[AgentStatus::Working, AgentStatus::Idle, AgentStatus::Exited]);

        let check = jobs_to_check(&mut state, MachineId::THIS).unwrap();

        assert_eq!(check.agents, [AgentId::new(2)]);
        assert_eq!(UpdatePlan::Service(check.plan), service_plan());
        assert_eq!(
            jobs_to_check(
                &mut with_server_of_another_build(&[AgentStatus::Idle]),
                build_box()
            ),
            None,
            "a server's script reads its own"
        );
    }

    /// A second click on "Update Local" while the first one's read runs.
    #[test]
    fn a_second_click_during_the_read_of_the_jobs_starts_nothing() {
        let mut state = with_local_service(&[AgentStatus::Idle]);
        assert!(jobs_to_check(&mut state, MachineId::THIS).is_some());

        let again = jobs_to_check(&mut state, MachineId::THIS);
        let started = start(&mut state, MachineId::THIS);

        assert_eq!(again, None);
        assert_eq!(started, None);
        assert_eq!(this_update(&state), None);
        let line = update_line(&state.machines.this).unwrap();
        assert_eq!(
            (line.text, line.link, line.down),
            ("Checking scheduled jobs…", UpdateLink::Show, false)
        );
        assert_eq!(
            jobs_checked(&mut state, MachineId::THIS, Vec::new()),
            Some(run(service_plan())),
            "the first read starts the update"
        );
        assert!(!state.machines.this.checking_jobs);
    }

    #[test]
    fn an_idle_agent_that_may_hold_a_scheduled_job_is_asked_about() {
        let mut state = with_local_service(&[AgentStatus::Idle, AgentStatus::Idle]);

        let started = jobs_checked(&mut state, MachineId::THIS, vec![AgentId::new(2)]);

        assert_eq!(started, None);
        assert_eq!(
            state.ui.dialog,
            Some(Dialog::ConfirmUpdate(MachineId::THIS))
        );
        assert_eq!(
            update_offer::busy_agents(&state, MachineId::THIS),
            [AgentId::new(2)]
        );
        assert_eq!(update_offer::running_agents(&state, MachineId::THIS), []);
    }

    #[test]
    fn with_no_job_held_the_idle_service_restarts_at_once() {
        let mut state = with_local_service(&[AgentStatus::Idle]);

        let started = jobs_checked(&mut state, MachineId::THIS, Vec::new());

        assert_eq!(started, Some(run(service_plan())));
    }

    /// Two clicks on "Update" read the logs twice: the first read starts the update.
    #[test]
    fn a_read_that_ends_once_the_update_runs_keeps_no_holder() {
        let mut state = with_local_service(&[AgentStatus::Idle]);
        jobs_checked(&mut state, MachineId::THIS, Vec::new()).unwrap();

        let second = jobs_checked(&mut state, MachineId::THIS, vec![AgentId::new(1)]);

        assert_eq!(second, None);
        assert_eq!(state.machines.this.job_holders, []);
    }

    #[test]
    fn a_wait_for_idle_agents_does_not_wait_for_a_scheduled_job() {
        let mut state = with_local_service(&[AgentStatus::Working, AgentStatus::Idle]);
        jobs_checked(&mut state, MachineId::THIS, vec![AgentId::new(2)]);

        assert_eq!(wait_for_idle(&mut state, MachineId::THIS), None);
        set_status(&mut state, AgentId::new(1), AgentStatus::Idle);

        assert_eq!(
            start_when_idle(&mut state, MachineId::THIS),
            Some(run(service_plan())),
            "the restart reads the jobs again"
        );
        assert_eq!(state.machines.this.job_holders, [], "a run starts afresh");
    }

    /// An agent started a turn while the restart read the raw logs.
    #[test]
    fn a_restart_refused_for_an_agent_that_started_to_work_lists_it_without_the_servers_hint() {
        let mut state = with_local_service(&[AgentStatus::Idle, AgentStatus::Idle]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(MachineId::THIS));
        start(&mut state, MachineId::THIS).unwrap();
        set_status(&mut state, AgentId::new(2), AgentStatus::Working);

        restart_ended(&mut state, MachineId::THIS, Err(RestartFailure::Busy));

        let Some(DaemonUpdate::Failed(failed)) = this_update(&state).cloned() else {
            panic!("expected a failure, got {:?}", this_update(&state));
        };
        assert_eq!(failed.failure, UpdateFailure::Refused);
        assert_eq!(
            failure_agents(&state, MachineId::THIS, &failed),
            [AgentId::new(2)]
        );
        assert_eq!(
            failed.stop_hint(),
            None,
            "the hint runs a command on a server"
        );
        assert!(failed.offers_update_now());
        let server = FailedUpdate {
            plan: plan(),
            ..failed
        };
        assert_eq!(server.stop_hint(), Some("<id>"));
        assert_eq!(failure_agents(&state, MachineId::THIS, &server), []);
    }

    #[test]
    fn a_restart_refused_for_scheduled_jobs_lists_them_and_offers_update_now() {
        let mut state = with_local_service(&[AgentStatus::Idle]);
        state.ui.dialog = Some(Dialog::DaemonUpdate(MachineId::THIS));
        start(&mut state, MachineId::THIS).unwrap();

        let ended = restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::HoldsJobs(vec![AgentId::new(1)])),
        );

        assert!(!ended);
        let Some(DaemonUpdate::Failed(failed)) = this_update(&state).cloned() else {
            panic!("expected a failure, got {:?}", this_update(&state));
        };
        assert_eq!(failed.failure, UpdateFailure::HoldsJobs);
        assert!(failed.offers_update_now());
        assert_eq!(
            update_offer::busy_agents(&state, MachineId::THIS),
            [AgentId::new(1)]
        );
        assert_eq!(
            update_line(&state.machines.this).map(|line| line.text),
            Some("Was not updated.")
        );
        assert_eq!(
            failure_text(&failed, "Local").lead,
            "These agents may hold scheduled jobs, which a restart would lose. Delete the jobs or stop the agents, then retry."
        );
        assert_eq!(
            start_now(&mut state, MachineId::THIS).map(|run| run.busy),
            Some(BusyAgents::Interrupt)
        );
    }

    #[test]
    fn this_computers_update_takes_the_window_only_while_its_daemon_cannot_be_used() {
        let mut state = with_local_service(&[AgentStatus::Working]);
        let takes = |state: &AppState| takes_the_window(&state.machines.this);

        start(&mut state, MachineId::THIS);
        wait_for_idle(&mut state, MachineId::THIS);
        assert!(!takes(&state), "a wait leaves the agents in use");

        start_now(&mut state, MachineId::THIS).unwrap();
        assert!(!takes(&state), "the old daemon still answers");
        drop_connection(&mut state, MachineId::THIS);
        assert!(takes(&state));

        restart_ended(
            &mut state,
            MachineId::THIS,
            Err(RestartFailure::NotStarted {
                error: "made-up".to_owned(),
                log: None,
            }),
        );
        assert!(takes(&state), "the failure says no daemon runs");
        back(&mut state, MachineId::THIS);
        assert!(!takes(&state));
    }

    #[test]
    fn this_computers_screens_open_as_any_servers() {
        let mut state = with_server();

        open(&mut state, MachineId::THIS);
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(MachineId::THIS)));

        open(&mut state, build_box());
        assert_eq!(state.ui.dialog, Some(Dialog::DaemonUpdate(build_box())));
    }

    #[test]
    fn a_follow_up_edit_moves_the_plan_and_the_failed_one_but_not_a_run() {
        let mut state = with_older_server();
        start(&mut state, build_box()).unwrap();
        let config = MachineConfig {
            name: "lab".to_owned(),
            host: "dev@lab".to_owned(),
            remote_command: "/srv/yetack".to_owned(),
            source_dir: Some("/src/yetack".to_owned()),
        };

        follow_config(known_mut(&mut state), &config);
        assert_eq!(progress(&state).plan, plan(), "the run keeps its server");

        finished(&mut state, build_box(), Some(1));
        follow_config(known_mut(&mut state), &config);
        let moved = UpdatePlan::Server(ServerPlan {
            host: "dev@lab".to_owned(),
            remote_command: "/srv/yetack".to_owned(),
            source_dir: Some("/src/yetack".to_owned()),
            commit: server_plan().commit,
            carried: CarriedDaemons::default(),
            daemon: None,
        });
        assert_eq!(known(&state).update_plan, Some(moved.clone()));
        let Some(DaemonUpdate::Failed(failed)) = update(&state) else {
            panic!("expected a failure");
        };
        assert_eq!(failed.plan, moved);
    }

    fn failed_with(failure: UpdateFailure) -> FailedUpdate {
        FailedUpdate {
            plan: plan(),
            failure,
            message: String::new(),
        }
    }

    #[test]
    fn a_cancel_the_user_asked_for_reads_as_one() {
        let lead = |failure| failure_text(&failed_with(failure), "build-box").lead;

        assert_eq!(
            lead(UpdateFailure::Cancelled),
            "The update was cancelled, so build-box still runs its old version."
        );
        assert_eq!(
            lead(UpdateFailure::Failed),
            "The update failed, so build-box still runs its old version."
        );
    }

    #[test]
    fn a_rollback_and_another_update_have_their_own_words() {
        let text = |failure| {
            let text = failure_text(&failed_with(failure), "build-box");
            (text.title, text.lead)
        };

        assert_eq!(
            text(UpdateFailure::RolledBack),
            (
                "build-box was not updated".to_owned(),
                "The new version did not start, so build-box runs its old version again."
                    .to_owned()
            )
        );
        assert_eq!(
            text(UpdateFailure::AlreadyRunning),
            (
                "An update is already running on build-box".to_owned(),
                "Try again in a few minutes.".to_owned()
            )
        );
    }

    #[test]
    fn every_failure_of_the_service_says_what_to_do() {
        for failure in [
            UpdateFailure::NotStopped {
                by_hand: Some(HandStop::Process(ProcessId::new(4242))),
            },
            UpdateFailure::NotStopped {
                by_hand: Some(HandStop::Unrecorded),
            },
            UpdateFailure::NotStopped {
                by_hand: Some(HandStop::Unverified),
            },
            UpdateFailure::OtherBuildAnswers,
        ] {
            let failed = FailedUpdate {
                plan: service_plan(),
                failure,
                message: String::new(),
            };

            let text = failure_text(&failed, "This computer");

            assert!(text.hint.is_some(), "{failure:?}");
        }
    }
}

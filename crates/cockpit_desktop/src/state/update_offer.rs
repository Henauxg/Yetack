//! Where each machine's daemon stands against this app, and what the app offers for it: an update
//! when the daemon refuses the app for an older protocol or runs another build, nothing when it is
//! newer.

use std::cmp::Ordering;
use std::fmt;

use cockpit_client::{ConnectError, SessionError};
use cockpit_protocol::{AgentId, AgentStatus, BuildCommit, PROTOCOL_VERSION, ReleaseVersion};

use super::accounts;
use super::app_state::{AgentEntry, AppState, MachineState};
use super::no_daemon::SERVICE_OFFERED;
use super::update_plan::UpdatePlan;
use crate::daemon_label::DaemonPlace;
use crate::machine::MachineId;

/// A program's release, and the commit it was built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramBuild {
    pub version: ReleaseVersion,
    /// `None` when its build did not know it.
    pub commit: Option<BuildCommit>,
}

impl ProgramBuild {
    /// This app's build, which an update installs: the app builds the daemon from its own tree,
    /// so a daemon built from the same tree says the same.
    pub fn this_program() -> Self {
        Self {
            version: ReleaseVersion::new(cockpit_daemon::RELEASE_VERSION.to_owned()),
            commit: cockpit_daemon::build_commit(),
        }
    }

    /// The build `known`'s daemon said it runs, while the app is connected to it. An earlier
    /// connection's may no longer be the one that runs.
    pub fn daemon_of(known: &MachineState) -> Option<Self> {
        if !known.is_connected() {
            return None;
        }
        Some(Self {
            version: known.daemon_version.clone()?,
            commit: known.daemon_commit.clone(),
        })
    }
}

/// "0.1.0 (3f9c2e1)", or "0.1.0" without a commit.
impl fmt::Display for ProgramBuild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.commit {
            Some(commit) => write!(formatter, "{} ({})", self.version, commit.short()),
            None => write!(formatter, "{}", self.version),
        }
    }
}

/// Where a machine's daemon stands against this app, when it is not this app's build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapKind {
    /// It refuses the app for an older protocol: the app cannot use the machine until it is
    /// updated.
    OlderProtocol,
    /// The app is connected, and the daemon runs an older release, or the same release from
    /// another commit.
    OtherBuild,
    /// The app is connected, and the daemon runs a newer release: the app is the one to update.
    NewerRelease,
    /// It refuses the app for a newer protocol.
    NewerProtocol,
}

impl GapKind {
    /// Whether the daemon is newer than the app, so that an update would move it backwards.
    pub fn is_newer(self) -> bool {
        match self {
            Self::NewerRelease | Self::NewerProtocol => true,
            Self::OlderProtocol | Self::OtherBuild => false,
        }
    }
}

/// A daemon that does not run this app's build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionGap {
    pub kind: GapKind,
    /// The refusal as the daemon or the client worded it, or the two builds.
    pub details: String,
    /// The daemon's build: the one it runs while connected, or the one its refusal names. A
    /// daemon of protocol 8 or older names none as it refuses.
    pub daemon: Option<ProgramBuild>,
    /// The protocol the daemon speaks.
    pub protocol: u16,
}

impl VersionGap {
    /// The daemon runs this app's release, built from another commit. Two commits of one release
    /// have no order the app knows, so neither build is the update.
    pub fn is_other_commit(&self) -> bool {
        self.kind == GapKind::OtherBuild
            && self
                .daemon
                .as_ref()
                .is_some_and(|daemon| daemon.version == ProgramBuild::this_program().version)
    }
}

/// The daemon's protocol, from a first connection that failed, when it differs from the app's. An
/// older daemon counts only where the app can update it: over SSH, and this computer's service or
/// socket where the app runs the service. Any other keeps the "No daemon answers" screen.
pub fn protocol_gap(place: DaemonPlace, error: &SessionError) -> Option<VersionGap> {
    let SessionError::Connect(connect) = error else {
        return None;
    };
    let protocol = connect.daemon_protocol()?;
    let kind = match protocol.cmp(&PROTOCOL_VERSION) {
        Ordering::Less if updates_older(place) => GapKind::OlderProtocol,
        Ordering::Greater => GapKind::NewerProtocol,
        Ordering::Less | Ordering::Equal => return None,
    };
    let (details, daemon) = match connect {
        ConnectError::Refused {
            reason,
            daemon_version,
            daemon_commit,
        } => (
            reason.clone(),
            daemon_version.clone().map(|version| ProgramBuild {
                version,
                commit: daemon_commit.clone(),
            }),
        ),
        other => (other.to_string(), None),
    };
    Some(VersionGap {
        kind,
        details,
        daemon,
        protocol,
    })
}

/// Whether the app updates a daemon at `place` of an older protocol. The daemon in the app runs
/// the app's own code.
fn updates_older(place: DaemonPlace) -> bool {
    match place {
        DaemonPlace::Ssh => true,
        DaemonPlace::Service | DaemonPlace::Socket => SERVICE_OFFERED,
        DaemonPlace::InApp => false,
    }
}

/// How a connected daemon's build stands against the app's: `None` when it is the same, or when
/// the app cannot tell, as with a release it cannot read or a commit either build does not know.
pub fn build_gap(daemon: &ProgramBuild, app: &ProgramBuild) -> Option<VersionGap> {
    let kind = match release_order(&daemon.version, &app.version)? {
        Ordering::Less => GapKind::OtherBuild,
        Ordering::Greater => GapKind::NewerRelease,
        Ordering::Equal => match (&daemon.commit, &app.commit) {
            (Some(daemon), Some(app)) if daemon != app => GapKind::OtherBuild,
            (Some(_) | None, _) => return None,
        },
    };
    Some(VersionGap {
        kind,
        details: format!("the daemon runs version {daemon}, this app version {app}"),
        daemon: Some(daemon.clone()),
        protocol: PROTOCOL_VERSION,
    })
}

/// How two releases compare, as Cargo versions do: "0.10.0" is after "0.9.0", and "1.0.0-beta"
/// before "1.0.0". `None` when either is not such a version.
pub fn release_order(left: &ReleaseVersion, right: &ReleaseVersion) -> Option<Ordering> {
    let (left_numbers, left_pre) = parse_release(left.as_str())?;
    let (right_numbers, right_pre) = parse_release(right.as_str())?;
    let order = left_numbers
        .cmp(&right_numbers)
        .then_with(|| match (left_pre, right_pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => pre_release_order(left, right),
        });
    Some(order)
}

/// The three numbers of a release, and its pre-release. Build metadata, after `+`, counts for
/// nothing.
fn parse_release(text: &str) -> Option<([u64; 3], Option<&str>)> {
    let text = text.split_once('+').map_or(text, |(release, _)| release);
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    let mut parts = core.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some((numbers, pre))
}

/// Semantic versioning's order of pre-releases: identifier by identifier, numbers by value and
/// before words, and a longer list after its own start.
fn pre_release_order(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let order = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => match (left.parse::<u64>(), right.parse::<u64>()) {
                (Ok(left), Ok(right)) => left.cmp(&right),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => left.cmp(right),
            },
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// What the app offers for a machine whose daemon does not run this app's build: the data the
/// update screens show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOffer {
    pub kind: GapKind,
    /// How an update would run. `None` when the app cannot update the daemon: it is newer, or the
    /// app does not know its own commit for a server's build.
    pub plan: Option<UpdatePlan>,
    /// The daemon's build, as `VersionGap::daemon` has it.
    pub daemon: Option<ProgramBuild>,
    /// The protocol the daemon speaks.
    pub daemon_protocol: u16,
    /// This app's build, which an update installs.
    pub app: ProgramBuild,
    /// The refusal, or the two builds, in words.
    pub details: String,
}

/// What the app offers for `machine`, `None` when its daemon runs this app's build or the app
/// cannot tell.
pub fn offer(state: &AppState, machine: MachineId) -> Option<UpdateOffer> {
    let known = state.machines.get(machine)?;
    let gap = known.version_gap.as_ref()?;
    let plan = if gap.kind.is_newer() {
        None
    } else {
        known.update_plan.clone()
    };
    Some(UpdateOffer {
        kind: gap.kind,
        plan,
        daemon: gap.daemon.clone(),
        daemon_protocol: gap.protocol,
        app: state.app_build.clone(),
        details: gap.details.clone(),
    })
}

/// The agents of `machine` that a restart would hurt, in the sidebar's order: the running ones
/// ([`running_agents`]), and the resting ones that may hold a scheduled job, which it would lose
/// (`MachineState::job_holders`).
pub fn busy_agents(state: &AppState, machine: MachineId) -> Vec<AgentId> {
    let holders = job_holders(state, machine);
    agents_where(state, machine, |entry| {
        runs_something(entry) || (rests(entry) && holders.contains(&entry.id()))
    })
}

/// The resting agents of `machine` that may hold a scheduled job, in the sidebar's order.
pub fn job_holding_agents(state: &AppState, machine: MachineId) -> Vec<AgentId> {
    let holders = job_holders(state, machine);
    agents_where(state, machine, |entry| {
        rests(entry) && holders.contains(&entry.id())
    })
}

fn job_holders(state: &AppState, machine: MachineId) -> &[AgentId] {
    state
        .machines
        .get(machine)
        .map_or(&[], |known| known.job_holders.as_slice())
}

/// The agents of `machine` that a restart would interrupt, in the sidebar's order: those that
/// work or wait for an answer, and the idle ones whose background tasks still run.
pub fn running_agents(state: &AppState, machine: MachineId) -> Vec<AgentId> {
    agents_where(state, machine, runs_something)
}

/// The agents of `machine` that run no turn, in the sidebar's order: the idle ones, and those
/// that wait only for their account's sign-in. Their `claude` runs, so they may hold a scheduled
/// job.
pub fn resting_agents(state: &AppState, machine: MachineId) -> Vec<AgentId> {
    agents_where(state, machine, rests)
}

fn agents_where(
    state: &AppState,
    machine: MachineId,
    wanted: impl Fn(&AgentEntry) -> bool,
) -> Vec<AgentId> {
    state
        .agents_in_order()
        .into_iter()
        .filter(|entry| MachineId::of(entry.id()) == machine && wanted(entry))
        .map(AgentEntry::id)
        .collect()
}

fn runs_something(entry: &AgentEntry) -> bool {
    match accounts::turn_status(&entry.summary) {
        AgentStatus::Working | AgentStatus::NeedsYou => true,
        AgentStatus::Idle => entry.summary.background_running > 0,
        AgentStatus::Exited => false,
    }
}

/// An agent that waits only for its account's sign-in has the status `Idle` here.
fn rests(entry: &AgentEntry) -> bool {
    accounts::turn_status(&entry.summary) == AgentStatus::Idle
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::time::Instant;

    use cockpit_protocol::SignInWait;

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::ConnectionState;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::update_plan::{ServerPlan, ServicePlan};

    fn refused(reason: &str) -> SessionError {
        SessionError::Connect(ConnectError::Refused {
            reason: reason.to_owned(),
            daemon_version: None,
            daemon_commit: None,
        })
    }

    fn older_daemon() -> SessionError {
        refused("the daemon speaks protocol 1, the client 4")
    }

    fn newer_daemon() -> SessionError {
        refused(&format!(
            "the daemon speaks protocol {}, the client {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 1
        ))
    }

    fn release(text: &str) -> ReleaseVersion {
        ReleaseVersion::new(text.to_owned())
    }

    fn build(version: &str, commit: Option<&str>) -> ProgramBuild {
        ProgramBuild {
            version: release(version),
            commit: commit.map(|commit| BuildCommit::new(commit.to_owned())),
        }
    }

    const APP_COMMIT: &str = "3f9c2e1a7b4d5c6e8f9a0b1c2d3e4f5a6b7c8d9e";
    const OTHER_COMMIT: &str = "9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c";

    fn app() -> ProgramBuild {
        build("0.4.2", Some(APP_COMMIT))
    }

    #[test]
    fn an_older_daemon_over_ssh_is_older_and_a_newer_one_is_newer() {
        let older = protocol_gap(DaemonPlace::Ssh, &older_daemon()).unwrap();
        let newer = protocol_gap(DaemonPlace::Ssh, &newer_daemon()).unwrap();

        assert_eq!(older.kind, GapKind::OlderProtocol);
        assert_eq!(older.details, "the daemon speaks protocol 1, the client 4");
        assert_eq!(newer.kind, GapKind::NewerProtocol);
    }

    #[test]
    fn a_version_mismatch_counts_the_same_way() {
        let mismatch = SessionError::Connect(ConnectError::VersionMismatch {
            daemon: PROTOCOL_VERSION + 1,
        });

        let gap = protocol_gap(DaemonPlace::Ssh, &mismatch).unwrap();

        assert_eq!(gap.kind, GapKind::NewerProtocol);
        assert_eq!(
            gap.details,
            format!(
                "the daemon speaks protocol {}, this client {PROTOCOL_VERSION}",
                PROTOCOL_VERSION + 1
            )
        );
    }

    #[test]
    fn this_computers_service_or_socket_is_offered_an_update_where_the_app_runs_the_service() {
        for place in [DaemonPlace::Socket, DaemonPlace::Service] {
            let older = protocol_gap(place, &older_daemon()).map(|gap| gap.kind);

            assert_eq!(
                older,
                SERVICE_OFFERED.then_some(GapKind::OlderProtocol),
                "{place:?}"
            );
        }
    }

    #[test]
    fn the_daemon_in_the_app_is_never_older() {
        assert_eq!(protocol_gap(DaemonPlace::InApp, &older_daemon()), None);
    }

    #[test]
    fn a_newer_daemon_counts_wherever_it_runs() {
        for place in [
            DaemonPlace::Ssh,
            DaemonPlace::Socket,
            DaemonPlace::Service,
            DaemonPlace::InApp,
        ] {
            assert_eq!(
                protocol_gap(place, &newer_daemon()).map(|gap| gap.kind),
                Some(GapKind::NewerProtocol),
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
        assert_eq!(
            protocol_gap(
                DaemonPlace::Ssh,
                &refused("the first message must be a hello")
            ),
            None
        );
        assert_eq!(protocol_gap(DaemonPlace::Ssh, &other_error), None);
        assert_eq!(protocol_gap(DaemonPlace::Socket, &socket_error), None);
    }

    #[test]
    fn releases_compare_as_cargo_versions() {
        let order = |left, right| release_order(&release(left), &release(right));

        assert_eq!(order("0.1.0", "0.2.0"), Some(Ordering::Less));
        assert_eq!(order("0.10.0", "0.9.0"), Some(Ordering::Greater));
        assert_eq!(order("1.0.0", "0.99.99"), Some(Ordering::Greater));
        assert_eq!(order("0.4.2", "0.4.2"), Some(Ordering::Equal));
        assert_eq!(order("1.0.0+nix", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(order("1.0.0-beta", "1.0.0"), Some(Ordering::Less));
        assert_eq!(
            order("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            order("1.0.0-alpha.9", "1.0.0-alpha.beta"),
            Some(Ordering::Less)
        );
        assert_eq!(order("1.0.0-alpha", "1.0.0-alpha.1"), Some(Ordering::Less));
    }

    #[test]
    fn a_release_the_app_cannot_read_compares_with_nothing() {
        for unreadable in [
            "",
            "1.0",
            "1.0.0.0",
            "one.two.three",
            "1..0",
            "+1.0.0",
            "v1.0.0",
        ] {
            assert_eq!(
                release_order(&release(unreadable), &release("1.0.0")),
                None,
                "{unreadable:?}"
            );
        }
    }

    #[test]
    fn an_older_release_or_another_commit_of_the_same_one_is_another_build() {
        let older = build_gap(&build("0.4.1", Some(APP_COMMIT)), &app()).unwrap();
        let other_commit = build_gap(&build("0.4.2", Some(OTHER_COMMIT)), &app()).unwrap();

        assert_eq!(older.kind, GapKind::OtherBuild);
        assert_eq!(other_commit.kind, GapKind::OtherBuild);
        assert_eq!(
            other_commit.details,
            "the daemon runs version 0.4.2 (9d8c7b6), this app version 0.4.2 (3f9c2e1)"
        );
    }

    #[test]
    fn a_newer_release_is_newer_whatever_its_commit() {
        for commit in [Some(APP_COMMIT), Some(OTHER_COMMIT), None] {
            let gap = build_gap(&build("0.5.0", commit), &app()).unwrap();

            assert_eq!(gap.kind, GapKind::NewerRelease, "{commit:?}");
        }
    }

    #[test]
    fn the_same_release_with_a_commit_either_side_does_not_know_offers_nothing() {
        assert_eq!(build_gap(&build("0.4.2", Some(APP_COMMIT)), &app()), None);
        assert_eq!(build_gap(&build("0.4.2", None), &app()), None);
        assert_eq!(
            build_gap(&build("0.4.2", Some(OTHER_COMMIT)), &build("0.4.2", None)),
            None
        );
        assert_eq!(
            build_gap(&build("0.4.1", None), &build("0.4.2", None)).map(|gap| gap.kind),
            Some(GapKind::OtherBuild),
            "an older release needs no commit"
        );
    }

    fn server_plan() -> UpdatePlan {
        UpdatePlan::Server(ServerPlan {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
            commit: BuildCommit::new(APP_COMMIT.to_owned()),
        })
    }

    fn service_plan() -> UpdatePlan {
        UpdatePlan::Service(ServicePlan {
            state_dir: PathBuf::from("/home/someone/.local/state/agents-cockpit"),
            log_folder: PathBuf::from("/home/someone/.local/state/agents-cockpit-desktop"),
        })
    }

    /// This computer, connected to its service, which runs `daemon`.
    fn local_running(daemon: &ProgramBuild) -> AppState {
        let mut state = state_with(Vec::new());
        state.app_build = app();
        let this = &mut state.machines.this;
        this.daemon = DaemonLabel::service();
        this.connection = ConnectionState::Connected;
        this.daemon_version = Some(daemon.version.clone());
        this.daemon_commit.clone_from(&daemon.commit);
        this.version_gap = build_gap(daemon, &app());
        this.update_plan = Some(service_plan());
        state
    }

    #[test]
    fn a_connected_machine_of_another_build_is_offered_its_plan_with_both_builds() {
        let daemon = build("0.4.2", Some(OTHER_COMMIT));
        let state = local_running(&daemon);

        let offered = offer(&state, MachineId::THIS).unwrap();

        assert_eq!(offered.kind, GapKind::OtherBuild);
        assert_eq!(offered.plan, Some(service_plan()));
        assert_eq!(offered.daemon, Some(daemon));
        assert_eq!(offered.app, app());
    }

    #[test]
    fn a_newer_daemon_is_offered_no_plan() {
        let state = local_running(&build("0.5.0", None));

        let offered = offer(&state, MachineId::THIS).unwrap();

        assert_eq!(offered.kind, GapKind::NewerRelease);
        assert_eq!(offered.plan, None);
    }

    #[test]
    fn a_daemon_of_this_apps_build_is_offered_nothing() {
        let state = local_running(&app());

        assert_eq!(offer(&state, MachineId::THIS), None);
    }

    #[test]
    fn a_refusing_daemon_tells_no_build_even_one_it_told_before() {
        let mut state = state_with(Vec::new());
        let server = MachineId::new(1);
        let mut known = MachineState::new(DaemonLabel::ssh("build-box".to_owned()));
        known.daemon_version = Some(release("0.3.0"));
        known.connection = ConnectionState::Disconnected {
            error: "refused".to_owned(),
            retry_at: Instant::now(),
        };
        known.version_gap = protocol_gap(DaemonPlace::Ssh, &older_daemon());
        known.update_plan = Some(server_plan());
        state.machines.others.insert(server, known);

        let offered = offer(&state, server).unwrap();

        assert_eq!(offered.kind, GapKind::OlderProtocol);
        assert_eq!(offered.plan, Some(server_plan()));
        assert_eq!(offered.daemon, None);
    }

    #[test]
    fn busy_agents_are_the_working_and_waiting_ones_of_the_machine() {
        let server = MachineId::new(1);
        let mut sign_in_only = summary(4, AgentStatus::NeedsYou);
        sign_in_only.sign_in_wait = SignInWait::Waiting;
        let mut elsewhere = summary(5, AgentStatus::Working);
        elsewhere.id = server.app_agent(AgentId::new(5));
        let state = state_with(vec![
            summary(1, AgentStatus::Working),
            summary(2, AgentStatus::NeedsYou),
            summary(3, AgentStatus::Idle),
            sign_in_only,
            elsewhere,
            summary(6, AgentStatus::Exited),
        ]);

        let mut busy = busy_agents(&state, MachineId::THIS);
        busy.sort();

        assert_eq!(busy, [AgentId::new(1), AgentId::new(2)]);
        assert_eq!(
            busy_agents(&state, server),
            [server.app_agent(AgentId::new(5))]
        );
    }

    #[test]
    fn an_idle_agent_whose_background_tasks_run_is_busy() {
        let mut watching = summary(1, AgentStatus::Idle);
        watching.background_running = 2;
        let mut ended = summary(2, AgentStatus::Exited);
        ended.background_running = 1;
        let state = state_with(vec![watching, ended, summary(3, AgentStatus::Idle)]);

        assert_eq!(busy_agents(&state, MachineId::THIS), [AgentId::new(1)]);
    }

    #[test]
    fn a_resting_agent_that_may_hold_a_scheduled_job_is_busy_without_running_anything() {
        let mut sign_in_only = summary(4, AgentStatus::NeedsYou);
        sign_in_only.sign_in_wait = SignInWait::Waiting;
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Exited),
            summary(3, AgentStatus::Working),
            sign_in_only,
            summary(5, AgentStatus::Idle),
        ]);
        state.machines.this.job_holders = [1, 2, 3, 4].map(AgentId::new).to_vec();

        let mut busy = busy_agents(&state, MachineId::THIS);
        busy.sort();
        let mut resting = resting_agents(&state, MachineId::THIS);
        resting.sort();

        assert_eq!(
            busy,
            [1, 3, 4].map(AgentId::new),
            "an ended agent holds no job"
        );
        let mut holding = job_holding_agents(&state, MachineId::THIS);
        holding.sort();
        assert_eq!(
            holding,
            [1, 4].map(AgentId::new),
            "the working one is busy anyway"
        );
        assert_eq!(running_agents(&state, MachineId::THIS), [AgentId::new(3)]);
        assert_eq!(resting, [1, 4, 5].map(AgentId::new));
    }
}

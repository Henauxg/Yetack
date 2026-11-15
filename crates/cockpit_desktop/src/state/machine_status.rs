//! What the app says of each machine's state: a tone, the Machines dialog's chip, and the line
//! under the head of its panel in the sidebar while it does not answer or has an update to show.
//! Without machine panels, the sidebar shows these lines in a strip at the top of its list.

use super::app_state::{AppState, ConnectionState, MachineState};
use super::daemon_update::{self, DaemonUpdate, LineProgress, UpdateLink};
use super::machines::MachineName;
use super::no_daemon::Absence;
use super::update_offer::GapKind;
use crate::machine::MachineId;
use crate::prefs::GroupBy;

/// How a machine's state shows: the color of its chip, and of the mark before its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// It answers: green.
    Fine,
    /// Nothing answers yet, and nothing went wrong: grey.
    Quiet,
    /// Worth a look: amber.
    Notice,
    /// It answers no request: red, and its panel's edge turns red.
    Down,
}

impl Tone {
    /// The class that gives it its color.
    pub fn class(self) -> &'static str {
        match self {
            Self::Fine => "tone-fine",
            Self::Quiet => "tone-quiet",
            Self::Notice => "tone-notice",
            Self::Down => "tone-down",
        }
    }
}

/// What a link of a machine's line does, or the Machines dialog's button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusAction {
    /// Opens the update's screens, under the link's label.
    Update(UpdateLink),
    Retry,
    /// Opens the "No daemon answers" screen as a dialog, to choose how agents run on this
    /// computer.
    ChooseDaemon,
}

impl StatusAction {
    /// The link's text in the sidebar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Update(link) => link.label(),
            Self::Retry => "Retry now",
            Self::ChooseDaemon => "Choose how agents run here",
        }
    }

    /// The button's text in the Machines dialog, where every button has one width.
    pub fn button_label(self) -> &'static str {
        match self {
            Self::ChooseDaemon => "Choose",
            Self::Update(_) | Self::Retry => self.label(),
        }
    }
}

/// The line under a machine panel's head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub text: &'static str,
    /// An update under way, which the line shows in place of `text`, with a bar.
    pub progress: Option<LineProgress>,
    /// The error, as a tooltip.
    pub error: Option<String>,
    /// The links after the text, in order.
    pub actions: Vec<StatusAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineStatus {
    pub tone: Tone,
    /// The Machines dialog's chip.
    pub chip: &'static str,
    /// `None` while the machine answers and has nothing to show.
    pub line: Option<StatusLine>,
}

/// What the app says of `known`: an update to show comes before how it answers.
pub fn status(known: &MachineState) -> MachineStatus {
    let answers = connection_status(known);
    let Some(update) = daemon_update::update_line(known) else {
        return answers;
    };
    let mut actions = vec![StatusAction::Update(update.link)];
    if update.retry {
        actions.push(StatusAction::Retry);
    }
    MachineStatus {
        tone: if update.down {
            Tone::Down
        } else {
            Tone::Notice
        },
        chip: update_chip(known).unwrap_or(answers.chip),
        line: Some(StatusLine {
            text: update.text,
            progress: update.progress,
            error: None,
            actions,
        }),
    }
}

fn connection_status(known: &MachineState) -> MachineStatus {
    let status = |tone, chip, line| MachineStatus { tone, chip, line };
    let line = |text, error, actions| {
        Some(StatusLine {
            text,
            progress: None,
            error,
            actions,
        })
    };
    match &known.connection {
        ConnectionState::Connected => status(Tone::Fine, "Connected", None),
        ConnectionState::Connecting { .. } => status(
            Tone::Notice,
            "Connecting",
            line("Connecting…", None, Vec::new()),
        ),
        ConnectionState::Disconnected { error, .. } => status(
            Tone::Down,
            "Not connected",
            line(
                "Not connected.",
                Some(error.clone()),
                vec![StatusAction::Retry],
            ),
        ),
        ConnectionState::NoDaemon(screen) => match screen.absence {
            Absence::NothingRuns => status(
                Tone::Quiet,
                "No daemon",
                line("No daemon yet.", None, vec![StatusAction::ChooseDaemon]),
            ),
            Absence::Failed => status(
                Tone::Down,
                "Not answering",
                line(
                    "Its daemon does not answer.",
                    Some(screen.error.clone()),
                    vec![StatusAction::ChooseDaemon, StatusAction::Retry],
                ),
            ),
        },
    }
}

/// The chip of a machine whose daemon does not run the app's build, or whose update reads the
/// scheduled jobs first, waits, runs or failed.
fn update_chip(known: &MachineState) -> Option<&'static str> {
    if known.checking_jobs {
        return Some("Checking");
    }
    match (&known.update, &known.version_gap) {
        (Some(DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_)), _) => Some("Updating"),
        (Some(DaemonUpdate::Waiting), _) => Some("Waiting"),
        (Some(DaemonUpdate::Failed(_)), _) => Some("Update failed"),
        (None, Some(gap)) => Some(match gap.kind {
            GapKind::OlderProtocol => "Older version",
            GapKind::OtherBuild if gap.is_other_commit() => "Other build",
            GapKind::OtherBuild => "Update available",
            GapKind::NewerRelease | GapKind::NewerProtocol => "Newer version",
        }),
        (None, None) => None,
    }
}

/// A line of the strip at the top of a sidebar without machine panels: a machine and what its
/// panel would say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripLine {
    pub machine: MachineId,
    pub name: String,
    pub tone: Tone,
    pub line: StatusLine,
}

/// The strip's lines, this computer first. None while the sidebar groups by machine: each panel
/// says it there.
pub fn strip_lines(state: &AppState) -> Vec<StripLine> {
    if state.prefs.sidebar.group_by == GroupBy::Machine {
        return Vec::new();
    }
    state
        .machines
        .all()
        .filter_map(|(machine, known)| {
            let MachineStatus { tone, line, .. } = status(known);
            Some(StripLine {
                machine,
                name: MachineName::new(machine, known).name,
                tone,
                line: line?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use cockpit_protocol::{BuildCommit, PROTOCOL_VERSION};

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::daemon_probe::DaemonProbe;
    use crate::state::no_daemon;
    use crate::state::update_offer::{ProgramBuild, VersionGap};
    use crate::state::update_plan::{ServerPlan, UpdatePlan};

    fn down(error: &str) -> ConnectionState {
        ConnectionState::Disconnected {
            error: error.to_owned(),
            retry_at: Instant::now(),
        }
    }

    fn older() -> VersionGap {
        VersionGap {
            kind: GapKind::OlderProtocol,
            details: "the daemon speaks protocol 8, the client 9".to_owned(),
            daemon: None,
            protocol: PROTOCOL_VERSION - 1,
        }
    }

    fn lab_plan() -> UpdatePlan {
        UpdatePlan::Server(ServerPlan {
            host: "dev@lab".to_owned(),
            remote_command: "/opt/yetack".to_owned(),
            source_dir: None,
            commit: BuildCommit::new("3f9c2e1a7b4d5c6e".to_owned()),
            daemon_file: None,
        })
    }

    /// Local in the app, connected, and the servers devbox (connected), gpu-box (not connected)
    /// and lab (older), in that order.
    fn four_machines() -> AppState {
        let mut state = AppState::new(DaemonLabel::in_app());
        state.machines.this.connection = ConnectionState::Connected;
        let servers = [
            ("devbox", ConnectionState::Connected, None),
            ("gpu-box", down("ssh: connection refused"), None),
            (
                "lab",
                down("the daemon refused the connection"),
                Some(older()),
            ),
        ];
        for (id, (name, connection, gap)) in (1..).zip(servers) {
            let mut known = MachineState::new(DaemonLabel::ssh(name.to_owned()));
            known.connection = connection;
            known.update_plan = gap.is_some().then(lab_plan);
            known.daemon_probe = DaemonProbe::owned_at("/opt/yetack");
            known.version_gap = gap;
            state.machines.others.insert(MachineId::new(id), known);
        }
        state
    }

    fn line(state: &AppState, machine: u16) -> Option<StatusLine> {
        let known = state.machines.get(MachineId::new(machine))?;
        status(known).line
    }

    #[test]
    fn a_machine_that_answers_says_nothing_and_shows_a_green_chip() {
        let state = four_machines();

        let local = status(&state.machines.this);

        assert_eq!(
            local,
            MachineStatus {
                tone: Tone::Fine,
                chip: "Connected",
                line: None
            }
        );
    }

    #[test]
    fn each_trouble_says_what_to_do() {
        let state = four_machines();

        let not_connected = line(&state, 2);
        let older = line(&state, 3);

        assert_eq!(
            not_connected,
            Some(StatusLine {
                text: "Not connected.",
                progress: None,
                error: Some("ssh: connection refused".to_owned()),
                actions: vec![StatusAction::Retry],
            })
        );
        assert_eq!(
            older.map(|line| (line.text, line.actions)),
            Some((
                "Runs an older version.",
                vec![StatusAction::Update(UpdateLink::Update)]
            ))
        );
        let lab = status(&state.machines.others[&MachineId::new(3)]);
        assert_eq!((lab.tone, lab.chip), (Tone::Down, "Older version"));
    }

    #[test]
    fn this_computer_says_how_it_connects_and_asks_where_agents_run() {
        let mut state = AppState::new(DaemonLabel::service());
        let connecting = status(&state.machines.this);
        no_daemon::show(
            &mut state,
            &DaemonLabel::service(),
            "no socket".to_owned(),
            Absence::NothingRuns,
            None,
        );
        let nothing_runs = status(&state.machines.this);
        no_daemon::show(
            &mut state,
            &DaemonLabel::service(),
            "connection refused".to_owned(),
            Absence::Failed,
            None,
        );
        let failed = status(&state.machines.this);

        assert_eq!(
            (connecting.tone, connecting.line.map(|line| line.text)),
            (Tone::Notice, Some("Connecting…"))
        );
        assert_eq!(
            (
                nothing_runs.tone,
                nothing_runs.line.map(|line| line.actions)
            ),
            (Tone::Quiet, Some(vec![StatusAction::ChooseDaemon]))
        );
        assert_eq!(failed.tone, Tone::Down);
        assert_eq!(
            failed.line.map(|line| (line.error, line.actions)),
            Some((
                Some("connection refused".to_owned()),
                vec![StatusAction::ChooseDaemon, StatusAction::Retry]
            ))
        );
    }

    #[test]
    fn an_update_under_way_shows_before_the_refused_connection() {
        let mut state = four_machines();
        let lab = MachineId::new(3);

        daemon_update::start(&mut state, lab);
        let running = status(&state.machines.others[&lab]);

        assert_eq!((running.tone, running.chip), (Tone::Notice, "Updating"));
        assert_eq!(
            running.line.map(|line| line.actions),
            Some(vec![StatusAction::Update(UpdateLink::Show)])
        );
    }

    #[test]
    fn a_connected_machine_of_another_build_or_a_newer_one_names_it_on_its_chip() {
        let mut state = four_machines();
        let devbox = MachineId::new(1);
        let chip = |state: &AppState| {
            let shown = status(&state.machines.others[&devbox]);
            (shown.tone, shown.chip)
        };
        let known = state.machines.others.get_mut(&devbox).unwrap();
        known.update_plan = Some(lab_plan());
        known.version_gap = Some(VersionGap {
            kind: GapKind::OtherBuild,
            details: String::new(),
            daemon: None,
            protocol: PROTOCOL_VERSION,
        });
        let other_build = chip(&state);

        state.machines.others.get_mut(&devbox).unwrap().update = Some(DaemonUpdate::Waiting);
        let waiting = chip(&state);

        let known = state.machines.others.get_mut(&devbox).unwrap();
        known.update = None;
        known.version_gap = Some(VersionGap {
            kind: GapKind::NewerRelease,
            details: String::new(),
            daemon: None,
            protocol: PROTOCOL_VERSION,
        });
        let newer = chip(&state);

        assert_eq!(other_build, (Tone::Notice, "Update available"));
        assert_eq!(waiting, (Tone::Notice, "Waiting"));
        assert_eq!(newer, (Tone::Notice, "Newer version"));
    }

    #[test]
    fn a_daemon_of_this_release_from_another_commit_is_another_build_not_an_update() {
        let mut state = four_machines();
        let devbox = MachineId::new(1);
        let known = state.machines.others.get_mut(&devbox).unwrap();
        known.update_plan = Some(lab_plan());
        known.version_gap = Some(VersionGap {
            kind: GapKind::OtherBuild,
            details: String::new(),
            daemon: Some(ProgramBuild {
                version: ProgramBuild::this_program().version,
                commit: Some(BuildCommit::new(
                    "743d6640000000000000000000000000000000aa".to_owned(),
                )),
            }),
            protocol: PROTOCOL_VERSION,
        });

        let shown = status(&state.machines.others[&devbox]);

        assert_eq!((shown.tone, shown.chip), (Tone::Notice, "Other build"));
    }

    #[test]
    fn a_machine_in_use_that_lost_its_connection_says_so_before_its_version() {
        let mut state = four_machines();
        let devbox = MachineId::new(1);
        let known = state.machines.others.get_mut(&devbox).unwrap();
        known.update_plan = Some(lab_plan());
        known.connection = down("connection reset");
        let mut shown = Vec::new();
        for (kind, update) in [
            (GapKind::OtherBuild, None),
            (GapKind::NewerRelease, None),
            (GapKind::OtherBuild, Some(DaemonUpdate::Waiting)),
        ] {
            let known = state.machines.others.get_mut(&devbox).unwrap();
            known.version_gap = Some(VersionGap {
                kind,
                details: String::new(),
                daemon: None,
                protocol: PROTOCOL_VERSION,
            });
            known.update = update;
            let status = status(&state.machines.others[&devbox]);
            shown.push((status.tone, status.chip, status.line.map(|line| line.text)));
        }

        let not_connected = (Tone::Down, "Not connected", Some("Not connected."));
        assert_eq!(shown, [not_connected; 3]);
    }

    #[test]
    fn the_strip_shows_only_without_machine_panels_and_only_the_machines_in_trouble() {
        let mut state = four_machines();
        let by_machine = strip_lines(&state);
        state.prefs.sidebar.group_by = GroupBy::Project;

        let names: Vec<(String, &str)> = strip_lines(&state)
            .into_iter()
            .map(|strip| (strip.name, strip.line.text))
            .collect();

        assert_eq!(by_machine, []);
        assert_eq!(
            names,
            [
                ("gpu-box".to_owned(), "Not connected."),
                ("lab".to_owned(), "Runs an older version."),
            ]
        );
    }

    #[test]
    fn the_strip_names_this_computer_local_and_puts_it_first() {
        let mut state = four_machines();
        state.prefs.sidebar.group_by = GroupBy::None;
        state.machines.this.connection = down("the daemon stopped");

        let first = strip_lines(&state).into_iter().next();

        assert_eq!(
            first.map(|strip| (strip.machine, strip.name, strip.tone)),
            Some((MachineId::THIS, "Local".to_owned(), Tone::Down))
        );
    }
}

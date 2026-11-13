//! The "No daemon answers" screen: what it shows, and which daemon a choice on it runs.

use std::io;

use cockpit_client::{ConnectError, SessionError};

use super::app_state::{AppState, ConnectionState};
use crate::config::DaemonConfig;
use crate::daemon_label::{DaemonLabel, DaemonPlace};

/// A button of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonChoice {
    /// Tries the daemon the screen names again.
    Retry,
    /// Runs the agents in the app's own process.
    InApp,
    /// Starts `agents-cockpit serve` on this machine, which outlives the app.
    Service,
}

/// A choice to carry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chosen {
    pub choice: DaemonChoice,
    /// "Use this every time" was ticked: the choice becomes the config's daemon.
    pub every_time: bool,
}

/// Why no daemon answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Absence {
    /// No daemon runs on this machine: its socket is missing, nothing listens on it, or this
    /// system reaches no socket. The usual first start, which the screen meets with a question.
    NothingRuns,
    /// The daemon the app tried should have answered.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoDaemon {
    /// The daemon the app tried, as a sentence names it.
    pub tried: String,
    /// The daemon the app tried, at the start of a title.
    pub subject: String,
    pub error: String,
    pub absence: Absence,
    /// The tick of "Use this every time" once the user or a choice on the screen set it. Until
    /// then, the box follows the config ([`every_time`]).
    pub tick: Option<bool>,
    /// How many choices failed in a row on this screen, so that a click that fails like the last
    /// one still changes what it shows.
    pub failed_tries: u32,
    /// The choice being carried out, which the screen waits for.
    pub busy: Option<DaemonChoice>,
}

impl NoDaemon {
    /// Whether "Use this every time" is ticked, where the config names `configured`. Until the
    /// user sets it, the box is ticked only at a first start: nothing runs and the config names no
    /// daemon, so the choice is a setup, made once. A daemon the config names, "Ask" included,
    /// stays unless the user ticks the box.
    fn every_time(&self, configured: Option<&DaemonConfig>) -> bool {
        self.tick
            .unwrap_or(self.absence == Absence::NothingRuns && configured.is_none())
    }
}

/// Which daemon a choice runs, and the daemon the config keeps from then on when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoicePlan {
    pub run: DaemonConfig,
    pub save: Option<DaemonConfig>,
}

/// Whether this platform can start the background service, and reach a daemon's socket.
pub const SERVICE_OFFERED: bool = cfg!(unix);

/// Shows the screen: the app could not reach `tried`. `every_time` is the tick of the last choice
/// on the screen, which it keeps. Before any choice, the box follows the config ([`every_time`]).
pub fn show(
    state: &mut AppState,
    tried: &DaemonLabel,
    error: String,
    absence: Absence,
    every_time: Option<bool>,
) {
    let failed_tries = match &state.machines.this.connection {
        ConnectionState::NoDaemon(screen) if screen.busy.is_some() => {
            screen.failed_tries.saturating_add(1)
        }
        ConnectionState::NoDaemon(_)
        | ConnectionState::Connecting { .. }
        | ConnectionState::Connected
        | ConnectionState::Disconnected { .. } => 0,
    };
    state.machines.this.connection = ConnectionState::NoDaemon(NoDaemon {
        tried: tried.name(),
        subject: tried.subject(),
        error,
        absence,
        tick: every_time,
        failed_tries,
        busy: None,
    });
}

/// What the screen says after `failed_tries` failed choices, if any.
pub fn failure_note(failed_tries: u32) -> Option<String> {
    match failed_tries {
        0 => None,
        1 => Some("That did not work. The details are below.".to_owned()),
        tries => Some(format!(
            "That did not work, {tries} tries so far. The details are below."
        )),
    }
}

/// Whether `error`, from the first connection to a daemon at `place`, means that no daemon runs.
pub fn absence(place: DaemonPlace, error: &SessionError) -> Absence {
    let SessionError::Connect(ConnectError::Socket { source, .. }) = error else {
        return Absence::Failed;
    };
    let nothing_listens = matches!(
        source.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::Unsupported
    );
    if place == DaemonPlace::Socket && nothing_listens {
        Absence::NothingRuns
    } else {
        Absence::Failed
    }
}

/// Whether the screen's "Use this every time" is ticked. Nothing is ticked while no screen shows.
pub fn every_time(state: &AppState) -> bool {
    match &state.machines.this.connection {
        ConnectionState::NoDaemon(screen) => screen.every_time(state.machines.configured.as_ref()),
        ConnectionState::Connecting { .. }
        | ConnectionState::Connected
        | ConnectionState::Disconnected { .. } => false,
    }
}

pub fn toggle_every_time(state: &mut AppState) {
    let configured = state.machines.configured.as_ref();
    if let ConnectionState::NoDaemon(screen) = &mut state.machines.this.connection
        && screen.busy.is_none()
    {
        screen.tick = Some(!screen.every_time(configured));
    }
}

/// A click on one of the screen's buttons. Returns the choice to carry out, unless another one
/// is under way.
pub fn choose(state: &mut AppState, choice: DaemonChoice) -> Option<Chosen> {
    let service_offered = state.machines.service_offered;
    let configured = state.machines.configured.as_ref();
    let ConnectionState::NoDaemon(screen) = &mut state.machines.this.connection else {
        return None;
    };
    if screen.busy.is_some() || (choice == DaemonChoice::Service && !service_offered) {
        return None;
    }
    let every_time = screen.every_time(configured);
    // The box shows the tick of the choice under way, even when the config changes meanwhile.
    screen.tick = Some(every_time);
    screen.busy = Some(choice);
    Some(Chosen { choice, every_time })
}

/// The daemon a choice runs, after the app tried `tried`. Without "Use this every time" the config
/// stays, so the next start tries its daemon again. With it, the config takes the choice unless it
/// names that daemon already.
pub fn plan(chosen: Chosen, tried: &DaemonConfig, configured: Option<&DaemonConfig>) -> ChoicePlan {
    let run = match chosen.choice {
        DaemonChoice::Retry => tried.clone(),
        DaemonChoice::InApp => DaemonConfig::InApp,
        DaemonChoice::Service => DaemonConfig::Local {
            socket: None,
            start_service: true,
        },
    };
    let save = (chosen.every_time && configured != Some(&run)).then(|| run.clone());
    ChoicePlan { run, save }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::daemon_label::DaemonLabel;

    fn screen() -> AppState {
        let mut state = AppState::new(DaemonLabel::ssh("build-box".to_owned()));
        show(
            &mut state,
            &DaemonLabel::ssh("build-box".to_owned()),
            "ssh failed: Connection refused".to_owned(),
            Absence::Failed,
            None,
        );
        state
    }

    fn shown(state: &AppState) -> &NoDaemon {
        match &state.machines.this.connection {
            ConnectionState::NoDaemon(screen) => screen,
            other => panic!("expected the screen, got {other:?}"),
        }
    }

    #[test]
    fn each_failed_choice_changes_what_the_screen_says() {
        let label = DaemonLabel::ssh("build-box".to_owned());
        let again = |state: &mut AppState| {
            show(state, &label, "refused".to_owned(), Absence::Failed, None);
        };
        let mut state = screen();
        assert_eq!(failure_note(shown(&state).failed_tries), None);

        choose(&mut state, DaemonChoice::Retry);
        again(&mut state);
        let first = failure_note(shown(&state).failed_tries);
        choose(&mut state, DaemonChoice::Retry);
        again(&mut state);
        let second = failure_note(shown(&state).failed_tries);

        assert!(first.is_some());
        assert!(second.is_some());
        assert_ne!(first, second);
        again(&mut state);
        assert_eq!(shown(&state).failed_tries, 0, "no choice was under way");
    }

    fn ssh() -> DaemonConfig {
        DaemonConfig::Ssh {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
        }
    }

    #[test]
    fn a_choice_is_for_this_run_unless_ticked() {
        let mut state = screen();

        let chosen = choose(&mut state, DaemonChoice::InApp).unwrap();

        assert_eq!(
            chosen,
            Chosen {
                choice: DaemonChoice::InApp,
                every_time: false
            }
        );
        assert_eq!(
            plan(chosen, &ssh(), Some(&ssh())),
            ChoicePlan {
                run: DaemonConfig::InApp,
                save: None
            }
        );
    }

    #[test]
    fn a_ticked_choice_becomes_the_configs_daemon() {
        let mut state = screen();
        toggle_every_time(&mut state);

        let in_app = choose(&mut state, DaemonChoice::InApp).unwrap();
        let service = Chosen {
            choice: DaemonChoice::Service,
            every_time: true,
        };

        assert_eq!(
            plan(in_app, &ssh(), Some(&ssh())),
            ChoicePlan {
                run: DaemonConfig::InApp,
                save: Some(DaemonConfig::InApp)
            }
        );
        let started = DaemonConfig::Local {
            socket: None,
            start_service: true,
        };
        assert_eq!(
            plan(service, &ssh(), Some(&ssh())),
            ChoicePlan {
                run: started.clone(),
                save: Some(started)
            }
        );
    }

    #[test]
    fn retry_tries_the_daemon_tried_last() {
        let retry = Chosen {
            choice: DaemonChoice::Retry,
            every_time: false,
        };
        let every_time = Chosen {
            every_time: true,
            ..retry
        };
        let service = DaemonConfig::Local {
            socket: None,
            start_service: true,
        };

        assert_eq!(
            plan(every_time, &ssh(), Some(&ssh())),
            ChoicePlan {
                run: ssh(),
                save: None
            },
            "the config's daemon is the default already"
        );
        assert_eq!(
            plan(retry, &service, Some(&ssh())),
            ChoicePlan {
                run: service.clone(),
                save: None
            }
        );
        assert_eq!(
            plan(every_time, &service, Some(&ssh())),
            ChoicePlan {
                run: service.clone(),
                save: Some(service)
            }
        );
    }

    #[test]
    fn the_screen_takes_one_choice_at_a_time_and_keeps_its_tick() {
        let mut state = screen();
        toggle_every_time(&mut state);
        let chosen = choose(&mut state, DaemonChoice::Retry).unwrap();

        assert_eq!(choose(&mut state, DaemonChoice::InApp), None);
        toggle_every_time(&mut state);
        assert!(every_time(&state), "the tick waits with the choice");

        // The daemon answered the choice, then its first connection failed.
        state.machines.this.connection = ConnectionState::Connecting { attempt: 1 };
        show(
            &mut state,
            &DaemonLabel::in_app(),
            "cannot create the folder /state".to_owned(),
            Absence::Failed,
            Some(chosen.every_time),
        );

        let screen = shown(&state);
        assert_eq!(screen.busy, None);
        assert!(every_time(&state));
        assert_eq!(screen.tried, "the daemon in this app");
    }

    fn socket_error(kind: io::ErrorKind) -> SessionError {
        SessionError::Connect(ConnectError::Socket {
            path: "/run/agents-cockpit.sock".into(),
            source: io::Error::from(kind),
        })
    }

    #[test]
    fn a_missing_or_silent_socket_on_this_machine_means_nothing_runs() {
        let on_socket = |kind| absence(DaemonPlace::Socket, &socket_error(kind));

        assert_eq!(on_socket(io::ErrorKind::NotFound), Absence::NothingRuns);
        assert_eq!(
            on_socket(io::ErrorKind::ConnectionRefused),
            Absence::NothingRuns
        );
        assert_eq!(on_socket(io::ErrorKind::Unsupported), Absence::NothingRuns);
        assert_eq!(on_socket(io::ErrorKind::PermissionDenied), Absence::Failed);
        assert_eq!(
            absence(DaemonPlace::Service, &socket_error(io::ErrorKind::NotFound)),
            Absence::Failed,
            "the service the app started should answer"
        );
        assert_eq!(
            absence(
                DaemonPlace::Ssh,
                &SessionError::Connect(ConnectError::Ssh {
                    stderr: "Connection timed out".to_owned()
                })
            ),
            Absence::Failed
        );
    }

    #[test]
    fn the_tick_starts_on_at_a_first_start_and_then_keeps_the_users() {
        let mut state = AppState::new(DaemonLabel::socket());
        let tick = |state: &mut AppState, absence, kept| {
            show(state, &DaemonLabel::socket(), String::new(), absence, kept);
            every_time(state)
        };

        assert!(tick(&mut state, Absence::NothingRuns, None));
        assert!(!tick(&mut state, Absence::Failed, None));
        assert!(!tick(&mut state, Absence::NothingRuns, Some(false)));
    }

    #[test]
    fn the_box_starts_ticked_only_when_the_config_names_no_daemon() {
        let ticked = |config: &str, absence| {
            let mut state = AppState::new(DaemonLabel::socket());
            state.machines.configured = config.parse::<Config>().unwrap().daemon;
            show(
                &mut state,
                &DaemonLabel::socket(),
                String::new(),
                absence,
                None,
            );
            choose(&mut state, DaemonChoice::InApp).map(|chosen| chosen.every_time)
        };

        assert_eq!(ticked("", Absence::NothingRuns), Some(true));
        assert_eq!(
            ticked("[notifications]\nenabled = false\n", Absence::NothingRuns),
            Some(true)
        );
        assert_eq!(
            ticked("[daemon]\ntransport = \"local\"\n", Absence::NothingRuns),
            Some(false),
            "a config that asks keeps asking"
        );
        assert_eq!(
            ticked("[daemon]\ntransport = \"in_app\"\n", Absence::NothingRuns),
            Some(false)
        );
        assert_eq!(ticked("", Absence::Failed), Some(false));
    }

    #[test]
    fn a_choice_under_way_keeps_its_tick_when_the_config_takes_another() {
        let mut state = AppState::new(DaemonLabel::socket());
        show(
            &mut state,
            &DaemonLabel::socket(),
            String::new(),
            Absence::NothingRuns,
            None,
        );

        let chosen = choose(&mut state, DaemonChoice::InApp);
        state.machines.configured = Some(DaemonConfig::default());

        assert_eq!(chosen.map(|chosen| chosen.every_time), Some(true));
        assert!(every_time(&state));
    }

    #[test]
    fn the_service_is_chosen_only_where_it_runs() {
        let mut state = screen();
        state.machines.service_offered = false;

        let refused = choose(&mut state, DaemonChoice::Service);
        state.machines.service_offered = true;
        let chosen = choose(&mut state, DaemonChoice::Service);

        assert_eq!(refused, None);
        assert_eq!(
            chosen.map(|chosen| chosen.choice),
            Some(DaemonChoice::Service)
        );
    }

    #[test]
    fn a_connected_app_has_no_screen_to_choose_on() {
        let mut state = AppState::new(DaemonLabel::in_app());
        state.machines.this.connection = ConnectionState::Connected;

        assert_eq!(choose(&mut state, DaemonChoice::InApp), None);
    }
}

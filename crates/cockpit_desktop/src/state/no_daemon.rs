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
    /// "Use this every time" is ticked.
    pub every_time: bool,
    /// How many choices failed in a row on this screen, so that a click that fails like the last
    /// one still changes what it shows.
    pub failed_tries: u32,
    /// The choice being carried out, which the screen waits for.
    pub busy: Option<DaemonChoice>,
}

/// Which daemon a choice runs, and the daemon the config keeps from then on when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoicePlan {
    pub run: DaemonConfig,
    pub save: Option<DaemonConfig>,
}

/// Whether this platform can start the background service.
pub const SERVICE_OFFERED: bool = cfg!(unix);

/// Whether this platform reaches a daemon's socket, so that looking for one again can help.
pub const SOCKET_REACHABLE: bool = cfg!(unix);

/// Shows the screen: the app could not reach `tried`. `every_time` is the tick of the last choice
/// on the screen, which it keeps. Before any choice, the tick is on when nothing runs: the choice
/// is then a setup, made once.
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
        every_time: every_time.unwrap_or(absence == Absence::NothingRuns),
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

pub fn toggle_every_time(state: &mut AppState) {
    if let ConnectionState::NoDaemon(screen) = &mut state.machines.this.connection
        && screen.busy.is_none()
    {
        screen.every_time = !screen.every_time;
    }
}

/// A click on one of the screen's buttons. Returns the choice to carry out, unless another one
/// is under way.
pub fn choose(state: &mut AppState, choice: DaemonChoice) -> Option<Chosen> {
    let ConnectionState::NoDaemon(screen) = &mut state.machines.this.connection else {
        return None;
    };
    if screen.busy.is_some() || (choice == DaemonChoice::Service && !SERVICE_OFFERED) {
        return None;
    }
    screen.busy = Some(choice);
    Some(Chosen {
        choice,
        every_time: screen.every_time,
    })
}

/// The daemon a choice runs, after the app tried `tried`. Without "Use this every time" the config
/// stays, so the next start tries its daemon again.
pub fn plan(chosen: Chosen, tried: &DaemonConfig, configured: &DaemonConfig) -> ChoicePlan {
    let run = match chosen.choice {
        DaemonChoice::Retry => tried.clone(),
        DaemonChoice::InApp => DaemonConfig::InApp,
        DaemonChoice::Service => DaemonConfig::Local {
            socket: None,
            start_service: true,
        },
    };
    let save = (chosen.every_time && run != *configured).then(|| run.clone());
    ChoicePlan { run, save }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            plan(chosen, &ssh(), &ssh()),
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
            plan(in_app, &ssh(), &ssh()),
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
            plan(service, &ssh(), &ssh()),
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
            plan(every_time, &ssh(), &ssh()),
            ChoicePlan {
                run: ssh(),
                save: None
            },
            "the config's daemon is the default already"
        );
        assert_eq!(
            plan(retry, &service, &ssh()),
            ChoicePlan {
                run: service.clone(),
                save: None
            }
        );
        assert_eq!(
            plan(every_time, &service, &ssh()),
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
        assert!(shown(&state).every_time, "the tick waits with the choice");

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
        assert!(screen.every_time);
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
    fn the_tick_starts_on_when_nothing_runs_and_then_keeps_the_users() {
        let mut state = AppState::new(DaemonLabel::socket());
        let tick = |state: &mut AppState, absence, every_time| {
            show(
                state,
                &DaemonLabel::socket(),
                String::new(),
                absence,
                every_time,
            );
            shown(state).every_time
        };

        assert!(tick(&mut state, Absence::NothingRuns, None));
        assert!(!tick(&mut state, Absence::Failed, None));
        assert!(!tick(&mut state, Absence::NothingRuns, Some(false)));
    }

    #[test]
    fn a_connected_app_has_no_screen_to_choose_on() {
        let mut state = AppState::new(DaemonLabel::in_app());
        state.machines.this.connection = ConnectionState::Connected;

        assert_eq!(choose(&mut state, DaemonChoice::InApp), None);
    }
}

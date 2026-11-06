//! The "No daemon answers" screen: what it shows, and which daemon a choice on it runs.

use super::app_state::{AppState, ConnectionState};
use crate::config::DaemonConfig;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoDaemon {
    /// The daemon the app tried, as the screen names it.
    pub tried: String,
    pub error: String,
    /// "Use this every time" is ticked.
    pub every_time: bool,
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

/// Shows the screen: the app could not reach `tried`. `every_time` is the tick of the last choice
/// on the screen, which it keeps.
pub fn show(state: &mut AppState, tried: String, error: String, every_time: bool) {
    state.connection = ConnectionState::NoDaemon(NoDaemon {
        tried,
        error,
        every_time,
        busy: None,
    });
}

pub fn toggle_every_time(state: &mut AppState) {
    if let ConnectionState::NoDaemon(screen) = &mut state.connection
        && screen.busy.is_none()
    {
        screen.every_time = !screen.every_time;
    }
}

/// A click on one of the screen's buttons. Returns the choice to carry out, unless another one
/// is under way.
pub fn choose(state: &mut AppState, choice: DaemonChoice) -> Option<Chosen> {
    let ConnectionState::NoDaemon(screen) = &mut state.connection else {
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
            "build-box, over SSH".to_owned(),
            "ssh failed: Connection refused".to_owned(),
            false,
        );
        state
    }

    fn shown(state: &AppState) -> &NoDaemon {
        match &state.connection {
            ConnectionState::NoDaemon(screen) => screen,
            other => panic!("expected the screen, got {other:?}"),
        }
    }

    fn ssh() -> DaemonConfig {
        DaemonConfig::Ssh {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
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
        state.connection = ConnectionState::Connecting { attempt: 1 };
        show(
            &mut state,
            "the daemon in this app".to_owned(),
            "cannot create the folder /state".to_owned(),
            chosen.every_time,
        );

        let screen = shown(&state);
        assert_eq!(screen.busy, None);
        assert!(screen.every_time);
        assert_eq!(screen.tried, "the daemon in this app");
    }

    #[test]
    fn a_connected_app_has_no_screen_to_choose_on() {
        let mut state = AppState::new(DaemonLabel::in_app());
        state.connection = ConnectionState::Connected;

        assert_eq!(choose(&mut state, DaemonChoice::InApp), None);
    }
}

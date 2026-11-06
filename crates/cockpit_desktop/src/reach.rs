//! Turns the daemon the config or the "No daemon answers" screen names into a transport: it
//! starts the daemon in the app, or the background service, when that is the daemon.

use std::path::{Path, PathBuf};

use cockpit_client::Transport;
use cockpit_daemon::{ServeError, StoreError};

use crate::config::{ConfigError, DaemonConfig, Environment, save_daemon};
use crate::daemon_label::DaemonLabel;
use crate::in_app::InAppHost;
use crate::prefs::PREFS_FOLDER;
use crate::state::error_chain;
use crate::state::no_daemon::{self, Chosen};

/// Says why the app reaches another daemon than the one in the app.
const ANOTHER_DAEMON_RUNS: &str = "Another daemon already runs on this machine, such as the background service: the app connects to it instead of running agents itself.";

/// Where the app finds, on this machine, what it starts or writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    pub config_file: PathBuf,
    /// The state folder of a daemon on this machine, which holds its socket and its lock.
    pub daemon_state: PathBuf,
    /// The app's own state folder, which holds the background service's log.
    pub app_state: PathBuf,
}

/// A daemon to connect to.
#[derive(Debug)]
pub struct Reached {
    pub transport: Transport,
    pub label: DaemonLabel,
    /// What the user should know about the daemon reached.
    pub note: Option<&'static str>,
}

/// A daemon that could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreached {
    pub label: DaemonLabel,
    pub error: String,
}

impl Places {
    pub fn new(environment: &Environment) -> Result<Self, ConfigError> {
        let daemon_state = environment.daemon_state_dir()?;
        let app_state = daemon_state.with_file_name(PREFS_FOLDER);
        Ok(Self {
            config_file: environment.config_file()?,
            daemon_state,
            app_state,
        })
    }

    fn socket(&self, configured: Option<&Path>) -> PathBuf {
        configured.map_or_else(
            || cockpit_daemon::socket_path(&self.daemon_state),
            Path::to_owned,
        )
    }
}

/// How the app names `daemon` before it reaches it.
pub fn label(daemon: &DaemonConfig) -> DaemonLabel {
    match daemon {
        DaemonConfig::Local {
            start_service: false,
            ..
        } => DaemonLabel::socket(),
        DaemonConfig::Local {
            start_service: true,
            ..
        } => DaemonLabel::service(),
        DaemonConfig::Ssh { host, .. } => DaemonLabel::ssh(host.clone()),
        DaemonConfig::InApp => DaemonLabel::in_app(),
    }
}

/// Gets `daemon` ready for a connection.
pub async fn reach(
    daemon: &DaemonConfig,
    places: &Places,
    host: &InAppHost,
) -> Result<Reached, Unreached> {
    let label = label(daemon);
    match daemon {
        DaemonConfig::Local {
            socket,
            start_service: false,
        } => Ok(Reached {
            transport: Transport::Local {
                socket: places.socket(socket.as_deref()),
            },
            label,
            note: None,
        }),
        DaemonConfig::Local {
            socket,
            start_service: true,
        } => start_service(socket.as_deref(), places, label).await,
        DaemonConfig::Ssh {
            host,
            remote_command,
        } => Ok(Reached {
            transport: Transport::Ssh {
                host: host.clone(),
                remote_command: remote_command.clone(),
            },
            label,
            note: None,
        }),
        DaemonConfig::InApp => run_in_app(places, host, label).await,
    }
}

async fn run_in_app(
    places: &Places,
    host: &InAppHost,
    label: DaemonLabel,
) -> Result<Reached, Unreached> {
    let Some(config) = cockpit_daemon::DaemonConfig::from_env(Some(places.daemon_state.clone()))
    else {
        return Err(Unreached {
            label,
            error: format!("{} is not set", cockpit_daemon::HOME_ENV),
        });
    };
    match host.start(config).await {
        Ok(link) => Ok(Reached {
            transport: Transport::InApp(link),
            label,
            note: None,
        }),
        Err(ServeError::Store(StoreError::AlreadyRunning { .. })) => Ok(Reached {
            transport: Transport::Local {
                socket: places.socket(None),
            },
            label: DaemonLabel::socket(),
            note: Some(ANOTHER_DAEMON_RUNS),
        }),
        Err(error) => Err(Unreached {
            label,
            error: error_chain(&error),
        }),
    }
}

/// The service serves the state folder of `socket`, or the default one.
#[cfg(unix)]
async fn start_service(
    socket: Option<&Path>,
    places: &Places,
    label: DaemonLabel,
) -> Result<Reached, Unreached> {
    let state_dir = match socket.and_then(Path::parent) {
        Some(folder) => folder.to_owned(),
        None => places.daemon_state.clone(),
    };
    match crate::service::ensure(&state_dir, &places.app_state).await {
        Ok(socket) => Ok(Reached {
            transport: Transport::Local { socket },
            label,
            note: None,
        }),
        Err(error) => Err(Unreached {
            label,
            error: error_chain(&error),
        }),
    }
}

#[cfg(not(unix))]
async fn start_service(
    _socket: Option<&Path>,
    _places: &Places,
    label: DaemonLabel,
) -> Result<Reached, Unreached> {
    Err(Unreached {
        label,
        error: "the background service runs on Unix only".to_owned(),
    })
}

/// The daemon a choice on the screen runs, after the app tried `tried`. With "Use this every
/// time", the choice becomes the config's daemon, `configured` included. Also returns the error
/// to show when the config cannot be written: the choice still holds for this run.
pub fn settle(
    chosen: Chosen,
    tried: &DaemonConfig,
    configured: &mut DaemonConfig,
    config_file: &Path,
) -> (DaemonConfig, Option<String>) {
    let plan = no_daemon::plan(chosen, tried, configured);
    let Some(save) = plan.save else {
        return (plan.run, None);
    };
    match save_daemon(config_file, &save) {
        Ok(()) => {
            *configured = save;
            (plan.run, None)
        }
        Err(error) => (plan.run, Some(error_chain(&error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::no_daemon::DaemonChoice;

    fn places(root: &Path) -> Places {
        Places {
            config_file: root.join("config/agents-cockpit/config.toml"),
            daemon_state: root.join("state/agents-cockpit"),
            app_state: root.join("state/agents-cockpit-desktop"),
        }
    }

    fn ssh() -> DaemonConfig {
        DaemonConfig::Ssh {
            host: "build-box".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
        }
    }

    #[test]
    fn places_follow_the_environment() {
        let environment = Environment {
            home: Some(PathBuf::from("/home/someone")),
            config_home: None,
            state_home: None,
        };

        let places = Places::new(&environment).unwrap();

        assert_eq!(
            places,
            Places {
                config_file: PathBuf::from("/home/someone/.config/agents-cockpit/config.toml"),
                daemon_state: PathBuf::from("/home/someone/.local/state/agents-cockpit"),
                app_state: PathBuf::from("/home/someone/.local/state/agents-cockpit-desktop"),
            }
        );
        assert_eq!(
            places.socket(None),
            PathBuf::from("/home/someone/.local/state/agents-cockpit/agents-cockpit.sock")
        );
    }

    #[tokio::test]
    async fn a_socket_or_ssh_daemon_is_reached_as_the_config_says() {
        let places = places(Path::new("/srv"));
        let host = InAppHost::default();

        let local = reach(&DaemonConfig::default(), &places, &host)
            .await
            .unwrap();
        let remote = reach(&ssh(), &places, &host).await.unwrap();

        assert_eq!(
            local.transport,
            Transport::Local {
                socket: PathBuf::from("/srv/state/agents-cockpit/agents-cockpit.sock")
            }
        );
        assert_eq!(local.label.name(), "the daemon on this machine");
        assert_eq!(
            remote.transport,
            Transport::Ssh {
                host: "build-box".to_owned(),
                remote_command: "/opt/agents-cockpit".to_owned()
            }
        );
        assert_eq!(remote.label.name(), "build-box, over SSH");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_app_runs_one_daemon_and_connects_to_another_that_holds_the_lock() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path());
        let first = InAppHost::default();
        let second = InAppHost::default();

        let in_app = reach(&DaemonConfig::InApp, &places, &first).await.unwrap();
        let again = reach(&DaemonConfig::InApp, &places, &first).await.unwrap();
        let other = reach(&DaemonConfig::InApp, &places, &second).await.unwrap();

        assert!(matches!(in_app.transport, Transport::InApp(_)));
        assert_eq!(in_app.label, DaemonLabel::in_app());
        assert_eq!(again.transport, in_app.transport, "the app starts it once");
        assert_eq!(
            other.transport,
            Transport::Local {
                socket: cockpit_daemon::socket_path(&places.daemon_state)
            }
        );
        assert_eq!(other.note, Some(ANOTHER_DAEMON_RUNS));
        first.shutdown();
        let after = reach(&DaemonConfig::InApp, &places, &second).await.unwrap();
        assert!(matches!(after.transport, Transport::InApp(_)));
        second.shutdown();
    }

    #[test]
    fn a_choice_for_this_run_leaves_the_config_alone() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path());
        let mut configured = ssh();
        let chosen = Chosen {
            choice: DaemonChoice::InApp,
            every_time: false,
        };

        let (run, error) = settle(chosen, &ssh(), &mut configured, &places.config_file);

        assert_eq!((run, error), (DaemonConfig::InApp, None));
        assert_eq!(configured, ssh());
        assert!(!places.config_file.exists());
    }

    #[test]
    fn use_this_every_time_writes_the_config() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path());
        let mut configured = ssh();
        let chosen = Chosen {
            choice: DaemonChoice::InApp,
            every_time: true,
        };

        let (run, error) = settle(chosen, &ssh(), &mut configured, &places.config_file);

        assert_eq!((run, error), (DaemonConfig::InApp, None));
        assert_eq!(configured, DaemonConfig::InApp);
        let saved = std::fs::read_to_string(&places.config_file).unwrap();
        assert_eq!(saved, "[daemon]\ntransport = \"in_app\"\n");
    }

    #[test]
    fn a_config_that_cannot_be_written_leaves_the_choice_for_this_run() {
        let temp = tempfile::tempdir().unwrap();
        let config_file = temp.path().join("config.toml");
        std::fs::write(&config_file, "this is not toml = = =").unwrap();
        let mut configured = ssh();
        let chosen = Chosen {
            choice: DaemonChoice::InApp,
            every_time: true,
        };

        let (run, error) = settle(chosen, &ssh(), &mut configured, &config_file);

        assert_eq!(run, DaemonConfig::InApp);
        assert_eq!(configured, ssh());
        assert!(error.unwrap().contains("is not a valid TOML file"));
    }
}

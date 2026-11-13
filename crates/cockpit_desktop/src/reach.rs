//! Turns the daemon the config or the "No daemon answers" screen names into a transport: it
//! starts the daemon in the app, or the background service, when that is the daemon.

use std::path::{Path, PathBuf};

use cockpit_client::Transport;
use cockpit_daemon::{ServeError, StoreError};

use crate::config::{ConfigError, DaemonConfig, Environment, save_daemon};
use crate::daemon_label::DaemonLabel;
use crate::in_app_host::InAppHost;
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
            ..
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
        Err(error @ ServeError::Store(StoreError::AlreadyRunning { .. })) => {
            let socket = places.socket(None);
            if !answers(&socket).await {
                return Err(Unreached {
                    label,
                    error: format!(
                        "{}, and nothing answers on {}: another copy of this app may run the agents",
                        error_chain(&error),
                        socket.display()
                    ),
                });
            }
            Ok(Reached {
                transport: Transport::Local { socket },
                label: DaemonLabel::socket(),
                note: Some(ANOTHER_DAEMON_RUNS),
            })
        }
        Err(error) => Err(Unreached {
            label,
            error: error_chain(&error),
        }),
    }
}

#[cfg(unix)]
async fn answers(socket: &Path) -> bool {
    crate::service::answers(socket).await
}

/// No daemon listens on a socket on this platform.
#[cfg(not(unix))]
async fn answers(_socket: &Path) -> bool {
    false
}

/// The state folder of the service that listens on `socket`, or on the default socket. A service
/// listens in its folder on a socket of its own name, so a `socket` named otherwise has none.
pub fn service_state_dir(socket: Option<&Path>, places: &Places) -> Option<PathBuf> {
    let Some(socket) = socket else {
        return Some(places.daemon_state.clone());
    };
    let folder = socket.parent()?;
    (socket.is_absolute() && cockpit_daemon::socket_path(folder) == socket)
        .then(|| folder.to_owned())
}

/// The service serves the state folder of `socket`, or the default one.
#[cfg(unix)]
async fn start_service(
    socket: Option<&Path>,
    places: &Places,
    label: DaemonLabel,
) -> Result<Reached, Unreached> {
    let Some(state_dir) = service_state_dir(socket, places) else {
        let named = socket.map_or_else(String::new, |socket| socket.display().to_string());
        return Err(Unreached {
            label,
            error: format!(
                "the app starts the service only for a socket such as {}, in the service's state folder: the config names {named}",
                places.socket(None).display(),
            ),
        });
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
        error: crate::service_update::UNIX_ONLY.to_owned(),
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
            source_dir: None,
        }
    }

    #[test]
    fn places_follow_the_environment() {
        #[cfg(unix)]
        let (home, config, state) = ("/home/someone", ".config", ".local/state");
        #[cfg(windows)]
        let (home, config, state) = (r"C:\Users\someone", r"AppData\Roaming", r"AppData\Local");
        let home = PathBuf::from(home);
        let environment = Environment {
            home: Some(home.clone()),
            config_home: None,
            state_home: None,
        };

        let places = Places::new(&environment).unwrap();

        let state = home.join(state);
        assert_eq!(
            places,
            Places {
                config_file: home.join(config).join("agents-cockpit").join("config.toml"),
                daemon_state: state.join("agents-cockpit"),
                app_state: state.join("agents-cockpit-desktop"),
            }
        );
        assert_eq!(
            places.socket(None),
            state.join("agents-cockpit").join("agents-cockpit.sock")
        );
    }

    #[test]
    fn a_service_serves_the_folder_its_socket_is_named_after() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path());
        let other = temp.path().join("other-state");
        let in_its_folder = cockpit_daemon::socket_path(&other);
        let named_otherwise = other.join("cockpit.sock");
        let relative = Path::new("other-state").join("agents-cockpit.sock");

        assert_eq!(
            service_state_dir(None, &places),
            Some(places.daemon_state.clone())
        );
        assert_eq!(
            service_state_dir(Some(&in_its_folder), &places),
            Some(other)
        );
        assert_eq!(service_state_dir(Some(&named_otherwise), &places), None);
        assert_eq!(service_state_dir(Some(&relative), &places), None);
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
        #[cfg(unix)]
        use tokio::net::UnixListener;

        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path());
        let first = InAppHost::default();
        let second = InAppHost::default();

        let in_app = reach(&DaemonConfig::InApp, &places, &first).await.unwrap();
        let again = reach(&DaemonConfig::InApp, &places, &first).await.unwrap();
        #[cfg(unix)]
        let listener = UnixListener::bind(places.socket(None)).unwrap();
        let other = reach(&DaemonConfig::InApp, &places, &second).await;

        assert!(matches!(in_app.transport, Transport::InApp(_)));
        assert_eq!(in_app.label, DaemonLabel::in_app());
        assert_eq!(again.transport, in_app.transport, "the app starts it once");
        #[cfg(unix)]
        {
            let other = other.unwrap();
            assert_eq!(
                other.transport,
                Transport::Local {
                    socket: cockpit_daemon::socket_path(&places.daemon_state)
                }
            );
            assert_eq!(other.note, Some(ANOTHER_DAEMON_RUNS));
            drop(listener);
        }
        #[cfg(not(unix))]
        assert!(other.is_err());
        first.shutdown();
        let after = reach(&DaemonConfig::InApp, &places, &second).await.unwrap();
        assert!(matches!(after.transport, Transport::InApp(_)));
        second.shutdown();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_service_starts_only_for_a_socket_it_can_listen_on() {
        let places = places(Path::new("/srv"));
        let named_otherwise = DaemonConfig::Local {
            socket: Some(PathBuf::from("/run/cockpit.sock")),
            start_service: true,
        };

        let refused = reach(&named_otherwise, &places, &InAppHost::default()).await;

        let Err(Unreached { label, error }) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(label, DaemonLabel::service());
        assert_eq!(
            error,
            "the app starts the service only for a socket such as /srv/state/agents-cockpit/agents-cockpit.sock, in the service's state folder: the config names /run/cockpit.sock"
        );
    }

    /// Another copy of the app holds the lock, and has no socket.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lock_held_without_a_socket_names_the_lock() {
        let temp = tempfile::tempdir().unwrap();
        let places = places(temp.path());
        let first = InAppHost::default();
        reach(&DaemonConfig::InApp, &places, &first).await.unwrap();

        let second = reach(&DaemonConfig::InApp, &places, &InAppHost::default()).await;

        let Err(Unreached { label, error }) = second else {
            panic!("expected no daemon, got {second:?}");
        };
        assert_eq!(label, DaemonLabel::in_app());
        let lock = places.daemon_state.join("serve.lock");
        assert!(
            error.starts_with(&format!(
                "another daemon already runs on {}",
                lock.display()
            )),
            "{error}"
        );
        assert!(
            error.ends_with("another copy of this app may run the agents"),
            "{error}"
        );
        first.shutdown();
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

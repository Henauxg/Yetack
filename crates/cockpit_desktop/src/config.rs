//! The app's config file, `$XDG_CONFIG_HOME/agents-cockpit/config.toml`: which daemon to reach.
//!
//! ```toml
//! [daemon]
//! transport = "ssh"
//! host = "build-box"
//! remote_command = "/home/me/.cargo/bin/agents-cockpit"
//!
//! [notifications]
//! enabled = true
//! ```
//!
//! The daemon may also be `transport = "local"`, with an optional `socket` and `start_service`,
//! or `transport = "in_app"`, the daemon in the app's own process. A missing file means the local
//! daemon, at its default socket.

use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

const APP_FOLDER: &str = "agents-cockpit";
const CONFIG_FILE: &str = "config.toml";
const DAEMON_SECTION: &str = "daemon";
/// Where a new config file is written before it replaces the old one.
const TEMPORARY_EXTENSION: &str = "toml.new";

const HOME_VAR: &str = "HOME";
const CONFIG_HOME_VAR: &str = "XDG_CONFIG_HOME";
const STATE_HOME_VAR: &str = "XDG_STATE_HOME";

/// The XDG defaults, relative to the home folder.
const DEFAULT_CONFIG_HOME: &str = ".config";
const DEFAULT_STATE_HOME: &str = ".local/state";

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub notifications: NotificationsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum DaemonConfig {
    /// `socket` defaults to `$XDG_STATE_HOME/agents-cockpit/agents-cockpit.sock`. With
    /// `start_service`, the app starts `agents-cockpit serve` when the socket does not answer.
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        socket: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "is_false")]
        start_service: bool,
    },
    /// `remote_command` is the absolute path of `agents-cockpit` on the host.
    Ssh {
        host: String,
        remote_command: String,
    },
    /// The daemon runs in the app: its agents stop when the app closes, and resume the next
    /// time the app runs them.
    InApp,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self::Local {
            socket: None,
            start_service: false,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

/// The `[daemon]` section alone, to write it.
#[derive(Serialize)]
struct DaemonSection<'config> {
    daemon: &'config DaemonConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    pub enabled: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a valid config", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{} is not a valid TOML file", path.display())]
    Edit {
        path: PathBuf,
        #[source]
        source: toml_edit::TomlError,
    },
    #[error("cannot encode the daemon's config")]
    Encode(#[source] toml::ser::Error),
    #[error("cannot write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot find the {what}: neither {var} nor {HOME_VAR} is an absolute path")]
    NoHome {
        what: &'static str,
        var: &'static str,
    },
}

/// The environment variables the paths come from. XDG ignores an empty or relative value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    pub home: Option<PathBuf>,
    pub config_home: Option<PathBuf>,
    pub state_home: Option<PathBuf>,
}

impl Environment {
    pub fn from_process() -> Self {
        let absolute = |var: &str| -> Option<PathBuf> {
            let path = PathBuf::from(std::env::var_os(var)?);
            path.is_absolute().then_some(path)
        };
        Self {
            home: absolute(HOME_VAR),
            config_home: absolute(CONFIG_HOME_VAR),
            state_home: absolute(STATE_HOME_VAR),
        }
    }

    pub fn config_file(&self) -> Result<PathBuf, ConfigError> {
        let base = xdg_base(
            self.config_home.as_deref(),
            self.home.as_deref(),
            DEFAULT_CONFIG_HOME,
        )
        .ok_or(ConfigError::NoHome {
            what: "config file",
            var: CONFIG_HOME_VAR,
        })?;
        Ok(base.join(APP_FOLDER).join(CONFIG_FILE))
    }

    /// The state folder of a daemon on this machine, the background service's or the app's own,
    /// as the daemon computes it.
    pub fn daemon_state_dir(&self) -> Result<PathBuf, ConfigError> {
        let base = self.state_base().ok_or(ConfigError::NoHome {
            what: "daemon's state folder",
            var: STATE_HOME_VAR,
        })?;
        Ok(base.join(APP_FOLDER))
    }

    /// `$XDG_STATE_HOME`, or its default under the home folder.
    pub fn state_base(&self) -> Option<PathBuf> {
        xdg_base(
            self.state_home.as_deref(),
            self.home.as_deref(),
            DEFAULT_STATE_HOME,
        )
    }
}

fn xdg_base(set: Option<&Path>, home: Option<&Path>, default_under_home: &str) -> Option<PathBuf> {
    match set {
        Some(path) => Some(path.to_owned()),
        None => home.map(|home| home.join(default_under_home)),
    }
}

impl Config {
    pub fn load(environment: &Environment) -> Result<Self, ConfigError> {
        let path = environment.config_file()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        text.parse::<Self>()
            .map_err(|source| ConfigError::Parse { path, source })
    }
}

/// Makes `daemon` the daemon of the config file `file`, which keeps its other settings and its
/// comments. A missing file is created.
pub fn save_daemon(file: &Path, daemon: &DaemonConfig) -> Result<(), ConfigError> {
    use toml_edit::{DocumentMut, Item};

    let edit_error = |source| ConfigError::Edit {
        path: file.to_owned(),
        source,
    };
    let write_error = |source| ConfigError::Write {
        path: file.to_owned(),
        source,
    };
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigError::Read {
                path: file.to_owned(),
                source,
            });
        }
    };
    let mut document: DocumentMut = text.parse().map_err(edit_error)?;
    let section = toml::to_string(&DaemonSection { daemon }).map_err(ConfigError::Encode)?;
    let mut section: DocumentMut = section.parse().map_err(edit_error)?;
    if let Some(mut daemon) = section.remove(DAEMON_SECTION) {
        // The comments above the old section's header.
        let decor = document
            .get(DAEMON_SECTION)
            .and_then(Item::as_table)
            .map(|table| table.decor().clone());
        if let (Some(decor), Some(table)) = (decor, daemon.as_table_mut()) {
            *table.decor_mut() = decor;
        }
        document.insert(DAEMON_SECTION, daemon);
    }
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).map_err(write_error)?;
    }
    let temporary = file.with_extension(TEMPORARY_EXTENSION);
    std::fs::write(&temporary, document.to_string()).map_err(write_error)?;
    std::fs::rename(&temporary, file).map_err(write_error)
}

impl FromStr for Config {
    type Err = toml::de::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> Environment {
        Environment {
            home: Some(PathBuf::from("/home/someone")),
            config_home: None,
            state_home: None,
        }
    }

    #[test]
    fn an_empty_file_means_the_local_daemon_with_notifications() {
        let config: Config = "".parse().unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(
            config.daemon,
            DaemonConfig::Local {
                socket: None,
                start_service: false
            }
        );
        assert!(config.notifications.enabled);
    }

    #[test]
    fn reads_an_ssh_daemon() {
        let text = r#"
            [daemon]
            transport = "ssh"
            host = "build-box"
            remote_command = "/opt/agents-cockpit/bin/agents-cockpit"

            [notifications]
            enabled = false
        "#;

        let config: Config = text.parse().unwrap();

        assert_eq!(
            config.daemon,
            DaemonConfig::Ssh {
                host: "build-box".to_owned(),
                remote_command: "/opt/agents-cockpit/bin/agents-cockpit".to_owned(),
            }
        );
        assert!(!config.notifications.enabled);
    }

    #[test]
    fn reads_a_local_socket() {
        let text = "[daemon]\ntransport = \"local\"\nsocket = \"/run/cockpit.sock\"\n";

        let config: Config = text.parse().unwrap();

        assert_eq!(
            config.daemon,
            DaemonConfig::Local {
                socket: Some(PathBuf::from("/run/cockpit.sock")),
                start_service: false
            }
        );
    }

    #[test]
    fn reads_the_daemon_in_the_app_and_a_service_to_start() {
        let in_app: Config = "[daemon]\ntransport = \"in_app\"\n".parse().unwrap();
        let service: Config = "[daemon]\ntransport = \"local\"\nstart_service = true\n"
            .parse()
            .unwrap();

        assert_eq!(in_app.daemon, DaemonConfig::InApp);
        assert_eq!(
            service.daemon,
            DaemonConfig::Local {
                socket: None,
                start_service: true
            }
        );
    }

    #[test]
    fn refuses_an_unknown_transport_or_key() {
        assert!("[daemon]\ntransport = \"tcp\"\n".parse::<Config>().is_err());
        assert!(
            "[daemon]\ntransport = \"local\"\nport = 22\n"
                .parse::<Config>()
                .is_err()
        );
        assert!(
            "[daemon]\ntransport = \"ssh\"\nhost = \"build-box\"\n"
                .parse::<Config>()
                .is_err()
        );
    }

    #[test]
    fn xdg_variables_take_precedence_over_home() {
        let environment = Environment {
            home: Some(PathBuf::from("/home/someone")),
            config_home: Some(PathBuf::from("/cfg")),
            state_home: Some(PathBuf::from("/state")),
        };

        assert_eq!(
            environment.config_file().unwrap(),
            PathBuf::from("/cfg/agents-cockpit/config.toml")
        );
        assert_eq!(
            environment.daemon_state_dir().unwrap(),
            PathBuf::from("/state/agents-cockpit")
        );
        assert_eq!(
            self::environment().config_file().unwrap(),
            PathBuf::from("/home/someone/.config/agents-cockpit/config.toml")
        );
        assert_eq!(
            self::environment().daemon_state_dir().unwrap(),
            PathBuf::from("/home/someone/.local/state/agents-cockpit")
        );
    }

    #[test]
    fn without_a_home_the_paths_cannot_be_found() {
        let environment = Environment::default();

        assert!(matches!(
            environment.config_file(),
            Err(ConfigError::NoHome { .. })
        ));
        assert!(matches!(
            environment.daemon_state_dir(),
            Err(ConfigError::NoHome { .. })
        ));
    }

    #[test]
    fn a_missing_file_loads_the_defaults() {
        let environment = Environment {
            home: None,
            config_home: Some(PathBuf::from("/nonexistent/agents-cockpit-test")),
            state_home: None,
        };

        assert_eq!(Config::load(&environment).unwrap(), Config::default());
    }

    #[test]
    fn saving_a_daemon_keeps_the_rest_of_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(CONFIG_FILE);
        let text = "# Reach the build box.\n[daemon]\ntransport = \"ssh\"\nhost = \"build-box\"\nremote_command = \"/opt/agents-cockpit\"\n\n[notifications]\n# Too many.\nenabled = false\n";
        std::fs::write(&file, text).unwrap();

        save_daemon(&file, &DaemonConfig::InApp).unwrap();

        let saved = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            saved,
            "# Reach the build box.\n[daemon]\ntransport = \"in_app\"\n\n[notifications]\n# Too many.\nenabled = false\n"
        );
        let config: Config = saved.parse().unwrap();
        assert_eq!(config.daemon, DaemonConfig::InApp);
        assert!(!config.notifications.enabled);
    }

    #[test]
    fn saving_a_daemon_creates_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(APP_FOLDER).join(CONFIG_FILE);
        let service = DaemonConfig::Local {
            socket: None,
            start_service: true,
        };

        save_daemon(&file, &service).unwrap();

        let saved = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            saved,
            "[daemon]\ntransport = \"local\"\nstart_service = true\n"
        );
        assert_eq!(saved.parse::<Config>().unwrap().daemon, service);
    }
}

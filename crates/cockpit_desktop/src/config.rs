//! The app's config file, `$XDG_CONFIG_HOME/agents-cockpit/config.toml` (on Windows,
//! `%APPDATA%\agents-cockpit\config.toml`): which daemon to reach.
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
//! A missing file means the local daemon, at its default socket.

use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use cockpit_client::Transport;
use serde::Deserialize;

const APP_FOLDER: &str = "agents-cockpit";
const CONFIG_FILE: &str = "config.toml";
const SOCKET_FILE: &str = "agents-cockpit.sock";

#[cfg(unix)]
const HOME_VAR: &str = "HOME";
#[cfg(unix)]
const CONFIG_HOME_VAR: &str = "XDG_CONFIG_HOME";
#[cfg(unix)]
const STATE_HOME_VAR: &str = "XDG_STATE_HOME";

/// The XDG defaults, relative to the home folder.
#[cfg(unix)]
const DEFAULT_CONFIG_HOME: &str = ".config";
#[cfg(unix)]
const DEFAULT_STATE_HOME: &str = ".local/state";

#[cfg(windows)]
const HOME_VAR: &str = "USERPROFILE";
#[cfg(windows)]
const CONFIG_HOME_VAR: &str = "APPDATA";
#[cfg(windows)]
const STATE_HOME_VAR: &str = "LOCALAPPDATA";

/// Windows' defaults, relative to the home folder.
#[cfg(windows)]
const DEFAULT_CONFIG_HOME: &str = r"AppData\Roaming";
#[cfg(windows)]
const DEFAULT_STATE_HOME: &str = r"AppData\Local";

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub notifications: NotificationsConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum DaemonConfig {
    /// `socket` defaults to `$XDG_STATE_HOME/agents-cockpit/agents-cockpit.sock`.
    Local {
        #[serde(default)]
        socket: Option<PathBuf>,
    },
    /// `remote_command` is the absolute path of `agents-cockpit` on the host.
    Ssh {
        host: String,
        remote_command: String,
    },
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self::Local { socket: None }
    }
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

/// How the app names the daemon's machine in its texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLabel {
    /// The SSH host, or "this machine".
    pub machine: String,
    pub over_ssh: bool,
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

    /// Where a daemon on this machine listens, as the daemon computes it.
    pub fn default_socket(&self) -> Result<PathBuf, ConfigError> {
        let base = self.state_base().ok_or(ConfigError::NoHome {
            what: "daemon's socket",
            var: STATE_HOME_VAR,
        })?;
        Ok(base.join(APP_FOLDER).join(SOCKET_FILE))
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

    pub fn transport(&self, environment: &Environment) -> Result<Transport, ConfigError> {
        match &self.daemon {
            DaemonConfig::Local {
                socket: Some(socket),
            } => Ok(Transport::Local {
                socket: socket.clone(),
            }),
            DaemonConfig::Local { socket: None } => Ok(Transport::Local {
                socket: environment.default_socket()?,
            }),
            DaemonConfig::Ssh {
                host,
                remote_command,
            } => Ok(Transport::Ssh {
                host: host.clone(),
                remote_command: remote_command.clone(),
            }),
        }
    }

    pub fn daemon_label(&self) -> DaemonLabel {
        match &self.daemon {
            DaemonConfig::Local { .. } => DaemonLabel {
                machine: "this machine".to_owned(),
                over_ssh: false,
            },
            DaemonConfig::Ssh { host, .. } => DaemonLabel {
                machine: host.clone(),
                over_ssh: true,
            },
        }
    }
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

    #[cfg(unix)]
    #[test]
    fn an_empty_file_means_the_local_daemon_with_notifications() {
        let config: Config = "".parse().unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(
            config.transport(&environment()).unwrap(),
            Transport::Local {
                socket: PathBuf::from(
                    "/home/someone/.local/state/agents-cockpit/agents-cockpit.sock"
                )
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
            config.transport(&environment()).unwrap(),
            Transport::Ssh {
                host: "build-box".to_owned(),
                remote_command: "/opt/agents-cockpit/bin/agents-cockpit".to_owned(),
            }
        );
        assert!(!config.notifications.enabled);
        assert_eq!(
            config.daemon_label(),
            DaemonLabel {
                machine: "build-box".to_owned(),
                over_ssh: true
            }
        );
    }

    #[test]
    fn reads_a_local_socket() {
        let text = "[daemon]\ntransport = \"local\"\nsocket = \"/run/cockpit.sock\"\n";

        let config: Config = text.parse().unwrap();

        assert_eq!(
            config.transport(&Environment::default()).unwrap(),
            Transport::Local {
                socket: PathBuf::from("/run/cockpit.sock")
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

    #[cfg(unix)]
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
            environment.default_socket().unwrap(),
            PathBuf::from("/state/agents-cockpit/agents-cockpit.sock")
        );
        assert_eq!(
            self::environment().config_file().unwrap(),
            PathBuf::from("/home/someone/.config/agents-cockpit/config.toml")
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
            Config::default().transport(&environment),
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
}

//! The app's config file, `$XDG_CONFIG_HOME/agents-cockpit/config.toml` (on Windows,
//! `%APPDATA%\agents-cockpit\config.toml`): which daemon to reach.
//!
//! ```toml
//! [daemon]
//! transport = "ssh"
//! host = "build-box"
//! remote_command = "/home/me/.local/bin/agents-cockpit"
//! # Where the daemon's update builds from, by default ~/agents-cockpit:
//! # source_dir = "/home/me/src/agents-cockpit"
//!
//! [notifications]
//! enabled = true
//! ```
//!
//! `[notifications] enabled = false` turns the app's notifications off until a choice in the
//! window. The window's bell sets them from then on, and its choice is saved with the preferences.
//!
//! The daemon may also be `transport = "local"`, with an optional `socket` and `start_service`,
//! or `transport = "in_app"`, the daemon in the app's own process. A missing file means the local
//! daemon, at its default socket.

use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, TomlError};

const APP_FOLDER: &str = "agents-cockpit";
const CONFIG_FILE: &str = "config.toml";
const DAEMON_SECTION: &str = "daemon";
const MACHINES_SECTION: &str = "machines";
/// Where a new config file is written before it replaces the old one.
const TEMPORARY_EXTENSION: &str = "toml.new";

/// The most symbolic links followed to the config file, as Linux allows.
const MAX_LINKS: usize = 40;

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
    /// This computer's daemon.
    pub daemon: DaemonConfig,
    /// The other machines, each reached over SSH, in the order the sidebar shows them.
    pub machines: Vec<MachineConfig>,
    pub notifications: NotificationsConfig,
}

/// A machine the app reaches over SSH beside this computer's daemon, as a `[[machines]]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    /// What the sidebar calls the machine.
    pub name: String,
    pub host: String,
    /// The absolute path of `agents-cockpit` on the host.
    pub remote_command: String,
    /// The git checkout the daemon's update builds from. Unset, the update script's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<String>,
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
    /// `remote_command` is the absolute path of `agents-cockpit` on the host, and `source_dir`
    /// the git checkout the daemon's update builds from (unset, the update script's default).
    Ssh {
        host: String,
        remote_command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_dir: Option<String>,
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

/// The `[[machines]]` tables alone, to write them.
#[derive(Serialize)]
struct MachinesSection<'config> {
    machines: &'config [MachineConfig],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationsConfig {
    /// The app's starting notification level: `false` is "Off". Read only until the window
    /// saves a level of its own.
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
        source: TomlError,
    },
    #[error("cannot encode the daemon's config")]
    Encode(#[source] toml::ser::Error),
    #[error("cannot write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is read-only", path.display())]
    ReadOnly { path: PathBuf },
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

/// Makes `daemon` the daemon of the config file `file`, as [`edit_config`] writes it.
pub fn save_daemon(file: &Path, daemon: &DaemonConfig) -> Result<(), ConfigError> {
    use toml_edit::Item;

    edit_config(file, |document, edit_error| {
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
        Ok(())
    })
}

/// Makes `machines` the `[[machines]]` tables of the config file `file`, as [`edit_config`]
/// writes it. An empty list removes the tables.
pub fn save_machines(file: &Path, machines: &[MachineConfig]) -> Result<(), ConfigError> {
    edit_config(file, |document, edit_error| {
        document.remove(MACHINES_SECTION);
        if machines.is_empty() {
            return Ok(());
        }
        let section =
            toml::to_string(&MachinesSection { machines }).map_err(ConfigError::Encode)?;
        let mut section: DocumentMut = section.parse().map_err(edit_error)?;
        if let Some(tables) = section.remove(MACHINES_SECTION) {
            document.insert(MACHINES_SECTION, tables);
        }
        Ok(())
    })
}

/// Changes the config file `file` with `edit`, keeping its other settings and its comments. A
/// missing file is created. When `file` is a symbolic link, as home-manager or a dotfiles
/// repository makes, its target is written and the link stays. The file keeps its mode, and a
/// read-only one is not written.
fn edit_config<Edit>(file: &Path, edit: Edit) -> Result<(), ConfigError>
where
    Edit: FnOnce(&mut DocumentMut, &dyn Fn(TomlError) -> ConfigError) -> Result<(), ConfigError>,
{
    let edit_error = |source| ConfigError::Edit {
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
    edit(&mut document, &edit_error)?;
    let target = link_target(file).map_err(|source| ConfigError::Read {
        path: file.to_owned(),
        source,
    })?;
    let write_error = |source| ConfigError::Write {
        path: target.clone(),
        source,
    };
    let permissions = match std::fs::metadata(&target) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(write_error(source)),
    };
    if permissions
        .as_ref()
        .is_some_and(std::fs::Permissions::readonly)
    {
        return Err(ConfigError::ReadOnly { path: target });
    }
    if let Some(folder) = target.parent() {
        std::fs::create_dir_all(folder).map_err(write_error)?;
    }
    let temporary = target.with_extension(TEMPORARY_EXTENSION);
    std::fs::write(&temporary, document.to_string()).map_err(write_error)?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&temporary, permissions).map_err(write_error)?;
    }
    std::fs::rename(&temporary, &target).map_err(write_error)
}

/// The file `file` names once its symbolic links are followed, which may not exist yet.
fn link_target(file: &Path) -> io::Result<PathBuf> {
    let mut path = file.to_owned();
    for _ in 0..MAX_LINKS {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&path)?;
                // A relative target is relative to the link's folder.
                path = match path.parent() {
                    Some(folder) => folder.join(target),
                    None => target,
                };
            }
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(format!(
        "more than {MAX_LINKS} symbolic links lead to {}",
        file.display()
    )))
}

impl FromStr for Config {
    type Err = toml::de::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        toml::from_str(text)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A made-up home folder, and the defaults under it, as the platform the tests run on writes
    /// them.
    #[cfg(unix)]
    pub(crate) const HOME: &str = "/home/someone";
    #[cfg(unix)]
    const CONFIG_UNDER_HOME: &str = "/home/someone/.config/agents-cockpit/config.toml";
    #[cfg(unix)]
    const STATE_UNDER_HOME: &str = "/home/someone/.local/state/agents-cockpit";
    #[cfg(windows)]
    pub(crate) const HOME: &str = r"C:\Users\someone";
    #[cfg(windows)]
    const CONFIG_UNDER_HOME: &str = r"C:\Users\someone\AppData\Roaming\agents-cockpit\config.toml";
    #[cfg(windows)]
    const STATE_UNDER_HOME: &str = r"C:\Users\someone\AppData\Local\agents-cockpit";

    fn environment() -> Environment {
        Environment {
            home: Some(PathBuf::from(HOME)),
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
                source_dir: None,
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
    fn each_machines_table_names_a_server_beside_this_computers_daemon() {
        let text = "[daemon]\ntransport = \"in_app\"\n\n[[machines]]\nname = \"devbox\"\nhost = \"dev@devbox\"\nremote_command = \"/opt/agents-cockpit\"\n\n[[machines]]\nname = \"gpu-box\"\nhost = \"gpu\"\nremote_command = \"/usr/local/bin/agents-cockpit\"\n";
        let config: Config = text.parse().unwrap();

        assert_eq!(config.daemon, DaemonConfig::InApp);
        assert_eq!(
            config.machines,
            [
                MachineConfig {
                    name: "devbox".to_owned(),
                    host: "dev@devbox".to_owned(),
                    remote_command: "/opt/agents-cockpit".to_owned(),
                    source_dir: None,
                },
                MachineConfig {
                    name: "gpu-box".to_owned(),
                    host: "gpu".to_owned(),
                    remote_command: "/usr/local/bin/agents-cockpit".to_owned(),
                    source_dir: None,
                },
            ]
        );
        assert!(
            "[[machines]]\nname = \"devbox\"\nhost = \"devbox\"\n"
                .parse::<Config>()
                .is_err(),
            "a machine needs the path of agents-cockpit"
        );
    }

    #[test]
    fn a_source_dir_is_optional_per_machine_and_on_an_ssh_daemon() {
        let text = "[daemon]\ntransport = \"ssh\"\nhost = \"build-box\"\nremote_command = \"/opt/agents-cockpit\"\nsource_dir = \"/srv/cockpit\"\n\n[[machines]]\nname = \"devbox\"\nhost = \"devbox\"\nremote_command = \"/opt/agents-cockpit\"\nsource_dir = \"/home/dev/src/cockpit\"\n";

        let config: Config = text.parse().unwrap();

        assert_eq!(
            config.daemon,
            DaemonConfig::Ssh {
                host: "build-box".to_owned(),
                remote_command: "/opt/agents-cockpit".to_owned(),
                source_dir: Some("/srv/cockpit".to_owned()),
            }
        );
        assert_eq!(
            config.machines.first().unwrap().source_dir.as_deref(),
            Some("/home/dev/src/cockpit")
        );
    }

    #[test]
    fn a_saved_server_keeps_its_source_dir() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(CONFIG_FILE);
        let devbox = MachineConfig {
            name: "devbox".to_owned(),
            host: "devbox".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: Some("/srv/cockpit".to_owned()),
        };

        save_machines(&file, std::slice::from_ref(&devbox)).unwrap();

        let config: Config = std::fs::read_to_string(&file).unwrap().parse().unwrap();
        assert_eq!(config.machines, [devbox]);
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
            home: Some(PathBuf::from(HOME)),
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
            PathBuf::from(CONFIG_UNDER_HOME)
        );
        assert_eq!(
            self::environment().daemon_state_dir().unwrap(),
            PathBuf::from(STATE_UNDER_HOME)
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
    fn saving_the_servers_replaces_their_tables_and_keeps_the_rest() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(CONFIG_FILE);
        let text = "# This computer.\n[daemon]\ntransport = \"in_app\"\n\n[[machines]]\nname = \"old\"\nhost = \"old-host\"\nremote_command = \"/opt/agents-cockpit\"\n";
        std::fs::write(&file, text).unwrap();
        let devbox = MachineConfig {
            name: "devbox".to_owned(),
            host: "dev@devbox".to_owned(),
            remote_command: "/home/dev/.cargo/bin/agents-cockpit".to_owned(),
            source_dir: None,
        };

        save_machines(&file, std::slice::from_ref(&devbox)).unwrap();
        let one: Config = std::fs::read_to_string(&file).unwrap().parse().unwrap();
        save_machines(&file, &[]).unwrap();
        let none = std::fs::read_to_string(&file).unwrap();

        assert_eq!(one.daemon, DaemonConfig::InApp);
        assert_eq!(one.machines, [devbox]);
        assert_eq!(none, "# This computer.\n[daemon]\ntransport = \"in_app\"\n");
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

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symbolic_link_writes_its_target_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let dotfiles = temp.path().join("dotfiles");
        let target = dotfiles.join(CONFIG_FILE);
        let link = temp.path().join(APP_FOLDER).join(CONFIG_FILE);
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(
            &target,
            "# Mine.
[daemon]
transport = \"local\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(Path::new("../dotfiles").join(CONFIG_FILE), &link).unwrap();

        save_daemon(&link, &DaemonConfig::InApp).unwrap();

        let link_type = std::fs::symlink_metadata(&link).unwrap().file_type();
        assert!(link_type.is_symlink(), "the link stays a link");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "# Mine.\n[daemon]\ntransport = \"in_app\"\n"
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
        let names: Vec<_> = std::fs::read_dir(&dotfiles).unwrap().collect();
        assert_eq!(names.len(), 1, "no temporary file is left");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_target_is_not_written() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("store-config.toml");
        let link = temp.path().join(CONFIG_FILE);
        let text = "[daemon]\ntransport = \"local\"\n";
        std::fs::write(&target, text).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let saved = save_daemon(&link, &DaemonConfig::InApp);

        assert!(
            matches!(&saved, Err(ConfigError::ReadOnly { path }) if *path == target),
            "{saved:?}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_missing_file_creates_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("dotfiles").join(CONFIG_FILE);
        let link = temp.path().join(CONFIG_FILE);
        std::os::unix::fs::symlink(&target, &link).unwrap();

        save_daemon(&link, &DaemonConfig::InApp).unwrap();

        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "[daemon]\ntransport = \"in_app\"\n"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

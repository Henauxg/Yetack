//! Where a server's daemon is installed, and who owns it, as the app reads it over SSH before it
//! offers to update it. The app replaces only a daemon it owns: one in a folder of the user's that
//! it installs to. Nix and `apt` own theirs, and the user updates those, as anything else, by hand.

/// The system the daemon this app carries runs on, as `uname -sm` prints it.
pub const CARRIED_SYSTEM: &str = "Linux x86_64";

/// What the app knows of a server's install.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DaemonProbe {
    /// The app has not looked yet, or looks again once asked.
    #[default]
    Unknown,
    Probing,
    Read(ServerInstall),
    /// ssh or the server failed, as this says.
    Failed(String),
}

/// A server, as the probe read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInstall {
    /// `uname -sm`: "Linux x86_64".
    pub system: String,
    /// `None` when no daemon is at the path the app was given.
    pub daemon: Option<InstalledDaemon>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledDaemon {
    /// The binary's absolute path, its links resolved.
    pub path: String,
    pub owner: DaemonOwner,
    /// What `--version` printed: "agents-cockpit 0.2.0 (3f9c2e1…)". Empty when it printed nothing.
    pub version: String,
}

/// Who may replace a daemon's binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonOwner {
    /// It is in `~/.local/share/agents-cockpit/bin` or `~/.local/bin`, where the user can write.
    App,
    /// It is in `/nix/store`: the system's configuration decides its version.
    Nix,
    /// `dpkg` names the package it comes from.
    Package(String),
    /// Anywhere else: the user updates it by hand.
    Other,
}

impl DaemonProbe {
    /// The probe of a server whose daemon the app owns, at `path`.
    #[cfg(test)]
    pub fn owned_at(path: &str) -> Self {
        Self::Read(ServerInstall {
            system: CARRIED_SYSTEM.to_owned(),
            daemon: Some(InstalledDaemon {
                path: path.to_owned(),
                owner: DaemonOwner::App,
                version: "agents-cockpit 0.0.1".to_owned(),
            }),
        })
    }
}

impl ServerInstall {
    /// Reads the probe's lines, `key=value` each. `Err` says what was missing.
    pub fn parse(output: &str) -> Result<Self, String> {
        let value = |key: &str| {
            output.lines().find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name == key).then(|| value.trim().to_owned())
            })
        };
        let system = value("system").ok_or("The server did not say which system it runs")?;
        let owner = match value("owner").as_deref() {
            Some("missing") => {
                return Ok(Self {
                    system,
                    daemon: None,
                });
            }
            Some("app") => DaemonOwner::App,
            Some("nix") => DaemonOwner::Nix,
            Some("other") => DaemonOwner::Other,
            Some(owner) => match owner.strip_prefix("package ") {
                Some(package) => DaemonOwner::Package(package.to_owned()),
                None => return Err(format!("The server named an unknown owner: {owner}")),
            },
            None => return Err("The server did not say who owns its daemon".to_owned()),
        };
        let path = value("path").ok_or("The server did not say where its daemon is")?;
        Ok(Self {
            system,
            daemon: Some(InstalledDaemon {
                path,
                owner,
                version: value("version").unwrap_or_default(),
            }),
        })
    }

    /// Whether the daemon this app carries runs there.
    pub fn runs_carried_daemon(&self) -> bool {
        self.system == CARRIED_SYSTEM
    }

    /// The daemon the app may replace: one it owns.
    pub fn replaceable(&self) -> Option<&InstalledDaemon> {
        self.daemon
            .as_ref()
            .filter(|daemon| daemon.owner == DaemonOwner::App)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_the_app_owns_comes_with_its_path_and_version() {
        let output = "system=Linux x86_64\npath=/home/someone/.local/bin/agents-cockpit\nowner=app\nversion=agents-cockpit 0.1.0 (743d664)\n";

        let install = ServerInstall::parse(output).unwrap();

        assert!(install.runs_carried_daemon());
        assert_eq!(
            install.replaceable(),
            Some(&InstalledDaemon {
                path: "/home/someone/.local/bin/agents-cockpit".to_owned(),
                owner: DaemonOwner::App,
                version: "agents-cockpit 0.1.0 (743d664)".to_owned(),
            })
        );
    }

    #[test]
    fn a_daemon_a_package_owns_names_the_package_and_is_not_replaceable() {
        let output = "system=Linux x86_64\npath=/usr/bin/agents-cockpit\nowner=package agents-cockpit\nversion=agents-cockpit 0.1.0\n";

        let install = ServerInstall::parse(output).unwrap();

        assert_eq!(
            install.daemon.as_ref().map(|daemon| &daemon.owner),
            Some(&DaemonOwner::Package("agents-cockpit".to_owned()))
        );
        assert_eq!(install.replaceable(), None);
    }

    #[test]
    fn a_server_without_a_daemon_or_of_another_system_says_so() {
        let install = ServerInstall::parse("system=Linux aarch64\nowner=missing\n").unwrap();

        assert_eq!(install.daemon, None);
        assert!(!install.runs_carried_daemon());
    }

    #[test]
    fn output_that_is_not_the_probes_is_an_error() {
        assert!(ServerInstall::parse("Welcome to the server!\n").is_err());
        assert!(ServerInstall::parse("system=Linux x86_64\nowner=root\npath=/x\n").is_err());
    }
}

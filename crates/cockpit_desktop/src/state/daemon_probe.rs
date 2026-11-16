//! Where a server's daemon is installed, and who owns it, as the app reads it over SSH before it
//! offers to update it. The app replaces only a daemon it owns: one in a folder of the user's that
//! it installs to. Nix, `apt` and `dnf` own theirs, and the user updates those, as anything else,
//! by hand.

use std::str::FromStr;

use strum::{EnumString, IntoStaticStr, VariantArray};

use super::update_plan::ServerPlan;

/// The system the daemon this app carries runs on, as `uname -sm` prints it.
pub const CARRIED_SYSTEM: &str = "Linux x86_64";

/// The folder the app installs the daemon to, as the screens name it.
pub const APP_BIN_FOLDER: &str = "~/.local/share/yetack/bin";

/// The user's own folder of programs, where the app also replaces a daemon it finds.
pub const USER_BIN_FOLDER: &str = "~/.local/bin";

/// The daemon's systemd unit, which the install writes.
pub const UNIT_FILE: &str = "~/.config/systemd/user/yetack.service";

/// The keys of the probe's `key=value` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum ProbeKey {
    System,
    Path,
    Command,
    Owner,
    Version,
}

/// The words the probe gives as `owner`, besides a package's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum OwnerWord {
    /// No daemon is at the path.
    Missing,
    App,
    Nix,
    Other,
}

/// What the probe's `owner` starts with before a package's format and name: "package deb yetack".
pub const PACKAGE_OWNER: &str = "package ";

/// The kind of package that holds a daemon, as the probe names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum PackageFormat {
    /// `dpkg` names its package: the release's `.deb`.
    Deb,
    /// `rpm` names its package: the release's `.rpm`.
    Rpm,
}

/// What the app knows of a server's install.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DaemonProbe {
    /// The app has not looked yet, or looks again once asked.
    #[default]
    Unknown,
    /// The probe of this number runs.
    Probing(u64),
    Read(ServerInstall),
    /// ssh or the server failed, as this says.
    Failed(String),
}

/// A probe to run: the server, and the daemon's path or command. Its number tells it from a probe
/// that an edit or a retry replaced since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTarget {
    pub host: String,
    pub binary: String,
    pub number: u64,
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
    /// The path the server runs it by, before the links: a profile's link outlives the Nix
    /// store path it leads to now.
    pub command: String,
    pub owner: DaemonOwner,
    /// What `--version` printed: "yetack 0.2.0 (3f9c2e1…)". Empty when it printed nothing.
    pub version: String,
}

/// Who may replace a daemon's binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonOwner {
    /// It is in `~/.local/share/yetack/bin` or `~/.local/bin`, where the user can write.
    App,
    /// It is in `/nix/store`: the system's configuration decides its version.
    Nix,
    /// A package of the system holds it.
    Package { name: String, format: PackageFormat },
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
                command: path.to_owned(),
                owner: DaemonOwner::App,
                version: "yetack 0.0.1".to_owned(),
            }),
        })
    }
}

impl FromStr for ServerInstall {
    type Err = String;

    /// Reads the probe's lines, `key=value` each. `Err` says what was missing.
    fn from_str(output: &str) -> Result<Self, String> {
        let value = |key: ProbeKey| {
            let key: &str = key.into();
            output.lines().find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name == key).then(|| value.trim().to_owned())
            })
        };
        let system =
            value(ProbeKey::System).ok_or("The server did not say which system it runs")?;
        let Some(owner) = value(ProbeKey::Owner) else {
            return Err("The server did not say who owns its daemon".to_owned());
        };
        let owner = match owner.parse::<OwnerWord>() {
            Ok(OwnerWord::Missing) => {
                return Ok(Self {
                    system,
                    daemon: None,
                });
            }
            Ok(OwnerWord::App) => DaemonOwner::App,
            Ok(OwnerWord::Nix) => DaemonOwner::Nix,
            Ok(OwnerWord::Other) => DaemonOwner::Other,
            Err(_) => {
                let unknown = || format!("The server named an unknown owner: {owner}");
                let package = owner
                    .strip_prefix(PACKAGE_OWNER)
                    .and_then(|package| package.split_once(' '));
                let Some((format, name)) = package else {
                    return Err(unknown());
                };
                let format: PackageFormat = format.parse().map_err(|_| unknown())?;
                DaemonOwner::Package {
                    name: name.to_owned(),
                    format,
                }
            }
        };
        let path = value(ProbeKey::Path).ok_or("The server did not say where its daemon is")?;
        let command = value(ProbeKey::Command)
            .filter(|command| command.starts_with('/'))
            .unwrap_or_else(|| path.clone());
        Ok(Self {
            system,
            daemon: Some(InstalledDaemon {
                path,
                command,
                owner,
                version: value(ProbeKey::Version).unwrap_or_default(),
            }),
        })
    }
}

impl ServerInstall {
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

    /// The name of the package that owns the daemon, when one does.
    pub fn package(&self) -> Option<&str> {
        match &self.daemon.as_ref()?.owner {
            DaemonOwner::Package { name, .. } => Some(name),
            DaemonOwner::App | DaemonOwner::Nix | DaemonOwner::Other => None,
        }
    }

    /// `plan` as an update runs it here: it replaces the daemon at the path the probe resolved,
    /// and sends the daemon this app carries only to a system it runs on.
    pub fn plan_for(&self, mut plan: ServerPlan) -> ServerPlan {
        if let Some(daemon) = &self.daemon {
            plan.remote_command.clone_from(&daemon.path);
        }
        if !self.runs_carried_daemon() {
            plan.daemon_file = None;
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::BuildCommit;

    use super::*;

    #[test]
    fn a_daemon_the_app_owns_comes_with_its_path_and_version() {
        let output = "system=Linux x86_64\npath=/home/someone/.local/bin/yetack\ncommand=yetack\nowner=app\nversion=yetack 0.1.0 (743d664)\n";

        let install: ServerInstall = output.parse().unwrap();

        assert!(install.runs_carried_daemon());
        assert_eq!(
            install.replaceable(),
            Some(&InstalledDaemon {
                path: "/home/someone/.local/bin/yetack".to_owned(),
                command: "/home/someone/.local/bin/yetack".to_owned(),
                owner: DaemonOwner::App,
                version: "yetack 0.1.0 (743d664)".to_owned(),
            })
        );
    }

    #[test]
    fn a_daemon_a_package_owns_names_the_package_and_is_not_replaceable() {
        for (word, format) in [("deb", PackageFormat::Deb), ("rpm", PackageFormat::Rpm)] {
            let output = format!(
                "system=Linux x86_64\npath=/usr/bin/yetack\nowner=package {word} yetack\nversion=yetack 0.1.0\n"
            );

            let install: ServerInstall = output.parse().unwrap();

            assert_eq!(
                install.daemon.as_ref().map(|daemon| &daemon.owner),
                Some(&DaemonOwner::Package {
                    name: "yetack".to_owned(),
                    format,
                })
            );
            assert_eq!(install.package(), Some("yetack"));
            assert_eq!(install.replaceable(), None);
        }
    }

    #[test]
    fn a_package_of_an_unknown_format_is_an_error() {
        let output = "system=Linux x86_64\npath=/usr/bin/yetack\nowner=package snap yetack\n";

        assert!(output.parse::<ServerInstall>().is_err());
    }

    #[test]
    fn a_daemon_found_through_a_link_keeps_the_links_path_to_run_by() {
        let output = "system=Linux x86_64\npath=/nix/store/0c4d9kx2-yetack/bin/yetack\ncommand=/home/someone/.nix-profile/bin/yetack\nowner=nix\n";

        let install: ServerInstall = output.parse().unwrap();
        let daemon = install.daemon.unwrap();

        assert_eq!(daemon.command, "/home/someone/.nix-profile/bin/yetack");
        assert_eq!(daemon.path, "/nix/store/0c4d9kx2-yetack/bin/yetack");
    }

    #[test]
    fn a_server_without_a_daemon_or_of_another_system_says_so() {
        let install: ServerInstall = "system=Linux aarch64\nowner=missing\n".parse().unwrap();

        assert_eq!(install.daemon, None);
        assert!(!install.runs_carried_daemon());
    }

    #[test]
    fn output_that_is_not_the_probes_is_an_error() {
        assert!("Welcome to the server!\n".parse::<ServerInstall>().is_err());
        assert!(
            "system=Linux x86_64\nowner=root\npath=/x\n"
                .parse::<ServerInstall>()
                .is_err()
        );
    }

    #[test]
    fn the_probes_words_are_pinned() {
        let keys: Vec<&str> = ProbeKey::VARIANTS.iter().map(|&key| key.into()).collect();
        let owners: Vec<&str> = OwnerWord::VARIANTS
            .iter()
            .map(|&owner| owner.into())
            .collect();
        let formats: Vec<&str> = PackageFormat::VARIANTS
            .iter()
            .map(|&format| format.into())
            .collect();

        assert_eq!(keys, ["system", "path", "command", "owner", "version"]);
        assert_eq!(owners, ["missing", "app", "nix", "other"]);
        assert_eq!(formats, ["deb", "rpm"]);
    }

    #[test]
    fn an_update_sends_the_carried_daemon_only_to_its_system_and_replaces_the_probed_path() {
        let plan = ServerPlan {
            host: "build-box".to_owned(),
            remote_command: "yetack".to_owned(),
            source_dir: None,
            commit: BuildCommit::new("3f9c2e1".to_owned()),
            daemon_file: Some("/opt/app/yetack-x86_64-linux".into()),
        };
        let here = "system=Linux x86_64\npath=/home/someone/.local/bin/yetack\nowner=app\n";
        let arm = "system=Linux aarch64\npath=/home/someone/.local/bin/yetack\nowner=app\n";
        let here: ServerInstall = here.parse().unwrap();
        let arm: ServerInstall = arm.parse().unwrap();

        let sends = here.plan_for(plan.clone());
        let builds = arm.plan_for(plan);

        assert_eq!(sends.remote_command, "/home/someone/.local/bin/yetack");
        assert!(sends.daemon_file.is_some());
        assert_eq!(builds.remote_command, "/home/someone/.local/bin/yetack");
        assert_eq!(builds.daemon_file, None);
    }
}

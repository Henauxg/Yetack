//! How the app names the daemon it reaches, in its texts.

use crate::path_style::PathStyle;

const THIS_MACHINE: &str = "this machine";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLabel {
    /// The SSH host, or "this machine".
    pub machine: String,
    pub place: DaemonPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonPlace {
    /// A daemon on this machine, at its socket.
    Socket,
    /// The background service the app starts when it does not answer.
    Service,
    Ssh,
    /// The daemon in the app's own process.
    InApp,
}

impl DaemonLabel {
    pub fn socket() -> Self {
        Self::on_this_machine(DaemonPlace::Socket)
    }

    pub fn service() -> Self {
        Self::on_this_machine(DaemonPlace::Service)
    }

    pub fn ssh(host: String) -> Self {
        Self {
            machine: host,
            place: DaemonPlace::Ssh,
        }
    }

    pub fn in_app() -> Self {
        Self::on_this_machine(DaemonPlace::InApp)
    }

    fn on_this_machine(place: DaemonPlace) -> Self {
        Self {
            machine: THIS_MACHINE.to_owned(),
            place,
        }
    }

    /// The path rules of the daemon's machine. SSH reaches `agents-cockpit bridge`, which only
    /// Unix builds.
    pub fn path_style(&self) -> PathStyle {
        match self.place {
            DaemonPlace::Ssh => PathStyle::Unix,
            DaemonPlace::Socket | DaemonPlace::Service | DaemonPlace::InApp => PathStyle::NATIVE,
        }
    }

    /// The daemon as the "No daemon answers" screen names it.
    pub fn name(&self) -> String {
        match self.place {
            DaemonPlace::Socket => "the daemon on this machine".to_owned(),
            DaemonPlace::Service => "the background service on this machine".to_owned(),
            DaemonPlace::Ssh => format!("{}, over SSH", self.machine),
            DaemonPlace::InApp => "the daemon in this app".to_owned(),
        }
    }

    /// The daemon at the start of a title, such as "build-box does not answer".
    pub fn subject(&self) -> String {
        match self.place {
            DaemonPlace::Socket => "The daemon on this machine".to_owned(),
            DaemonPlace::Service => "The background service".to_owned(),
            DaemonPlace::Ssh => self.machine.clone(),
            DaemonPlace::InApp => "The daemon in this app".to_owned(),
        }
    }

    /// The daemon as the sidebar's foot names it once connected.
    pub fn connected_line(&self) -> String {
        match self.place {
            DaemonPlace::Socket => "The daemon on this machine".to_owned(),
            DaemonPlace::Service => "The background service on this machine".to_owned(),
            DaemonPlace::Ssh => format!("{}, over SSH", self.machine),
            DaemonPlace::InApp => "Agents run in this app".to_owned(),
        }
    }
}

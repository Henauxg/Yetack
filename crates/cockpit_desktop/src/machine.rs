//! The machines the app reaches, each through its own daemon, and the app's ids for their agents.
//!
//! Every daemon numbers its own agents from 1, so two daemons give the same ids. The app gives
//! each agent an id of its own: the machine in the high bits, the daemon's id in the low ones.
//! This computer is machine 0, whose agents keep the daemon's ids. The connection to each machine
//! translates the ids of what it sends and receives.

use cockpit_client::SessionUpdate;
use cockpit_protocol::{AgentId, Catchup, Request, ResponseBody};

/// The bits of an app id that hold the daemon's id: a daemon numbers about a million agents
/// before its ids reach another machine's.
const DAEMON_BITS: u32 = 20;
const DAEMON_MASK: u32 = (1 << DAEMON_BITS) - 1;

/// A machine the app reaches. The default is this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct MachineId(u16);

impl MachineId {
    /// This computer: a daemon in the app, the background service, or a socket here.
    pub const THIS: Self = Self(0);

    /// The last machine whose number fits in the high bits of an app id.
    pub const LAST: Self = Self((1 << (u32::BITS - DAEMON_BITS)) - 1);

    pub fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub fn get(self) -> u16 {
        self.0
    }

    /// The machine whose daemon runs the agent the app calls `agent`.
    pub fn of(agent: AgentId) -> Self {
        let machine = agent.get() >> DAEMON_BITS;
        Self(u16::try_from(machine).unwrap_or(u16::MAX))
    }

    /// The app's id for the agent this machine's daemon calls `agent`.
    pub fn app_agent(self, agent: AgentId) -> AgentId {
        AgentId::new((u32::from(self.0) << DAEMON_BITS) | (agent.get() & DAEMON_MASK))
    }

    /// The id this machine's daemon gives the agent the app calls `agent`.
    pub fn daemon_agent(agent: AgentId) -> AgentId {
        AgentId::new(agent.get() & DAEMON_MASK)
    }

    /// A request from the app, as this machine's daemon reads it.
    pub fn request_to_daemon(request: &mut Request) {
        request.map_agents(Self::daemon_agent);
    }

    /// An answer from this machine's daemon, as the app reads it.
    pub fn answer_to_app(self, body: &mut ResponseBody) {
        body.map_agents(|agent| self.app_agent(agent));
    }

    /// An update from this machine's daemon, as the app reads it.
    pub fn update_to_app(self, update: &mut SessionUpdate) {
        let map = |agent| self.app_agent(agent);
        match update {
            SessionUpdate::Connected(Catchup::Snapshot(snapshot)) => snapshot.map_agents(map),
            SessionUpdate::Event(record) => record.event.map_agents(map),
            SessionUpdate::Connected(Catchup::Replay)
            | SessionUpdate::Connecting { .. }
            | SessionUpdate::Disconnected { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentParams, Event, EventRecord, Seq};

    use super::*;

    #[test]
    fn this_computers_agents_keep_the_daemons_ids() {
        let agent = AgentId::new(42);

        assert_eq!(MachineId::THIS.app_agent(agent), agent);
        assert_eq!(MachineId::of(agent), MachineId::THIS);
        assert_eq!(MachineId::daemon_agent(agent), agent);
    }

    #[test]
    fn two_machines_give_the_same_daemon_id_two_app_ids() {
        let devbox = MachineId::new(1);
        let gpu = MachineId::new(2);
        let daemon = AgentId::new(3);

        let on_devbox = devbox.app_agent(daemon);
        let on_gpu = gpu.app_agent(daemon);

        assert_ne!(on_devbox, on_gpu);
        assert_eq!(MachineId::of(on_devbox), devbox);
        assert_eq!(MachineId::of(on_gpu), gpu);
        assert_eq!(MachineId::daemon_agent(on_gpu), daemon);
    }

    #[test]
    fn the_last_machine_keeps_its_agents_apart() {
        let daemon = AgentId::new(DAEMON_MASK);

        let app = MachineId::LAST.app_agent(daemon);

        assert_eq!(MachineId::of(app), MachineId::LAST);
        assert_eq!(MachineId::daemon_agent(app), daemon);
        assert_eq!(app, AgentId::new(u32::MAX));
    }

    #[test]
    fn messages_cross_between_the_app_and_a_machine() {
        let devbox = MachineId::new(1);
        let app = devbox.app_agent(AgentId::new(7));
        let mut request = Request::AgentStop(AgentParams { agent: app });
        let mut update = SessionUpdate::Event(EventRecord {
            seq: Seq::new(1),
            event: Event::AgentRemoved {
                agent: AgentId::new(7),
            },
        });

        MachineId::request_to_daemon(&mut request);
        devbox.update_to_app(&mut update);

        assert_eq!(
            request,
            Request::AgentStop(AgentParams {
                agent: AgentId::new(7)
            })
        );
        assert!(matches!(
            update,
            SessionUpdate::Event(record) if record.event == Event::AgentRemoved { agent: app }
        ));
    }
}

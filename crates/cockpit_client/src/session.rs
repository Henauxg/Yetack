//! A connection that comes back after a drop.

use std::time::Duration;

use cockpit_protocol::{Catchup, EventRecord, Request, ResponseBody, Seq};
use tokio::sync::mpsc;

use crate::connection::{Connection, EVENT_QUEUE};
use crate::error::{CloseError, ConnectError, RequestError};
use crate::lossy_channel;
use crate::lossy_channel::{LossyReceiver, LossySender, Received, Sent};
use crate::requester::{Command, Requester};
use crate::transport::Transport;

/// The wait between connection attempts: `initial` after a drop, doubling after each failed
/// attempt up to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
        }
    }
}

#[derive(Debug)]
pub enum SessionUpdate {
    /// An attempt starts. `attempt` counts from 1 since the last successful connection.
    Connecting {
        attempt: u32,
    },
    /// Connected. With `Catchup::Snapshot`, replace the whole state. With `Catchup::Replay`, keep
    /// it: the missed events follow as `Event`s.
    Connected(Catchup),
    /// The attempt failed, or the connection dropped. Requests in flight failed with
    /// `RequestError::Disconnected`; until the next connection they fail with
    /// `RequestError::NotConnected`.
    ///
    /// With `CloseError::EventsLost`, events were dropped and the session closed the connection to
    /// resynchronize: the next `Connected` brings a snapshot.
    Disconnected {
        error: SessionError,
        retry_in: Duration,
    },
    Event(EventRecord),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Closed(#[from] CloseError),
}

/// Connects to a daemon, and reconnects after every drop with the last `Seq` it saw.
///
/// Updates wait for `next_update` in a queue of 4096. When an event finds the queue full, the
/// session drops the waiting updates and reconnects without a last `Seq`: `next_update` then gives
/// `Disconnected` with `CloseError::EventsLost`, `Connecting`, and `Connected` with a snapshot.
///
/// A task owns the current connection; this handle talks to it over channels. Dropping the
/// session ends the task and closes the connection.
pub struct Session {
    requester: Requester,
    updates: LossyReceiver<SessionUpdate>,
}

impl Session {
    /// Starts connecting at once.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, since it spawns the session's task.
    #[must_use]
    pub fn start(transport: Transport, backoff: Backoff) -> Self {
        let (commands_sender, commands) = mpsc::unbounded_channel();
        let (updates_sender, updates) = lossy_channel::channel(EVENT_QUEUE);
        tokio::spawn(run(transport, backoff, commands, updates_sender));
        Self {
            requester: Requester::new(commands_sender),
            updates,
        }
    }

    /// A handle that sends requests on whichever connection is current.
    #[must_use]
    pub fn requester(&self) -> &Requester {
        &self.requester
    }

    pub async fn request(&self, request: Request) -> Result<ResponseBody, RequestError> {
        self.requester.request(request).await
    }

    /// `None` only when the session's task has ended, as when the runtime shuts down.
    ///
    /// It is cancel safe, so it can wait in a `tokio::select!`.
    pub async fn next_update(&mut self) -> Option<SessionUpdate> {
        loop {
            match self.updates.recv().await? {
                Received::Item(update) => return Some(update),
                // The `Disconnected` that follows reports it.
                Received::Lost => {}
            }
        }
    }
}

/// The session's task. It returns when the `Session` is dropped.
async fn run(
    transport: Transport,
    backoff: Backoff,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: LossySender<SessionUpdate>,
) {
    let mut last_seq: Option<Seq> = None;
    let mut delay = backoff.initial;
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        let connecting = SessionUpdate::Connecting { attempt };
        if report_while_disconnected(connecting, &mut commands, &updates)
            .await
            .is_none()
        {
            return;
        }
        let connect = Connection::connect(&transport, last_seq);
        let Some(connected) = while_disconnected(connect, &mut commands, &updates).await else {
            return;
        };
        let error = match connected {
            Ok((connection, catchup)) => {
                attempt = 0;
                delay = backoff.initial;
                if let Catchup::Snapshot(snapshot) = &catchup {
                    last_seq = snapshot.last_seq;
                }
                let served = serve(connection, catchup, &mut commands, &updates, &mut last_seq);
                let Some(error) = served.await else {
                    return;
                };
                if matches!(error, CloseError::EventsLost) {
                    last_seq = None;
                }
                SessionError::Closed(error)
            }
            Err(error) => SessionError::Connect(error),
        };
        let disconnected = SessionUpdate::Disconnected {
            error,
            retry_in: delay,
        };
        if report_while_disconnected(disconnected, &mut commands, &updates)
            .await
            .is_none()
        {
            return;
        }
        let wait = tokio::time::sleep(delay);
        if while_disconnected(wait, &mut commands, &updates)
            .await
            .is_none()
        {
            return;
        }
        delay = delay.saturating_mul(2).min(backoff.max);
    }
}

/// Queues `update`, waiting for room, and refuses the requests that come meanwhile. Returns `None`
/// when the `Session` is dropped.
async fn report_while_disconnected(
    update: SessionUpdate,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    updates: &LossySender<SessionUpdate>,
) -> Option<()> {
    while_disconnected(updates.send(update), commands, updates)
        .await?
        .ok()
}

/// Runs `future` and refuses the requests that come meanwhile, since no connection carries them.
/// Returns `None` when the `Session` is dropped.
async fn while_disconnected<F: Future>(
    future: F,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    updates: &LossySender<SessionUpdate>,
) -> Option<F::Output> {
    let refuse = |command: Command| command.answer(Err(RequestError::NotConnected));
    meanwhile(future, commands, updates, refuse).await
}

/// Runs `future` and hands the requests that come meanwhile to `handle`. Returns `None` when the
/// `Session` is dropped.
async fn meanwhile<F: Future>(
    future: F,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    updates: &LossySender<SessionUpdate>,
    mut handle: impl FnMut(Command),
) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Some(output),
            command = commands.recv() => handle(command?),
            () = updates.closed() => return None,
        }
    }
}

/// Reports the connection, then passes events to the session's user and requests to the
/// connection, until the connection closes or an event finds the update queue full. Returns
/// `None` when the `Session` is dropped.
async fn serve(
    mut connection: Connection,
    catchup: Catchup,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    updates: &LossySender<SessionUpdate>,
    last_seq: &mut Option<Seq>,
) -> Option<CloseError> {
    let requester = connection.requester().clone();
    let connected = updates.send(SessionUpdate::Connected(catchup));
    let forward = |command| requester.forward(command);
    meanwhile(connected, commands, updates, forward)
        .await?
        .ok()?;
    loop {
        tokio::select! {
            event = connection.next_event() => match event {
                Ok(record) => {
                    let seq = record.seq;
                    match updates.send_or_drop(SessionUpdate::Event(record)) {
                        Sent::Queued => *last_seq = Some(seq),
                        Sent::Dropped => return Some(CloseError::EventsLost),
                        Sent::Closed => return None,
                    }
                }
                Err(error) => return Some(error),
            },
            command = commands.recv() => requester.forward(command?),
            () = updates.closed() => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentId, AgentParams, Event};

    use super::*;
    use crate::test_daemon;
    use crate::test_daemon::{TestDaemon, in_time};

    const FAST: Backoff = Backoff {
        initial: Duration::from_millis(10),
        max: Duration::from_millis(25),
    };

    async fn next(session: &mut Session) -> SessionUpdate {
        tokio::time::timeout(Duration::from_secs(5), session.next_update())
            .await
            .expect("an update within 5 seconds")
            .expect("the session runs")
    }

    fn removed(agent: u32) -> Event {
        Event::AgentRemoved {
            agent: AgentId::new(agent),
        }
    }

    fn interrupt() -> Request {
        Request::AgentInterrupt(AgentParams {
            agent: AgentId::new(3),
        })
    }

    #[tokio::test]
    async fn reconnects_with_the_last_seq_seen() {
        let daemon = TestDaemon::new();
        let mut session = Session::start(daemon.transport(), FAST);

        // First connection: a snapshot at 10, then the connection drops.
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connecting { attempt: 1 }
        ));
        let (peer, last_seq) = daemon
            .accept_and_welcome(Catchup::Snapshot(test_daemon::snapshot(Some(10))))
            .await;
        assert_eq!(last_seq, None);
        let SessionUpdate::Connected(Catchup::Snapshot(received)) = next(&mut session).await else {
            panic!("expected a snapshot");
        };
        assert_eq!(received, test_daemon::snapshot(Some(10)));
        drop(peer);
        let SessionUpdate::Disconnected { error, retry_in } = next(&mut session).await else {
            panic!("expected a disconnection");
        };
        assert!(
            matches!(error, SessionError::Closed(CloseError::Closed)),
            "{error:?}"
        );
        assert_eq!(retry_in, FAST.initial);

        // Second connection: the snapshot's seq, a replay, and an event.
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connecting { attempt: 1 }
        ));
        let (mut peer, last_seq) = daemon.accept_and_welcome(Catchup::Replay).await;
        assert_eq!(last_seq, Some(Seq::new(10)));
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connected(Catchup::Replay)
        ));
        peer.event(11, removed(1)).await;
        let SessionUpdate::Event(record) = next(&mut session).await else {
            panic!("expected an event");
        };
        assert_eq!((record.seq, record.event), (Seq::new(11), removed(1)));
        drop(peer);
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Disconnected { .. }
        ));

        // Third connection: the event's seq.
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connecting { attempt: 1 }
        ));
        let (_peer, last_seq) = daemon.accept_and_welcome(Catchup::Replay).await;
        assert_eq!(last_seq, Some(Seq::new(11)));
    }

    #[tokio::test]
    async fn requests_go_through_the_current_connection() {
        let daemon = TestDaemon::new();
        let mut session = Session::start(daemon.transport(), FAST);
        let (mut peer, _) = daemon.accept_and_welcome(Catchup::Replay).await;
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connecting { .. }
        ));
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connected(_)
        ));
        let interrupt = Request::AgentInterrupt(AgentParams {
            agent: AgentId::new(3),
        });
        let answer = async {
            let (id, request) = peer.request().await;
            peer.respond(id, ResponseBody::Done).await;
            request
        };

        let (response, request) = tokio::join!(session.request(interrupt.clone()), answer);

        assert_eq!(request, interrupt);
        assert_eq!(response.unwrap(), ResponseBody::Done);
    }

    #[tokio::test]
    async fn retries_back_off_and_requests_fail_while_disconnected() {
        let daemon = TestDaemon::new();
        let transport = Transport::Local {
            socket: daemon.missing_socket(),
        };
        let mut session = Session::start(transport, FAST);

        let mut waits = Vec::new();
        for expected_attempt in 1..=4 {
            let SessionUpdate::Connecting { attempt } = next(&mut session).await else {
                panic!("expected an attempt");
            };
            assert_eq!(attempt, expected_attempt);
            let SessionUpdate::Disconnected { error, retry_in } = next(&mut session).await else {
                panic!("expected a failed attempt");
            };
            assert!(
                matches!(error, SessionError::Connect(ConnectError::Socket { .. })),
                "{error:?}"
            );
            waits.push(retry_in);
        }
        let response = session
            .request(Request::AgentInterrupt(AgentParams {
                agent: AgentId::new(3),
            }))
            .await;

        let millis: Vec<u128> = waits.iter().map(Duration::as_millis).collect();
        assert_eq!(millis, [10, 20, 25, 25]);
        assert!(
            matches!(response, Err(RequestError::NotConnected)),
            "{response:?}"
        );
    }

    #[tokio::test]
    async fn a_session_whose_updates_overflow_reconnects_for_a_snapshot() {
        let daemon = TestDaemon::new();
        let mut session = Session::start(daemon.transport(), FAST);
        let (mut peer, _) = daemon.accept_and_welcome(Catchup::Replay).await;
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connecting { .. }
        ));
        assert!(matches!(
            next(&mut session).await,
            SessionUpdate::Connected(_)
        ));
        let flood = test_daemon::more_events_than_a_queue_holds();

        // Nobody reads the updates, so the session drops them and the connection.
        let closed = in_time(async {
            for seq in 1..=flood {
                peer.event(seq, removed(1)).await;
            }
            peer.next().await
        })
        .await;
        let refused = in_time(session.request(interrupt())).await;
        let disconnected = next(&mut session).await;
        let connecting = next(&mut session).await;
        let ((_peer, last_seq), connected) = tokio::join!(
            daemon.accept_and_welcome(Catchup::Snapshot(test_daemon::snapshot(Some(flood)))),
            next(&mut session),
        );

        assert_eq!(closed, None);
        assert!(
            matches!(refused, Err(RequestError::NotConnected)),
            "{refused:?}"
        );
        let SessionUpdate::Disconnected { error, .. } = disconnected else {
            panic!("expected a disconnection, got {disconnected:?}");
        };
        assert!(
            matches!(error, SessionError::Closed(CloseError::EventsLost)),
            "{error:?}"
        );
        assert!(
            matches!(connecting, SessionUpdate::Connecting { attempt: 1 }),
            "{connecting:?}"
        );
        assert_eq!(last_seq, None);
        let SessionUpdate::Connected(Catchup::Snapshot(snapshot)) = connected else {
            panic!("expected a snapshot, got {connected:?}");
        };
        assert_eq!(snapshot, test_daemon::snapshot(Some(flood)));
    }
}

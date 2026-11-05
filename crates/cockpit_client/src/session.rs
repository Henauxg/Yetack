//! A connection that comes back after a drop.

use std::time::Duration;

use cockpit_protocol::{Catchup, EventRecord, Request, ResponseBody, Seq};
use tokio::sync::mpsc;

use crate::connection::Connection;
use crate::error::{CloseError, ConnectError, RequestError};
use crate::requester::{Command, Requester};
use crate::transport::Transport;

/// Updates the session holds before it stops reading from the daemon.
const UPDATE_BUFFER: usize = 256;

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
/// A task owns the current connection; this handle talks to it over channels. Dropping the
/// session ends the task and closes the connection.
pub struct Session {
    requester: Requester,
    updates: mpsc::Receiver<SessionUpdate>,
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
        let (updates_sender, updates) = mpsc::channel(UPDATE_BUFFER);
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
    pub async fn next_update(&mut self) -> Option<SessionUpdate> {
        self.updates.recv().await
    }
}

/// The session's task. It returns when the `Session` is dropped.
async fn run(
    transport: Transport,
    backoff: Backoff,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: mpsc::Sender<SessionUpdate>,
) {
    let mut last_seq: Option<Seq> = None;
    let mut delay = backoff.initial;
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        if updates
            .send(SessionUpdate::Connecting { attempt })
            .await
            .is_err()
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
                if updates
                    .send(SessionUpdate::Connected(catchup))
                    .await
                    .is_err()
                {
                    return;
                }
                let Some(error) = serve(connection, &mut commands, &updates, &mut last_seq).await
                else {
                    return;
                };
                SessionError::Closed(error)
            }
            Err(error) => SessionError::Connect(error),
        };
        let disconnected = SessionUpdate::Disconnected {
            error,
            retry_in: delay,
        };
        if updates.send(disconnected).await.is_err() {
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

/// Runs `future` and refuses the requests that come meanwhile, since no connection carries them.
/// Returns `None` when the `Session` is dropped.
async fn while_disconnected<F: Future>(
    future: F,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    updates: &mpsc::Sender<SessionUpdate>,
) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    loop {
        tokio::select! {
            output = &mut future => return Some(output),
            command = commands.recv() => command?.answer(Err(RequestError::NotConnected)),
            () = updates.closed() => return None,
        }
    }
}

/// Passes events to the session's user and requests to the connection, until the connection
/// closes. Returns `None` when the `Session` is dropped.
async fn serve(
    mut connection: Connection,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    updates: &mpsc::Sender<SessionUpdate>,
    last_seq: &mut Option<Seq>,
) -> Option<CloseError> {
    let requester = connection.requester().clone();
    loop {
        tokio::select! {
            event = connection.next_event() => match event {
                Ok(record) => {
                    *last_seq = Some(record.seq);
                    updates.send(SessionUpdate::Event(record)).await.ok()?;
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
    use crate::test_daemon::TestDaemon;

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
}

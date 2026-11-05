//! One connection to a daemon, from `Hello` until it closes.

use std::collections::HashMap;
use std::time::Duration;

use cockpit_protocol::{
    Catchup, ClientMessage, EventRecord, Frame, FrameError, FrameReader, FrameWriter,
    PROTOCOL_VERSION, Request, RequestId, ResponseBody, Seq, ServerMessage,
};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::error::{CloseError, ConnectError, RequestError};
use crate::lossy_channel::{LossyReceiver, LossySender, Received, Sent};
use crate::requester::{Command, Requester};
use crate::transport::{SshProcess, Transport};
use crate::{lossy_channel, transport};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// The events that wait for `next_event`, enough for a burst such as the replay after a reconnect.
/// When an event finds the queue full, the waiting events are dropped, so that reading the
/// connection never waits for the app.
pub(crate) const EVENT_QUEUE: usize = 4096;

/// A connection that passed the handshake.
///
/// A task owns the transport and the requests waiting for a response; this handle talks to it over
/// channels. Dropping the connection closes the transport and kills ssh.
pub struct Connection {
    requester: Requester,
    events: LossyReceiver<EventRecord>,
    /// Why the connection closed; taken by the first `next_event` that returns it.
    close: Option<oneshot::Receiver<CloseError>>,
}

type Reply = oneshot::Sender<Result<ResponseBody, RequestError>>;

impl Connection {
    /// Opens the transport, sends `Hello` with `last_seq`, and waits for the daemon's `Welcome`.
    ///
    /// # Panics
    ///
    /// Outside a Tokio runtime, since it spawns the connection's task.
    pub async fn connect(
        transport: &Transport,
        last_seq: Option<Seq>,
    ) -> Result<(Self, Catchup), ConnectError> {
        match transport {
            Transport::Local { socket } => {
                let (read, write) = transport::connect_socket(socket).await?.into_split();
                establish(read, write, None, last_seq).await
            }
            Transport::Ssh {
                host,
                remote_command,
            } => {
                let ssh = transport::spawn_ssh(host, remote_command)?;
                establish(ssh.stdout, ssh.stdin, Some(ssh.process), last_seq).await
            }
        }
    }

    #[must_use]
    pub fn requester(&self) -> &Requester {
        &self.requester
    }

    pub async fn request(&self, request: Request) -> Result<ResponseBody, RequestError> {
        self.requester.request(request).await
    }

    /// The next event.
    ///
    /// `CloseError::EventsLost` means the events that waited were dropped, and the connection goes
    /// on: the caller is out of date until it reconnects without a last `Seq`. Any other error
    /// means the connection closed, and every request in flight failed with
    /// `RequestError::Disconnected`.
    ///
    /// It is cancel safe, so it can wait in a `tokio::select!`.
    pub async fn next_event(&mut self) -> Result<EventRecord, CloseError> {
        match self.events.recv().await {
            Some(Received::Item(record)) => return Ok(record),
            Some(Received::Lost) => return Err(CloseError::EventsLost),
            None => {}
        }
        let Some(close) = &mut self.close else {
            return Err(CloseError::AlreadyClosed);
        };
        // The task ends without a reason only when the runtime shuts down.
        let reason = close.await.unwrap_or(CloseError::Closed);
        self.close = None;
        Err(reason)
    }
}

async fn establish<R, W>(
    read: R,
    write: W,
    ssh: Option<SshProcess>,
    last_seq: Option<Seq>,
) -> Result<(Connection, Catchup), ConnectError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = FrameReader::new(BufReader::new(read));
    let mut writer = FrameWriter::new(write);
    let handshake = handshake(&mut reader, &mut writer, last_seq);
    let catchup = match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok(catchup)) => catchup,
        Ok(Err(error)) => return Err(explain_with_ssh(error, ssh).await),
        Err(_elapsed) => return Err(ConnectError::Timeout),
    };
    let (commands_sender, commands) = mpsc::unbounded_channel();
    let (events_sender, events) = lossy_channel::channel(EVENT_QUEUE);
    let (close_sender, close) = oneshot::channel();
    let ends = Ends {
        events: events_sender,
        close: close_sender,
    };
    tokio::spawn(run(reader, writer, commands, ends, ssh));
    let connection = Connection {
        requester: Requester::new(commands_sender),
        events,
        close: Some(close),
    };
    Ok((connection, catchup))
}

async fn handshake<R, W>(
    reader: &mut FrameReader<R>,
    writer: &mut FrameWriter<W>,
    last_seq: Option<Seq>,
) -> Result<Catchup, ConnectError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        last_seq,
    };
    writer.send(&hello).await?;
    match reader.next_message().await? {
        Some(ServerMessage::Welcome {
            protocol_version,
            catchup,
        }) => {
            if protocol_version == PROTOCOL_VERSION {
                Ok(catchup)
            } else {
                Err(ConnectError::VersionMismatch {
                    daemon: protocol_version,
                })
            }
        }
        Some(ServerMessage::Refused { reason }) => Err(ConnectError::Refused { reason }),
        Some(ServerMessage::Response { .. } | ServerMessage::Event(_)) => {
            Err(ConnectError::Unexpected)
        }
        None => Err(ConnectError::Closed),
    }
}

/// When ssh ended the connection, what it wrote to stderr says why.
async fn explain_with_ssh(error: ConnectError, ssh: Option<SshProcess>) -> ConnectError {
    let Some(ssh) = ssh else {
        return error;
    };
    if !matches!(error, ConnectError::Closed | ConnectError::Frame(_)) {
        return error;
    }
    match ssh.stderr_tail().await {
        Some(stderr) => ConnectError::Ssh { stderr },
        None => error,
    }
}

/// Where the connection's task sends events, and why it closed.
struct Ends {
    events: LossySender<EventRecord>,
    close: oneshot::Sender<CloseError>,
}

/// Owns the connection after the handshake: reads responses and events, sends requests, and
/// matches each response to its request.
async fn run<R, W>(
    mut reader: FrameReader<BufReader<R>>,
    writer: FrameWriter<W>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    ends: Ends,
    ssh: Option<SshProcess>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // The writer has its own task so that reading goes on while a large request goes out.
    // Otherwise the client and the daemon could each wait forever for the other to read.
    let (frames, frames_to_write) = mpsc::unbounded_channel();
    let (write_failure_sender, mut write_failure) = oneshot::channel();
    let writer_task = tokio::spawn(write_frames(writer, frames_to_write, write_failure_sender));

    let mut pending: HashMap<RequestId, Reply> = HashMap::new();
    let mut next_id = RequestId::new(1);
    let close = loop {
        tokio::select! {
            message = reader.next_message::<ServerMessage>() => match message {
                Ok(Some(message)) => {
                    if !dispatch(message, &mut pending, &ends.events) {
                        break None;
                    }
                }
                Ok(None) => break Some(CloseError::Closed),
                Err(FrameError::Decode { request_id, source }) => {
                    fail_undecodable(request_id, source, &mut pending);
                }
                Err(error) => break Some(CloseError::Frame(error)),
            },
            command = commands.recv() => {
                let Some(Command { request, reply }) = command else {
                    break None;
                };
                let id = next_id;
                next_id = next_id.next();
                match Frame::encode(&ClientMessage::Request { id, request }) {
                    Ok(frame) => {
                        pending.insert(id, reply);
                        // When the writer has stopped, `write_failure` ends the loop.
                        let _ = frames.send(frame);
                    }
                    Err(error) => {
                        let _ = reply.send(Err(RequestError::Encode(error)));
                    }
                }
            }
            failure = &mut write_failure => {
                break Some(match failure {
                    Ok(error) => CloseError::Frame(error),
                    Err(_) => CloseError::Closed,
                });
            }
            () = ends.events.closed() => break None,
        }
    };

    writer_task.abort();
    for (_, reply) in pending.drain() {
        let _ = reply.send(Err(RequestError::Disconnected));
    }
    if let Some(error) = close {
        // The connection may be gone already.
        let _ = ends.close.send(error);
    }
    // Kills ssh.
    drop(ssh);
}

/// Hands a message to whoever waits for it, without waiting. Returns `false` when the
/// `Connection` is gone.
fn dispatch(
    message: ServerMessage,
    pending: &mut HashMap<RequestId, Reply>,
    events: &LossySender<EventRecord>,
) -> bool {
    match message {
        ServerMessage::Response { id, result } => {
            match pending.remove(&id) {
                Some(reply) => {
                    let _ = reply.send(result.into_result().map_err(RequestError::Failed));
                }
                None => tracing::warn!(?id, "the daemon answered a request that is not pending"),
            }
            true
        }
        ServerMessage::Event(record) => events.send_or_drop(record) != Sent::Closed,
        ServerMessage::Welcome { .. } | ServerMessage::Refused { .. } => {
            tracing::warn!("the daemon sent a second handshake message");
            true
        }
    }
}

/// A line that does not decode fails its request when it names one. An event that does not
/// decode is skipped: closing would replay it on the next connection.
fn fail_undecodable(
    request_id: Option<RequestId>,
    source: serde_json::Error,
    pending: &mut HashMap<RequestId, Reply>,
) {
    match request_id.and_then(|id| pending.remove(&id)) {
        Some(reply) => {
            let _ = reply.send(Err(RequestError::InvalidResponse(source)));
        }
        None => tracing::warn!(%source, "skipped a message that does not decode"),
    }
}

async fn write_frames<W: AsyncWrite + Unpin>(
    mut writer: FrameWriter<W>,
    mut frames: mpsc::UnboundedReceiver<Frame>,
    failure: oneshot::Sender<FrameError>,
) {
    while let Some(frame) = frames.recv().await {
        if let Err(error) = writer.send_frame(&frame).await {
            let _ = failure.send(error);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentId, AgentParams, ErrorBody, ErrorCode, Event, ResponseResult};

    use super::*;
    use crate::test_daemon;
    use crate::test_daemon::{Peer, TestDaemon, in_time};

    fn interrupt() -> Request {
        Request::AgentInterrupt(AgentParams {
            agent: AgentId::new(3),
        })
    }

    fn stop() -> Request {
        Request::AgentStop(AgentParams {
            agent: AgentId::new(3),
        })
    }

    /// A body that names the request it answers.
    fn echo(request: &Request) -> ResponseBody {
        ResponseBody::File {
            path: None,
            text: request.method().to_string(),
        }
    }

    fn removed(agent: u32) -> Event {
        Event::AgentRemoved {
            agent: AgentId::new(agent),
        }
    }

    /// Connects a client to `daemon`, which welcomes it with a replay.
    async fn connect(daemon: &TestDaemon) -> (Connection, Peer) {
        let transport = daemon.transport();
        let (connected, (peer, _)) = tokio::join!(
            Connection::connect(&transport, None),
            daemon.accept_and_welcome(Catchup::Replay),
        );
        let (connection, _) = connected.unwrap();
        (connection, peer)
    }

    #[tokio::test]
    async fn hello_carries_the_last_seq_and_welcome_gives_the_catchup() {
        let daemon = TestDaemon::new();

        let transport = daemon.transport();

        let (connected, (_peer, last_seq)) = tokio::join!(
            Connection::connect(&transport, Some(Seq::new(5))),
            daemon.accept_and_welcome(Catchup::Snapshot(test_daemon::snapshot(Some(9)))),
        );

        let (_connection, catchup) = connected.unwrap();
        assert_eq!(last_seq, Some(Seq::new(5)));
        assert_eq!(catchup, Catchup::Snapshot(test_daemon::snapshot(Some(9))));
    }

    #[tokio::test]
    async fn a_refused_hello_fails_with_the_reason() {
        let daemon = TestDaemon::new();
        let transport = daemon.transport();
        let refuse = async {
            let mut peer = daemon.accept().await;
            peer.hello().await;
            peer.send(&ServerMessage::Refused {
                reason: "the daemon speaks protocol 2".to_owned(),
            })
            .await;
            peer
        };

        let (connected, _peer) = tokio::join!(Connection::connect(&transport, None), refuse);

        let Err(ConnectError::Refused { reason }) = connected else {
            panic!("expected a refusal");
        };
        assert_eq!(reason, "the daemon speaks protocol 2");
    }

    #[tokio::test]
    async fn a_welcome_in_another_version_fails() {
        let daemon = TestDaemon::new();
        let transport = daemon.transport();
        let welcome = async {
            let mut peer = daemon.accept().await;
            peer.hello().await;
            peer.send(&ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION + 1,
                catchup: Catchup::Replay,
            })
            .await;
            peer
        };

        let (connected, _peer) = tokio::join!(Connection::connect(&transport, None), welcome);

        assert!(matches!(
            connected,
            Err(ConnectError::VersionMismatch { daemon }) if daemon == PROTOCOL_VERSION + 1
        ));
    }

    #[tokio::test]
    async fn a_daemon_that_closes_before_answering_fails_the_connect() {
        let daemon = TestDaemon::new();
        let transport = daemon.transport();
        let close = async {
            let mut peer = daemon.accept().await;
            peer.hello().await;
        };

        let (connected, ()) = tokio::join!(Connection::connect(&transport, None), close);

        assert!(matches!(connected, Err(ConnectError::Closed)));
    }

    #[tokio::test]
    async fn a_missing_socket_fails_the_connect() {
        let daemon = TestDaemon::new();
        let transport = Transport::Local {
            socket: daemon.missing_socket(),
        };

        let connected = Connection::connect(&transport, None).await;

        assert!(matches!(connected, Err(ConnectError::Socket { .. })));
    }

    #[tokio::test]
    async fn a_response_matches_its_request_across_events() {
        let daemon = TestDaemon::new();
        let (mut connection, mut peer) = connect(&daemon).await;
        let answer = async {
            let (id, request) = peer.request().await;
            peer.event(6, removed(1)).await;
            peer.respond(id, ResponseBody::Done).await;
            peer.event(7, removed(2)).await;
            request
        };

        let (response, request) = tokio::join!(connection.request(interrupt()), answer);

        assert_eq!(request, interrupt());
        assert_eq!(response.unwrap(), ResponseBody::Done);
        let first = connection.next_event().await.unwrap();
        let second = connection.next_event().await.unwrap();
        assert_eq!((first.seq, first.event), (Seq::new(6), removed(1)));
        assert_eq!((second.seq, second.event), (Seq::new(7), removed(2)));
    }

    #[tokio::test]
    async fn responses_out_of_order_reach_their_requests() {
        let daemon = TestDaemon::new();
        let (connection, mut peer) = connect(&daemon).await;
        let answer_backwards = async {
            let (first_id, first) = peer.request().await;
            let (second_id, second) = peer.request().await;
            peer.respond(second_id, echo(&second)).await;
            peer.respond(first_id, echo(&first)).await;
        };

        let (interrupted, stopped, ()) = tokio::join!(
            connection.request(interrupt()),
            connection.request(stop()),
            answer_backwards,
        );

        assert_eq!(interrupted.unwrap(), echo(&interrupt()));
        assert_eq!(stopped.unwrap(), echo(&stop()));
    }

    #[tokio::test]
    async fn an_error_response_fails_the_request() {
        let daemon = TestDaemon::new();
        let (connection, mut peer) = connect(&daemon).await;
        let error = ErrorBody::new(ErrorCode::NotFound, "no agent 3".to_owned());
        let refuse = async {
            let (id, _) = peer.request().await;
            peer.send(&ServerMessage::Response {
                id,
                result: ResponseResult::Error(error.clone()),
            })
            .await;
        };

        let (response, ()) = tokio::join!(connection.request(interrupt()), refuse);

        let Err(RequestError::Failed(body)) = response else {
            panic!("expected a failed request, got {response:?}");
        };
        assert_eq!(body, error);
    }

    #[tokio::test]
    async fn an_undecodable_response_fails_only_its_request() {
        let daemon = TestDaemon::new();
        let (connection, mut peer) = connect(&daemon).await;
        let answer_badly = async {
            let (id, _) = peer.request().await;
            let line = format!(
                r#"{{"type":"response","id":{},"result":{{"ok":{{"type":"surprise"}}}}}}"#,
                id.get()
            );
            peer.send_line(&line).await;
        };

        let (response, ()) = tokio::join!(connection.request(interrupt()), answer_badly);
        let answer_well = async {
            let (id, _) = peer.request().await;
            peer.respond(id, ResponseBody::Done).await;
        };
        let (next_response, ()) = tokio::join!(connection.request(stop()), answer_well);

        assert!(
            matches!(response, Err(RequestError::InvalidResponse(_))),
            "{response:?}"
        );
        assert_eq!(next_response.unwrap(), ResponseBody::Done);
    }

    #[tokio::test]
    async fn an_undecodable_event_is_skipped() {
        let daemon = TestDaemon::new();
        let (mut connection, mut peer) = connect(&daemon).await;

        peer.send_line(r#"{"type":"event","seq":6,"event":{"type":"surprise"}}"#)
            .await;
        peer.event(7, removed(2)).await;
        let next = connection.next_event().await.unwrap();

        assert_eq!((next.seq, next.event), (Seq::new(7), removed(2)));
    }

    /// Reads a request, sends more events than the connection queues, then answers the request.
    async fn flood_then_answer(peer: &mut Peer) {
        let (id, _) = peer.request().await;
        for seq in 1..=test_daemon::more_events_than_a_queue_holds() {
            peer.event(seq, removed(1)).await;
        }
        peer.respond(id, ResponseBody::Done).await;
    }

    #[tokio::test]
    async fn a_response_after_more_events_than_the_queue_holds_reaches_its_request() {
        let daemon = TestDaemon::new();
        let (connection, mut peer) = connect(&daemon).await;

        let (response, ()) = in_time(async {
            tokio::join!(
                connection.request(interrupt()),
                flood_then_answer(&mut peer)
            )
        })
        .await;

        assert_eq!(response.unwrap(), ResponseBody::Done);
    }

    #[tokio::test]
    async fn a_full_queue_drops_its_events_and_the_connection_goes_on() {
        let daemon = TestDaemon::new();
        let (mut connection, mut peer) = connect(&daemon).await;
        // The response comes last, so every event was read once the request completes.
        let (flooded, ()) = in_time(async {
            tokio::join!(
                connection.request(interrupt()),
                flood_then_answer(&mut peer)
            )
        })
        .await;
        flooded.unwrap();

        let lost = in_time(connection.next_event()).await;
        peer.event(9000, removed(2)).await;
        let next = in_time(connection.next_event()).await.unwrap();
        let answer = async {
            let (id, _) = peer.request().await;
            peer.respond(id, ResponseBody::Done).await;
        };
        let (response, ()) =
            in_time(async { tokio::join!(connection.request(stop()), answer) }).await;

        assert!(matches!(lost, Err(CloseError::EventsLost)), "{lost:?}");
        assert_eq!((next.seq, next.event), (Seq::new(9000), removed(2)));
        assert_eq!(response.unwrap(), ResponseBody::Done);
    }

    #[tokio::test]
    async fn a_dropped_connection_fails_the_requests_in_flight() {
        let daemon = TestDaemon::new();
        let (mut connection, mut peer) = connect(&daemon).await;
        let drop_after_reading = async move {
            peer.request().await;
            drop(peer);
        };

        let (response, ()) = tokio::join!(connection.request(interrupt()), drop_after_reading);

        assert!(
            matches!(response, Err(RequestError::Disconnected)),
            "{response:?}"
        );
        assert!(matches!(
            connection.next_event().await,
            Err(CloseError::Closed)
        ));
        assert!(matches!(
            connection.next_event().await,
            Err(CloseError::AlreadyClosed)
        ));
        let late = connection.request(stop()).await;
        assert!(matches!(late, Err(RequestError::NotConnected)), "{late:?}");
    }

    #[tokio::test]
    async fn dropping_the_connection_closes_the_transport() {
        let daemon = TestDaemon::new();
        let (connection, mut peer) = connect(&daemon).await;

        drop(connection);

        assert_eq!(peer.next().await, None);
    }
}

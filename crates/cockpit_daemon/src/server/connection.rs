//! One client's session: `Hello`, `Welcome` with its catch-up, then requests, responses and live
//! events.

use std::io;
use std::time::Duration;

use cockpit_protocol::{
    Catchup, ClientMessage, ErrorBody, ErrorCode, Frame, FrameError, FrameReader, PROTOCOL_VERSION,
    ReleaseVersion, RequestId, ResponseResult, Seq, ServerMessage, Snapshot,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use crate::build_commit::{RELEASE_VERSION, build_commit};
use crate::hub::{HubInput, Joined, Registration};
use crate::store::{ReplayRead, read_replay};

/// How long a client has to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one write may wait on a client that does not read.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Requests waiting for an answer; past this the connection stops reading until one ends.
const MAX_IN_FLIGHT: usize = 64;
/// A replay larger than this costs more than a snapshot.
const MAX_REPLAY_BYTES: usize = 64 * 1024 * 1024;

/// Why a connection ended, for the log.
#[derive(Debug, thiserror::Error)]
enum End {
    #[error("the client closed the connection")]
    Closed,
    #[error("the client did not say hello in time")]
    NoHello,
    #[error("the client was refused")]
    Refused,
    /// The hub dropped the client, which read too slowly.
    #[error("the client reads too slowly")]
    TooSlow,
    #[error("a write to the client timed out")]
    WriteTimeout,
    #[error("the connection failed: {0}")]
    Io(io::Error),
    #[error("the connection failed: {0}")]
    Frame(FrameError),
    #[error("the daemon is stopping")]
    HubGone,
}

type Reader<R> = FrameReader<BufReader<R>>;

/// Serves one client over `read` and `writer`: a socket's two halves, or an in-memory stream's.
pub(crate) async fn serve<R, W>(read: R, mut writer: W, hub: mpsc::Sender<HubInput>)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = FrameReader::new(BufReader::new(read));
    match run(&mut reader, &mut writer, &hub).await {
        Ok(()) | Err(End::Closed) => tracing::debug!("a client left"),
        Err(end) => tracing::info!(%end, "closed a connection"),
    }
}

async fn run<R, W>(
    reader: &mut Reader<R>,
    writer: &mut W,
    hub: &mpsc::Sender<HubInput>,
) -> Result<(), End>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let last_seq = hello(reader, writer).await?;
    let Joining {
        catchup,
        replayed_through,
        replay,
        live,
    } = join(hub, last_seq).await?;
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        daemon_version: ReleaseVersion::new(RELEASE_VERSION.to_owned()),
        daemon_commit: build_commit(),
        catchup,
        replayed_through,
    };
    send(writer, &welcome).await?;
    for frame in replay {
        write(writer, &frame).await?;
    }
    serve_requests(reader, writer, hub, live).await
}

/// What a client gets after its hello: the welcome's catch-up, the replay's frames, then the live
/// events.
struct Joining {
    catchup: Catchup,
    /// The last event the replay covers. `None` with a snapshot.
    replayed_through: Option<Seq>,
    /// Empty with a snapshot.
    replay: Vec<Vec<u8>>,
    live: mpsc::Receiver<Frame>,
}

impl Joining {
    fn snapshot(snapshot: Snapshot, live: mpsc::Receiver<Frame>) -> Self {
        Self {
            catchup: Catchup::Snapshot(snapshot),
            replayed_through: None,
            replay: Vec::new(),
            live,
        }
    }
}

/// Reads the `Hello`, refusing a client of another protocol version.
async fn hello<R, W>(reader: &mut Reader<R>, writer: &mut W) -> Result<Option<Seq>, End>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let message = tokio::time::timeout(HELLO_TIMEOUT, reader.next_message::<ClientMessage>())
        .await
        .map_err(|_| End::NoHello)?;
    let reason = match message {
        Ok(Some(ClientMessage::Hello {
            protocol_version,
            last_seq,
        })) => {
            if protocol_version == PROTOCOL_VERSION {
                return Ok(last_seq);
            }
            format!("the daemon speaks protocol {PROTOCOL_VERSION}, the client {protocol_version}")
        }
        Ok(Some(ClientMessage::Request { .. })) | Err(FrameError::Decode { .. }) => {
            "the first message must be a hello".to_owned()
        }
        Ok(None) => return Err(End::Closed),
        Err(error) => return Err(End::Frame(error)),
    };
    let refused = ServerMessage::Refused {
        reason,
        daemon_version: Some(ReleaseVersion::new(RELEASE_VERSION.to_owned())),
        daemon_commit: build_commit(),
    };
    send(writer, &refused).await?;
    Err(End::Refused)
}

/// Registers with the hub and reads the replay from the logs. A replay too large to send becomes
/// a snapshot.
async fn join(hub: &mpsc::Sender<HubInput>, last_seq: Option<Seq>) -> Result<Joining, End> {
    let Registration { joined, frames } = register(hub, last_seq).await?;
    let (after, upto, events, details) = match joined {
        Joined::Snapshot(snapshot) => return Ok(Joining::snapshot(*snapshot, frames)),
        Joined::Replay {
            after,
            upto,
            events,
            details,
        } => (after, upto, events, details),
    };
    let read = tokio::task::spawn_blocking(move || {
        read_replay(&events, after, upto, details, MAX_REPLAY_BYTES)
    })
    .await;
    match read {
        Ok(Ok(ReplayRead::Frames(replay))) => {
            return Ok(Joining {
                catchup: Catchup::Replay,
                replayed_through: upto,
                replay,
                live: frames,
            });
        }
        Ok(Ok(ReplayRead::TooLarge)) => tracing::info!("a replay is too large: sending a snapshot"),
        Ok(Err(error)) => {
            tracing::warn!(error = %crate::error_chain::chain(&error), "cannot read a replay");
        }
        Err(error) => tracing::warn!(%error, "the replay reader failed"),
    }
    // Leaves the hub's clients, so no event reaches the client twice.
    drop(frames);
    let Registration { joined, frames } = register(hub, None).await?;
    match joined {
        Joined::Snapshot(snapshot) => Ok(Joining::snapshot(*snapshot, frames)),
        Joined::Replay { .. } => Err(End::HubGone),
    }
}

async fn register(
    hub: &mpsc::Sender<HubInput>,
    last_seq: Option<Seq>,
) -> Result<Registration, End> {
    let (reply, registration) = oneshot::channel();
    hub.send(HubInput::Connect { last_seq, reply })
        .await
        .map_err(|_| End::HubGone)?;
    registration.await.map_err(|_| End::HubGone)
}

async fn serve_requests<R, W>(
    reader: &mut Reader<R>,
    writer: &mut W,
    hub: &mpsc::Sender<HubInput>,
    mut frames: mpsc::Receiver<Frame>,
) -> Result<(), End>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut in_flight: JoinSet<(RequestId, ResponseResult)> = JoinSet::new();
    loop {
        tokio::select! {
            message = reader.next_message::<ClientMessage>(),
                if in_flight.len() < MAX_IN_FLIGHT =>
            {
                match message {
                    Ok(Some(ClientMessage::Request { id, request })) => {
                        let (reply, answer) = oneshot::channel();
                        hub.send(HubInput::Request { request, reply })
                            .await
                            .map_err(|_| End::HubGone)?;
                        in_flight.spawn(async move {
                            let result = answer.await.unwrap_or_else(|_| {
                                failure(
                                    ErrorCode::Internal,
                                    "the daemon dropped the request".to_owned(),
                                )
                            });
                            (id, result)
                        });
                    }
                    Ok(Some(ClientMessage::Hello { .. })) => {
                        tracing::warn!("ignored a second hello");
                    }
                    Ok(None) => return Ok(()),
                    Err(FrameError::Decode { request_id: Some(id), source }) => {
                        let result = failure(ErrorCode::InvalidRequest, source.to_string());
                        respond(writer, id, result).await?;
                    }
                    Err(FrameError::Decode { request_id: None, source }) => {
                        tracing::warn!(%source, "skipped a line that is not a message");
                    }
                    Err(error) => return Err(End::Frame(error)),
                }
            }
            frame = frames.recv() => match frame {
                Some(frame) => write(writer, frame.as_bytes()).await?,
                None => return Err(End::TooSlow),
            },
            Some(done) = in_flight.join_next() => match done {
                Ok((id, result)) => respond(writer, id, result).await?,
                Err(error) => tracing::warn!(%error, "a request's task failed"),
            },
        }
    }
}

/// Sends a response, or an error in its place when it is too large to send.
async fn respond<W: AsyncWrite + Unpin>(
    writer: &mut W,
    id: RequestId,
    result: ResponseResult,
) -> Result<(), End> {
    let response = ServerMessage::Response { id, result };
    match Frame::encode(&response) {
        Ok(frame) => write(writer, frame.as_bytes()).await,
        Err(error) => {
            let result = failure(
                ErrorCode::Internal,
                format!("the response cannot be sent: {error}"),
            );
            send(writer, &ServerMessage::Response { id, result }).await
        }
    }
}

async fn send<W: AsyncWrite + Unpin>(writer: &mut W, message: &ServerMessage) -> Result<(), End> {
    let frame = Frame::encode(message).map_err(End::Frame)?;
    write(writer, frame.as_bytes()).await
}

async fn write<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> Result<(), End> {
    use tokio::io::AsyncWriteExt;

    tokio::time::timeout(WRITE_TIMEOUT, writer.write_all(bytes))
        .await
        .map_err(|_| End::WriteTimeout)?
        .map_err(End::Io)
}

fn failure(code: ErrorCode, message: String) -> ResponseResult {
    ResponseResult::Error(ErrorBody::new(code, message))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use cockpit_protocol::{Event, EventRecord, FrameWriter, Request, Settings};
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;
    use crate::daemon::Daemon;
    use crate::daemon_config::DaemonConfig;

    const WAIT: Duration = Duration::from_secs(10);

    /// A daemon on a new state folder in `temp`, which runs no agent.
    fn start(temp: &Path) -> Daemon {
        Daemon::prepare(DaemonConfig {
            state_dir: temp.join("state"),
            claude_program: temp.join("no-claude"),
            claude_config_dir: temp.join("claude"),
        })
        .unwrap()
        .start()
    }

    /// The client's end of one connection.
    struct Client {
        reader: FrameReader<BufReader<ReadHalf<DuplexStream>>>,
        writer: FrameWriter<WriteHalf<DuplexStream>>,
        requests: u32,
    }

    impl Client {
        /// Connects to `hub` with `last_seq` in the hello, and returns the client and the welcome.
        async fn join(
            hub: &mpsc::Sender<HubInput>,
            last_seq: Option<Seq>,
        ) -> (Self, ServerMessage) {
            let (ours, theirs) = tokio::io::duplex(64 * 1024);
            let (read, write) = tokio::io::split(theirs);
            tokio::spawn(serve(read, write, hub.clone()));
            let (read, write) = tokio::io::split(ours);
            let mut client = Self {
                reader: FrameReader::new(BufReader::new(read)),
                writer: FrameWriter::new(write),
                requests: 0,
            };
            let hello = ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                last_seq,
            };
            client.writer.send(&hello).await.unwrap();
            let welcome = client.next().await;
            (client, welcome)
        }

        async fn next(&mut self) -> ServerMessage {
            tokio::time::timeout(WAIT, self.reader.next_message())
                .await
                .expect("a message in time")
                .unwrap()
                .expect("the connection is open")
        }

        /// The next event, past the responses.
        async fn next_event(&mut self) -> EventRecord {
            loop {
                if let ServerMessage::Event(record) = self.next().await {
                    return record;
                }
            }
        }

        /// Hides the MCP server `server`, and returns the number of the event that says so.
        async fn hide(&mut self, server: &str) -> Seq {
            self.requests += 1;
            let settings = Settings {
                hidden_mcp_servers: BTreeSet::from([server.to_owned()]),
            };
            let request = ClientMessage::Request {
                id: RequestId::new(self.requests),
                request: Request::SettingsSet(settings.clone()),
            };
            self.writer.send(&request).await.unwrap();
            let record = self.next_event().await;
            assert_eq!(record.event, Event::SettingsChanged(settings));
            record.seq
        }
    }

    fn replayed_through(welcome: &ServerMessage) -> Option<Seq> {
        let ServerMessage::Welcome {
            replayed_through, ..
        } = welcome
        else {
            panic!("expected a welcome, got {welcome:?}");
        };
        *replayed_through
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_replays_welcome_names_the_daemons_last_event() {
        let temp = tempfile::tempdir().unwrap();
        let daemon = start(temp.path());
        let hub = daemon.inputs();
        let (mut watcher, first_welcome) = Client::join(hub, None).await;
        let first = watcher.hide("notes").await;
        let second = watcher.hide("search").await;

        let (mut behind, behind_welcome) = Client::join(hub, Some(first)).await;
        let (mut current, current_welcome) = Client::join(hub, Some(second)).await;
        let live = watcher.hide("issues").await;

        assert!(matches!(
            first_welcome,
            ServerMessage::Welcome {
                catchup: Catchup::Snapshot(_),
                replayed_through: None,
                ..
            }
        ));
        assert!(matches!(
            behind_welcome,
            ServerMessage::Welcome {
                catchup: Catchup::Replay,
                ..
            }
        ));
        assert_eq!(replayed_through(&behind_welcome), Some(second));
        assert_eq!(behind.next_event().await.seq, second, "the replay");
        assert_eq!(behind.next_event().await.seq, live, "a live event");
        assert_eq!(
            replayed_through(&current_welcome),
            Some(second),
            "an empty replay"
        );
        assert_eq!(current.next_event().await.seq, live);
        daemon.stop_hub();
    }
}

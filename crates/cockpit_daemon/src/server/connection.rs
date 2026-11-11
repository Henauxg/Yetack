//! One client's session: `Hello`, `Welcome` with its catch-up, then requests, responses and live
//! events.

use std::io;
use std::time::Duration;

use cockpit_protocol::{
    Catchup, ClientMessage, ErrorBody, ErrorCode, Frame, FrameError, FrameReader, PROTOCOL_VERSION,
    ReleaseVersion, RequestId, ResponseResult, Seq, ServerMessage,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

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
    let (frames, replay, catchup) = join(hub, last_seq).await?;
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        daemon_version: ReleaseVersion::new(env!("CARGO_PKG_VERSION").to_owned()),
        catchup,
    };
    send(writer, &welcome).await?;
    for frame in replay {
        write(writer, &frame).await?;
    }
    serve_requests(reader, writer, hub, frames).await
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
    send(writer, &ServerMessage::Refused { reason }).await?;
    Err(End::Refused)
}

/// Registers with the hub and reads the replay from the logs. A replay too large to send becomes
/// a snapshot.
async fn join(
    hub: &mpsc::Sender<HubInput>,
    last_seq: Option<Seq>,
) -> Result<(mpsc::Receiver<Frame>, Vec<Vec<u8>>, Catchup), End> {
    let Registration { joined, frames } = register(hub, last_seq).await?;
    let (after, upto, events) = match joined {
        Joined::Snapshot(snapshot) => {
            return Ok((frames, Vec::new(), Catchup::Snapshot(*snapshot)));
        }
        Joined::Replay {
            after,
            upto,
            events,
        } => (after, upto, events),
    };
    let read =
        tokio::task::spawn_blocking(move || read_replay(&events, after, upto, MAX_REPLAY_BYTES))
            .await;
    match read {
        Ok(Ok(ReplayRead::Frames(replay))) => return Ok((frames, replay, Catchup::Replay)),
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
        Joined::Snapshot(snapshot) => Ok((frames, Vec::new(), Catchup::Snapshot(*snapshot))),
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

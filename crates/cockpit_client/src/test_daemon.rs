//! A daemon for tests: a Unix socket in a temporary folder, driven step by step by the test.

use std::path::PathBuf;

use cockpit_protocol::{
    Catchup, ClientMessage, Event, EventRecord, Frame, FrameReader, PROTOCOL_VERSION, PlanUsage,
    Request, RequestId, ResponseBody, ResponseResult, Seq, ServerMessage, Settings, Snapshot,
};
use tempfile::TempDir;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::transport::Transport;

pub(crate) struct TestDaemon {
    folder: TempDir,
    socket: PathBuf,
    listener: UnixListener,
}

/// The daemon's end of one connection.
pub(crate) struct Peer {
    reader: FrameReader<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

pub(crate) fn snapshot(last_seq: Option<u64>) -> Snapshot {
    Snapshot {
        agents: Vec::new(),
        plan_usage: PlanUsage::default(),
        settings: Settings::default(),
        last_seq: last_seq.map(Seq::new),
    }
}

impl TestDaemon {
    pub(crate) fn new() -> Self {
        let folder = tempfile::tempdir().unwrap();
        let socket = folder.path().join("agents-cockpit.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        Self {
            folder,
            socket,
            listener,
        }
    }

    pub(crate) fn transport(&self) -> Transport {
        Transport::Local {
            socket: self.socket.clone(),
        }
    }

    /// A socket path where nothing listens.
    pub(crate) fn missing_socket(&self) -> PathBuf {
        self.folder.path().join("missing.sock")
    }

    pub(crate) async fn accept(&self) -> Peer {
        let (stream, _) = self.listener.accept().await.unwrap();
        let (read, write) = stream.into_split();
        Peer {
            reader: FrameReader::new(BufReader::new(read)),
            writer: write,
        }
    }

    /// Accepts a connection, reads its `Hello` and welcomes it. Returns the hello's `last_seq`.
    pub(crate) async fn accept_and_welcome(&self, catchup: Catchup) -> (Peer, Option<Seq>) {
        let mut peer = self.accept().await;
        let last_seq = peer.hello().await;
        peer.send(&ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            catchup,
        })
        .await;
        (peer, last_seq)
    }
}

impl Peer {
    /// The next message, or `None` when the client closed the connection.
    pub(crate) async fn next(&mut self) -> Option<ClientMessage> {
        self.reader.next_message().await.unwrap()
    }

    /// Reads a `Hello` and returns its `last_seq`.
    pub(crate) async fn hello(&mut self) -> Option<Seq> {
        match self.next().await {
            Some(ClientMessage::Hello {
                protocol_version,
                last_seq,
            }) => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                last_seq
            }
            other => panic!("expected a hello, got {other:?}"),
        }
    }

    pub(crate) async fn request(&mut self) -> (RequestId, Request) {
        match self.next().await {
            Some(ClientMessage::Request { id, request }) => (id, request),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    pub(crate) async fn send(&mut self, message: &ServerMessage) {
        let frame = Frame::encode(message).unwrap();
        self.writer.write_all(frame.as_bytes()).await.unwrap();
    }

    pub(crate) async fn respond(&mut self, id: RequestId, body: ResponseBody) {
        let response = ServerMessage::Response {
            id,
            result: ResponseResult::Ok(body),
        };
        self.send(&response).await;
    }

    pub(crate) async fn event(&mut self, seq: u64, event: Event) {
        let record = EventRecord {
            seq: Seq::new(seq),
            event,
        };
        self.send(&ServerMessage::Event(record)).await;
    }

    /// Writes `line` and a newline as they are, for a line the protocol types cannot produce.
    pub(crate) async fn send_line(&mut self, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.unwrap();
    }
}

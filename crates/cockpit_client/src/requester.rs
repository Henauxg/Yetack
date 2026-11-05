use cockpit_protocol::{Request, ResponseBody};
use tokio::sync::{mpsc, oneshot};

use crate::error::RequestError;

/// Sends requests on a connection or a session, from any task.
///
/// A request fails with `RequestError::NotConnected` once the connection is closed: it is never
/// queued for a later connection, so a prompt is never sent twice.
#[derive(Debug, Clone)]
pub struct Requester {
    commands: mpsc::UnboundedSender<Command>,
}

/// A request, and where its result goes.
pub(crate) struct Command {
    pub(crate) request: Request,
    pub(crate) reply: oneshot::Sender<Result<ResponseBody, RequestError>>,
}

impl Command {
    pub(crate) fn answer(self, result: Result<ResponseBody, RequestError>) {
        // The caller may have stopped waiting.
        let _ = self.reply.send(result);
    }
}

impl Requester {
    pub(crate) fn new(commands: mpsc::UnboundedSender<Command>) -> Self {
        Self { commands }
    }

    pub async fn request(&self, request: Request) -> Result<ResponseBody, RequestError> {
        let (reply, response) = oneshot::channel();
        self.forward(Command { request, reply });
        // The command is dropped without an answer only when it was never sent.
        response.await.unwrap_or(Err(RequestError::NotConnected))
    }

    pub(crate) fn forward(&self, command: Command) {
        if let Err(unsent) = self.commands.send(command) {
            unsent.0.answer(Err(RequestError::NotConnected));
        }
    }
}

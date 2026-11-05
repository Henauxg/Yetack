use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::hub::HubInput;
use crate::server::peer::DaemonIdentity;
use crate::server::{connection, peer};

/// Only the owner may connect: on Linux, connecting needs write permission on the socket.
const SOCKET_MODE: u32 = 0o600;
/// The wait after a failed accept, such as when the process runs out of file descriptors.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Binds the socket, replacing a file a crashed daemon left. The caller holds the lock, so no
/// other daemon uses that file.
pub(crate) fn bind(path: &Path) -> io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!(path = %path.display(), "removed a leftover socket"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, Permissions::from_mode(SOCKET_MODE))?;
    Ok(listener)
}

/// Accepts connections forever. Each peer is checked before any of its bytes is read.
pub(crate) async fn accept_loop(
    listener: UnixListener,
    hub: mpsc::Sender<HubInput>,
    daemon: DaemonIdentity,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => match peer::check(&stream, daemon) {
                Ok(()) => {
                    tokio::spawn(connection::serve(stream, hub.clone()));
                }
                Err(error) => {
                    tracing::warn!(error = %crate::error_chain::chain(&error), "refused a connection");
                }
            },
            Err(error) => {
                tracing::warn!(%error, "cannot accept a connection");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
    }
}

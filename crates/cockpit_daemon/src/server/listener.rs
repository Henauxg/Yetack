use std::fs::Permissions;
use std::io;
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
///
/// The socket starts with the mode the umask leaves, until it gets [`SOCKET_MODE`]: `path` must
/// be in a folder only the owner may enter.
pub(crate) fn bind(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

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
                    let (read, write) = stream.into_split();
                    tokio::spawn(connection::serve(read, write, hub.clone()));
                }
                Err(error) => {
                    tracing::warn!(
                        error = %crate::error_chain::chain(&error),
                        "refused a connection"
                    );
                }
            },
            Err(error) => {
                tracing::warn!(%error, "cannot accept a connection");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_replaces_a_leftover_socket_and_admits_only_the_owner() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("yetack.sock");
        // Dropping a listener leaves its file, as a crashed daemon does.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());

        let _listener = bind(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, SOCKET_MODE);
        std::os::unix::net::UnixStream::connect(&path).unwrap();
    }
}

//! `agents-cockpit bridge`: what `ssh <host> agents-cockpit bridge` runs. It copies bytes between
//! its stdin and stdout and the daemon's socket, and parses nothing.

use std::path::Path;

use anyhow::Context;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;

const BUFFER_BYTES: usize = 64 * 1024;

/// Copies until either side closes.
pub(crate) async fn run(socket: &Path) -> anyhow::Result<()> {
    let stream = UnixStream::connect(socket).await.with_context(|| {
        format!(
            "cannot connect to {}: is `agents-cockpit serve` running?",
            socket.display()
        )
    })?;
    let (mut from_daemon, mut to_daemon) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    tokio::select! {
        copied = copy(&mut stdin, &mut to_daemon) => copied.context("cannot copy to the daemon")?,
        copied = copy(&mut from_daemon, &mut stdout) => copied.context("cannot copy from the daemon")?,
    }
    Ok(())
}

/// Copies and flushes each chunk at once: a protocol line must not wait in a buffer.
async fn copy<R, W>(reader: &mut R, writer: &mut W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buffer = vec![0; BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer).await?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            return Ok(());
        };
        writer.write_all(chunk).await?;
        writer.flush().await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn copies_until_the_reader_ends() {
        let input = b"{\"type\":\"hello\"}\nsecond line\n".to_vec();
        let mut output = Vec::new();

        copy(&mut input.as_slice(), &mut output).await.unwrap();

        assert_eq!(output, input);
    }
}

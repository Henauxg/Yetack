//! Opens the web links of an agent's text in the user's browser.
//!
//! On Linux the app runs `xdg-open` without `LD_LIBRARY_PATH`. A parent process may set it, as
//! the NixOS `claude` wrapper does with its own alsa-lib, and an app started from an agent's
//! session inherits it. The browser then loads a library built for another glibc and exits before
//! it opens the page. On other systems, the `webbrowser` crate opens the link, as Dioxus does.

use std::io;
#[cfg(target_os = "linux")]
use std::process::{ExitStatus, Stdio};

#[cfg(target_os = "linux")]
use tokio::process::Command;

#[cfg(target_os = "linux")]
const OPENER: &str = "xdg-open";
/// The variable the browser does not inherit: see the module's documentation.
#[cfg(target_os = "linux")]
const LIBRARY_PATH: &str = "LD_LIBRARY_PATH";

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[cfg(target_os = "linux")]
    #[error("{OPENER} ended with {0}")]
    Failed(ExitStatus),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Opens `url`, an http or https address, and waits until the opener is done. The browser itself
/// may keep running.
#[cfg(target_os = "linux")]
pub async fn open(url: &str) -> Result<(), OpenError> {
    match run(opener_command(url)).await {
        Err(OpenError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            tracing::warn!("{OPENER} is missing, opening the link with the webbrowser crate");
            open_with_crate(url).await
        }
        result => result,
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn open(url: &str) -> Result<(), OpenError> {
    open_with_crate(url).await
}

#[cfg(target_os = "linux")]
fn opener_command(url: &str) -> Command {
    let mut command = Command::new(OPENER);
    // The opener's output is not read: a browser it starts keeps the pipes open while it runs.
    command
        .arg(url)
        .env_remove(LIBRARY_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(target_os = "linux")]
async fn run(mut command: Command) -> Result<(), OpenError> {
    let status = command.status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(OpenError::Failed(status))
    }
}

async fn open_with_crate(url: &str) -> Result<(), OpenError> {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || webbrowser::open(&url))
        .await
        .map_err(io::Error::other)??;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn the_opener_gets_the_url_without_the_library_path() {
        let command = opener_command("https://example.org/a?b=1&c=2");
        let command = command.as_std();

        assert_eq!(command.get_program(), OPENER);
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, ["https://example.org/a?b=1&c=2"]);
        let library_path = command.get_envs().find(|(name, _)| *name == LIBRARY_PATH);
        assert_eq!(library_path, Some((OsStr::new(LIBRARY_PATH), None)));
    }

    #[tokio::test]
    async fn an_opener_that_fails_is_an_error() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 3"]);

        let result = run(command).await;

        assert!(
            matches!(result, Err(OpenError::Failed(status)) if status.code() == Some(3)),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn an_opener_that_succeeds_is_done() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        assert!(run(command).await.is_ok());
    }
}

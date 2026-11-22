//! Opens the web links of an agent's text, and the web pages and PDFs the file view shows, in the
//! user's browser.
//!
//! On Linux the app runs `xdg-open` without `LD_LIBRARY_PATH`. A parent process may set it, as
//! the NixOS `claude` wrapper does with its own alsa-lib, and an app started from an agent's
//! session inherits it. The browser then loads a library built for another glibc and exits before
//! it opens the page. On other systems, the `webbrowser` crate opens the link, as Dioxus does. On
//! Windows it starts the browser that opens `http` addresses, whatever program opens `.html`
//! files. A PDF opens there too, but on Linux in the program `xdg-open` picks for PDFs.

use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{ExitStatus, Stdio};

use cockpit_protocol::is_pdf_name;
#[cfg(target_os = "linux")]
use tokio::process::Command;
use url::Url;

use crate::path_style;
use crate::state::page_copy;

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
    #[error("the file is not a web page")]
    NotAPage,
    #[error("the file is not a PDF")]
    NotAPdf,
    /// Resolving a path on a network share makes Windows send its host a hash of the user's
    /// login.
    #[error("a file on a network share does not open in the browser")]
    NetworkShare,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Opens `url`, a web address or a page's `file:` address, and waits until the opener is done.
/// The browser itself may keep running.
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

/// The files the opener opens by their `file:` address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenedFile {
    /// An `.html` or `.htm` file.
    Page,
    /// A `.pdf` file.
    Pdf,
}

impl OpenedFile {
    fn is_named(self, name: &str) -> bool {
        match self {
            Self::Page => page_copy::is_page(name),
            Self::Pdf => is_pdf_name(name),
        }
    }

    fn refusal(self) -> OpenError {
        match self {
            Self::Page => OpenError::NotAPage,
            Self::Pdf => OpenError::NotAPdf,
        }
    }
}

/// Opens the web page at `path`, an `.html` or `.htm` file on this computer. The symbolic links
/// on the path are resolved first, and the file they lead to must be a page too: on Linux the
/// opener picks the program by the name it gets.
pub async fn open_page(path: PathBuf) -> Result<(), OpenError> {
    open_file(path, OpenedFile::Page).await
}

/// Opens the PDF at `path`, a `.pdf` file on this computer, with the checks of [`open_page`].
pub async fn open_pdf(path: PathBuf) -> Result<(), OpenError> {
    open_file(path, OpenedFile::Pdf).await
}

async fn open_file(path: PathBuf, opened: OpenedFile) -> Result<(), OpenError> {
    let url = tokio::task::spawn_blocking(move || file_url(&path, opened))
        .await
        .map_err(io::Error::other)??;
    open(&url).await
}

fn file_url(path: &Path, opened: OpenedFile) -> Result<String, OpenError> {
    if cfg!(windows) && path_style::is_network_share(&path.to_string_lossy()) {
        return Err(OpenError::NetworkShare);
    }
    let resolved = path.canonicalize()?;
    let is_file = resolved.metadata()?.is_file();
    if !is_file || !opened.is_named(&resolved.to_string_lossy()) {
        return Err(opened.refusal());
    }
    Url::from_file_path(&resolved)
        .map(String::from)
        .map_err(|()| opened.refusal())
}

async fn open_with_crate(url: &str) -> Result<(), OpenError> {
    let url = url.to_owned();
    tokio::task::spawn_blocking(move || webbrowser::open(&url))
        .await
        .map_err(io::Error::other)??;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn a_page_opens_by_its_file_address_once_its_links_are_resolved() {
        let temp = tempfile::tempdir().unwrap();
        let page = temp.path().join("mock up").join("Index.HTML");
        std::fs::create_dir(page.parent().unwrap()).unwrap();
        std::fs::write(&page, "<p>Hi</p>").unwrap();

        let url = file_url(&page, OpenedFile::Page).unwrap();

        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/mock%20up/Index.HTML"), "{url}");
    }

    #[test]
    fn a_file_or_a_folder_that_is_no_page_does_not_open() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        std::fs::create_dir(temp.path().join("site.html")).unwrap();

        for path in ["notes.txt", "site.html"] {
            let refused = file_url(&temp.path().join(path), OpenedFile::Page);
            assert!(
                matches!(refused, Err(OpenError::NotAPage)),
                "{path}: {refused:?}"
            );
        }
        let missing = file_url(&temp.path().join("gone.html"), OpenedFile::Page);
        assert!(matches!(missing, Err(OpenError::Io(_))), "{missing:?}");
    }

    #[test]
    fn a_pdf_opens_by_its_file_address_and_no_other_file_does() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Report.PDF"), "%PDF-").unwrap();
        std::fs::write(temp.path().join("index.html"), "<p>Hi</p>").unwrap();

        let url = file_url(&temp.path().join("Report.PDF"), OpenedFile::Pdf).unwrap();
        let refused = file_url(&temp.path().join("index.html"), OpenedFile::Pdf);

        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("/Report.PDF"), "{url}");
        assert!(matches!(refused, Err(OpenError::NotAPdf)), "{refused:?}");
    }

    /// Checked before the path is resolved: the host's name does not resolve, so a lookup fails
    /// with another error.
    #[cfg(windows)]
    #[test]
    fn a_file_on_a_network_share_does_not_open() {
        for (path, opened) in [
            (r"\\fileserver.invalid\team\report.pdf", OpenedFile::Pdf),
            ("//fileserver.invalid/team/index.html", OpenedFile::Page),
        ] {
            let refused = file_url(Path::new(path), opened);
            assert!(
                matches!(refused, Err(OpenError::NetworkShare)),
                "{path}: {refused:?}"
            );
        }
    }

    /// The opener picks a program by the name it gets: a link named like a page must lead to one.
    #[cfg(unix)]
    #[test]
    fn a_link_named_like_a_page_must_lead_to_one() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("run.sh"), "#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(temp.path().join("run.sh"), temp.path().join("page.html"))
            .unwrap();

        let refused = file_url(&temp.path().join("page.html"), OpenedFile::Page);

        assert!(matches!(refused, Err(OpenError::NotAPage)), "{refused:?}");
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_opener_that_succeeds_is_done() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);

        assert!(run(command).await.is_ok());
    }
}

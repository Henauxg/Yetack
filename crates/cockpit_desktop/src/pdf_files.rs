//! Opens a PDF of the file view outside the app: the file itself for an agent on this computer,
//! else a copy of the bytes the daemon sent, in a new private folder of the temporary folder. The
//! browser shows it, or on Linux the program `xdg-open` picks for PDFs (module `browser`).
//!
//! The app's own window shows no PDF: a PDF viewer in the webview would parse the file beside the
//! app's pages, and the Linux webview has none.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cockpit_protocol::is_pdf_name;

use crate::browser::{self, OpenError};
use crate::page_files;
use crate::path_style::{self, PathStyle};

/// What the button that opens a PDF says on this system.
pub const OPEN_LABEL: &str = if cfg!(target_os = "linux") {
    "Open in the PDF viewer"
} else {
    "Open in the browser"
};

/// The start of the name of the folder each copy goes in.
const COPY_PREFIX: &str = "yetack-pdf-";

/// The copy's name when the PDF's own name does not suit this computer.
const FALLBACK_NAME: &str = "document.pdf";

/// Where the PDF to open is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfSource {
    /// A file on this computer, by its absolute path.
    Here(PathBuf),
    /// A file on a server: its name and its bytes.
    Server { name: String, bytes: Arc<[u8]> },
}

impl PdfSource {
    /// Where the file view opens the PDF at `path`, absolute on the agent's machine, whose bytes
    /// the daemon sent: the file itself on this computer, else a copy named as the file.
    pub fn of_read(path: &str, on_this_computer: bool, bytes: &Arc<[u8]>) -> Self {
        if on_this_computer {
            return Self::Here(PathBuf::from(path));
        }
        let name = PathStyle::of_text(path).file_name(path).unwrap_or_default();
        Self::Server {
            name: name.to_owned(),
            bytes: Arc::clone(bytes),
        }
    }

    /// Where the file view opens the file at `path` that the daemon did not send, as a PDF over
    /// its limit: the file itself, when it is on this computer, named as a PDF, and on no network
    /// share, which the daemon may have refused (see [`path_style::is_network_share`]). `None`
    /// else, since the app has no copy of a server's file.
    pub fn unread(path: &str, on_this_computer: bool) -> Option<Self> {
        (on_this_computer && is_pdf_name(path) && !path_style::is_network_share(path))
            .then(|| Self::Here(PathBuf::from(path)))
    }
}

/// Opens the PDF `source` names. A server's copy stays after this returns, since the browser
/// reads it then.
pub async fn open(source: PdfSource) -> Result<(), OpenError> {
    let path = match source {
        PdfSource::Here(path) => path,
        PdfSource::Server { name, bytes } => tokio::task::spawn_blocking(move || {
            let folder = tempfile::Builder::new()
                .prefix(COPY_PREFIX)
                .tempdir()?
                .keep();
            write_copy(&folder, &name, &bytes)
        })
        .await
        .map_err(io::Error::other)??,
    };
    browser::open_pdf(path).await
}

/// Writes `bytes` in `folder`, named `name` when this computer takes that name and it ends with
/// `.pdf`, else [`FALLBACK_NAME`]. Returns the copy's path.
fn write_copy(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    use std::io::Write as _;

    let name = if page_files::is_plain_name(name) && is_pdf_name(name) {
        name
    } else {
        FALLBACK_NAME
    };
    let path = folder.join(name);
    File::create_new(&path)?.write_all(bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pdf_read_opens_here_on_this_computer_and_as_a_named_copy_from_a_server() {
        let bytes: Arc<[u8]> = Arc::from(&b"%PDF-"[..]);

        let here = PdfSource::of_read(r"C:\mock\docs\report.pdf", true, &bytes);
        let server = PdfSource::of_read("/srv/docs/report.pdf", false, &bytes);

        assert_eq!(
            here,
            PdfSource::Here(PathBuf::from(r"C:\mock\docs\report.pdf"))
        );
        assert_eq!(
            server,
            PdfSource::Server {
                name: "report.pdf".to_owned(),
                bytes,
            }
        );
    }

    /// A PDF over the daemon's limit comes with no bytes, and opens by its path on this computer.
    #[test]
    fn an_unread_pdf_opens_only_from_this_computer() {
        assert_eq!(
            PdfSource::unread("/srv/docs/large.PDF", true),
            Some(PdfSource::Here(PathBuf::from("/srv/docs/large.PDF")))
        );
        assert_eq!(PdfSource::unread("/srv/docs/large.pdf", false), None);
        assert_eq!(PdfSource::unread("/srv/docs/notes.txt", true), None);
    }

    /// The daemon refuses a file on a share the agent does not work on, and so must the button:
    /// resolving the path would make Windows send that host a hash of the user's login.
    #[test]
    fn an_unread_pdf_on_a_network_share_does_not_open() {
        for path in [
            r"\\fileserver\team\large.pdf",
            "//fileserver/team/large.pdf",
            r"\\?\UNC\fileserver\team\large.pdf",
        ] {
            assert_eq!(PdfSource::unread(path, true), None, "{path}");
        }
        assert_eq!(
            PdfSource::unread(r"C:\mock\docs\large.pdf", true),
            Some(PdfSource::Here(PathBuf::from(r"C:\mock\docs\large.pdf")))
        );
    }

    #[test]
    fn a_copy_keeps_the_pdfs_name() {
        let temp = tempfile::tempdir().unwrap();

        let path = write_copy(temp.path(), "Report 2026.PDF", b"%PDF-").unwrap();

        assert_eq!(path, temp.path().join("Report 2026.PDF"));
        assert_eq!(std::fs::read(&path).unwrap(), b"%PDF-");
    }

    /// The opener picks a program by the name it gets, and a server's name may not suit this
    /// computer.
    #[test]
    fn a_name_that_is_no_plain_pdf_name_gives_the_fallback() {
        for name in ["run.bat", "a:b.pdf", "NUL.pdf", "", "notes.pdf."] {
            let temp = tempfile::tempdir().unwrap();

            let path = write_copy(temp.path(), name, b"%PDF-").unwrap();

            assert_eq!(path, temp.path().join(FALLBACK_NAME), "{name}");
        }
    }
}

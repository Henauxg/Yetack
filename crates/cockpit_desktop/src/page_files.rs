//! Writes the copy of a server's web page to this computer, for the browser: the page and the
//! files it names, in a new private folder of the temporary folder (`state::page_copy` reads
//! them through the daemon).

use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use crate::browser::{self, OpenError};
use crate::state::page_copy::ReadyCopy;

/// The start of the name of the folder each copy goes in.
const COPY_PREFIX: &str = "yetack-page-";

/// The names Windows reads as a device in any folder, whatever their case and extension.
const DEVICE_NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
/// The names of the numbered ports, which one digit ends.
const PORT_NAMES: [&str; 2] = ["COM", "LPT"];
/// The digits that end a port's name besides 0 to 9: Windows reads `COM¹` as a port too.
const SUPERSCRIPT_DIGITS: [char; 3] = ['\u{b9}', '\u{b2}', '\u{b3}'];
/// The characters a Windows file name cannot hold.
const FORBIDDEN: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Writes `copy` to a new folder, then opens its page. The folder stays: the browser reads its
/// files after this returns. Returns the paths of the files the copy lacks, as [`write`] leaves
/// them out.
pub async fn open_copy(copy: ReadyCopy) -> Result<Vec<String>, OpenError> {
    let (page, refused) = tokio::task::spawn_blocking(move || {
        let folder = tempfile::Builder::new()
            .prefix(COPY_PREFIX)
            .tempdir()?
            .keep();
        write(&folder, &copy)
    })
    .await
    .map_err(io::Error::other)??;
    browser::open_page(page).await?;
    Ok(refused)
}

/// Writes the files of `copy` under `folder`. Returns the page's path, and the files left out:
/// those whose name this computer cannot take, and those it fails to write but the page. A file
/// never replaces another: where case does not count, `A.png` would replace `a.png`.
fn write(folder: &Path, copy: &ReadyCopy) -> Result<(PathBuf, Vec<String>), OpenError> {
    let page = local_path(folder, &copy.page).ok_or(OpenError::NotAPage)?;
    let mut refused = Vec::new();
    for (names, bytes) in &copy.files {
        let written = local_path(folder, names).map(|path| write_file(&path, bytes));
        match written {
            Some(Ok(())) => {}
            Some(Err(error)) if *names == copy.page => return Err(error.into()),
            Some(Err(error)) => {
                tracing::warn!("Cannot write a file of a page's copy: {error}");
                refused.push(names.join("/"));
            }
            None => refused.push(names.join("/")),
        }
    }
    Ok((page, refused))
}

fn write_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create_new(path)?.write_all(bytes)
}

/// `names` under `folder`, when each is a plain file name on Linux as on Windows.
fn local_path(folder: &Path, names: &[String]) -> Option<PathBuf> {
    let mut path = folder.to_owned();
    for name in names {
        if !is_plain_name(name) {
            return None;
        }
        path.push(name);
    }
    Some(path)
}

/// Whether `name` names one file in a folder, on Linux as on Windows. Windows reads a name such
/// as `NUL` or `nul.png` as a device, and `a:b` as a stream of the file `a`.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    let single = matches!(components.next(), Some(Component::Normal(only)) if only == name)
        && components.next().is_none();
    let forbidden = name.contains(FORBIDDEN) || name.chars().any(char::is_control);
    single && !forbidden && !names_a_device(name) && !name.ends_with(['.', ' '])
}

fn names_a_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let port = PORT_NAMES.iter().any(|port| {
        let number = stem
            .get(..port.len())
            .filter(|start| start.eq_ignore_ascii_case(port))
            .and_then(|_| stem.get(port.len()..));
        number.is_some_and(|number| {
            let mut characters = number.chars();
            let digit = characters
                .next()
                .is_some_and(|digit| digit.is_ascii_digit() || SUPERSCRIPT_DIGITS.contains(&digit));
            digit && characters.next().is_none()
        })
    });
    port || DEVICE_NAMES
        .iter()
        .any(|device| stem.eq_ignore_ascii_case(device))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(path: &str) -> Vec<String> {
        path.split('/').map(str::to_owned).collect()
    }

    #[test]
    fn a_plain_name_is_one_name_that_no_system_reads_otherwise() {
        for plain in [
            "index.html",
            "a b.png",
            ".hidden",
            "COM10.txt",
            "console.log",
            "café.css",
        ] {
            assert!(is_plain_name(plain), "{plain}");
        }
        for other in [
            "",
            ".",
            "..",
            "a/b",
            r"a\b",
            "a:b",
            "NUL",
            "nul.png",
            "Com1",
            "lpt\u{b9}.txt",
            "conin$",
            "aux .js",
            "trailing.",
            "trailing ",
            "tab\there",
        ] {
            assert!(!is_plain_name(other), "{other:?}");
        }
    }

    #[test]
    fn the_copy_keeps_the_places_of_its_files_and_leaves_out_the_names_it_cannot_take() {
        let temp = tempfile::tempdir().unwrap();
        let copy = ReadyCopy {
            page: names("pages/index.html"),
            files: vec![
                (names("pages/index.html"), b"<img src=a.png>".to_vec()),
                (names("pages/a.png"), vec![0x89, 0x50]),
                (names("css/site.css"), b"body {}".to_vec()),
                (names("pages/nul.png"), b"device".to_vec()),
            ],
            missing: Vec::new(),
        };

        let (page, refused) = write(temp.path(), &copy).unwrap();

        assert_eq!(page, temp.path().join("pages").join("index.html"));
        assert_eq!(std::fs::read(&page).unwrap(), b"<img src=a.png>");
        assert_eq!(
            std::fs::read(temp.path().join("pages").join("a.png")).unwrap(),
            [0x89, 0x50]
        );
        assert_eq!(
            std::fs::read(temp.path().join("css").join("site.css")).unwrap(),
            b"body {}"
        );
        assert_eq!(refused, ["pages/nul.png"]);
    }

    /// A file and a folder of one name, as `a` and `a/b.png`, or `A` and `a/b.png` where case
    /// does not count, cannot both be written.
    #[test]
    fn a_file_that_cannot_be_written_is_left_out() {
        let temp = tempfile::tempdir().unwrap();
        let copy = ReadyCopy {
            page: names("index.html"),
            files: vec![
                (names("index.html"), b"<img src=shots>".to_vec()),
                (names("shots"), b"file".to_vec()),
                (names("shots/a.png"), vec![0x89, 0x50]),
                (names("b.png"), vec![0x89]),
            ],
            missing: Vec::new(),
        };

        let (page, refused) = write(temp.path(), &copy).unwrap();

        assert_eq!(std::fs::read(&page).unwrap(), b"<img src=shots>");
        assert_eq!(std::fs::read(temp.path().join("b.png")).unwrap(), [0x89]);
        assert_eq!(refused, ["shots/a.png"]);
    }

    /// Names a Linux server tells apart by their case are one file on Windows and macOS.
    #[test]
    fn a_later_file_never_replaces_an_earlier_one() {
        let temp = tempfile::tempdir().unwrap();
        let copy = ReadyCopy {
            page: names("index.html"),
            files: vec![
                (names("index.html"), b"<p>page</p>".to_vec()),
                (names("INDEX.HTML"), b"<p>other</p>".to_vec()),
                (names("a.png"), b"first".to_vec()),
                (names("A.png"), b"second".to_vec()),
            ],
            missing: Vec::new(),
        };

        let (page, _) = write(temp.path(), &copy).unwrap();

        assert_eq!(std::fs::read(&page).unwrap(), b"<p>page</p>");
        assert_eq!(std::fs::read(temp.path().join("a.png")).unwrap(), b"first");
    }

    #[test]
    fn a_page_whose_name_this_computer_cannot_take_is_no_page() {
        let temp = tempfile::tempdir().unwrap();
        let copy = ReadyCopy {
            page: names("a:b.html"),
            files: vec![(names("a:b.html"), Vec::new())],
            missing: Vec::new(),
        };

        let written = write(temp.path(), &copy);

        assert!(matches!(written, Err(OpenError::NotAPage)), "{written:?}");
    }
}

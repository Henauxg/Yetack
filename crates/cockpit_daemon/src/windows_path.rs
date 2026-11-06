//! Windows paths as text, for the file requests' checks: whether a path is inside a folder,
//! whether it reaches a network share, and how it shows.
//!
//! `canonicalize` and `GetFinalPathNameByHandleW` give verbatim paths, `\\?\C:\work` and
//! `\\?\UNC\host\share\work`, while a request names `C:\work` or `\\host\share\work`. The checks
//! read both forms as the same path, and compare names whatever their case, as Windows does.
//! A device path, such as `\\.\pipe\name`, is inside no folder.

use std::path::{Path, PathBuf};

const VERBATIM_SHARE: &str = r"\\?\UNC\";
const VERBATIM: &str = r"\\?\";
const DEVICE: &str = r"\\.\";
const NT_OBJECT: &str = r"\??\";
const SHARE: &str = r"\\";
/// The names Windows reads as a device in any folder, whatever their extension.
const DEVICE_NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
/// Followed by a digit, or by a superscript 1, 2 or 3, they name a port.
const PORT_NAMES: [&str; 2] = ["COM", "LPT"];

/// Where an absolute Windows path starts.
#[derive(Debug, Clone, Copy)]
enum Root<'path> {
    Disk(char),
    Share { host: &'path str, share: &'path str },
}

impl Root<'_> {
    fn same(self, other: Root<'_>) -> bool {
        match (self, other) {
            (Self::Disk(letter), Root::Disk(other)) => letter.eq_ignore_ascii_case(&other),
            (
                Self::Share { host, share },
                Root::Share {
                    host: other_host,
                    share: other_share,
                },
            ) => same_name(host, other_host) && same_name(share, other_share),
            (Self::Disk(_), Root::Share { .. }) | (Self::Share { .. }, Root::Disk(_)) => false,
        }
    }
}

#[derive(Debug)]
struct Absolute<'path> {
    root: Root<'path>,
    names: Vec<&'path str>,
}

/// `None` for a relative path, a device path, or a path with `..`.
fn parse(path: &str) -> Option<Absolute<'_>> {
    let (root, rest) = if let Some(rest) = path.strip_prefix(VERBATIM_SHARE) {
        share_root(rest)?
    } else if let Some(rest) = path.strip_prefix(VERBATIM) {
        disk_root(rest)?
    } else if path.starts_with(DEVICE) || path.starts_with(NT_OBJECT) {
        return None;
    } else if path.starts_with(SHARE) || path.starts_with("//") {
        share_root(path.get(SHARE.len()..)?)?
    } else {
        disk_root(path)?
    };
    let mut names = Vec::new();
    for name in rest.split(is_separator) {
        match name {
            "" | "." => {}
            ".." => return None,
            name => names.push(name),
        }
    }
    Some(Absolute { root, names })
}

fn is_separator(character: char) -> bool {
    matches!(character, '\\' | '/')
}

/// `C:\rest`: the root and the rest.
fn disk_root(path: &str) -> Option<(Root<'_>, &str)> {
    let mut characters = path.chars();
    let letter = characters.next().filter(char::is_ascii_alphabetic)?;
    let rest = characters.as_str().strip_prefix(':')?;
    rest.starts_with(is_separator)
        .then_some((Root::Disk(letter), rest))
}

/// `host\share\rest`, once the leading `\\` is gone: the root and the rest.
fn share_root(path: &str) -> Option<(Root<'_>, &str)> {
    let mut parts = path.splitn(3, is_separator);
    let host = parts.next().filter(|host| !host.is_empty())?;
    let share = parts.next().filter(|share| !share.is_empty())?;
    // A host of `.` or `?` would make it a device path.
    if matches!(host, "." | "?") {
        return None;
    }
    Some((Root::Share { host, share }, parts.next().unwrap_or("")))
}

/// Whether two names are the same name for Windows, whatever their case.
fn same_name(left: &str, right: &str) -> bool {
    left.chars().map(upper).eq(right.chars().map(upper))
}

/// Windows maps each character to one upper case character, or leaves it: `ß` stays `ß`.
fn upper(character: char) -> char {
    let mut upper = character.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single,
        _ => character,
    }
}

/// Whether `path` is `folder` or inside it. Both are absolute, verbatim or not.
pub(crate) fn is_inside(path: &str, folder: &str) -> bool {
    let (Some(path), Some(folder)) = (parse(path), parse(folder)) else {
        return false;
    };
    path.root.same(folder.root)
        && path.names.len() >= folder.names.len()
        && path
            .names
            .iter()
            .zip(&folder.names)
            .all(|(name, folder_name)| same_name(name, folder_name))
}

/// Whether the file system may open `path` for a check: a path on a disk, or on a network
/// share that one of `allowed` is on. A device path, a relative one and one with `..` are
/// refused, and so is a share no allowed folder is on, so that no check makes Windows connect
/// to another host.
pub(crate) fn may_open<'allowed>(
    path: &str,
    allowed: impl IntoIterator<Item = &'allowed str>,
) -> bool {
    let Some(path) = parse(path) else {
        return false;
    };
    match path.root {
        Root::Disk(_) => true,
        Root::Share { .. } => allowed
            .into_iter()
            .filter_map(parse)
            .any(|folder| folder.root.same(path.root)),
    }
}

/// Whether a name of `path` is one Windows reads as a device, such as `NUL`, `COM1.txt` or
/// `AUX:`.
pub(crate) fn names_a_device(path: &str) -> bool {
    path.split(is_separator).any(|name| {
        let stem = name.split(['.', ':']).next().unwrap_or(name).trim_end();
        let port = PORT_NAMES.iter().any(|port| {
            stem.get(..port.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(port))
                && stem.get(port.len()..).is_some_and(|number| {
                    matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                        || matches!(number, "\u{b9}" | "\u{b2}" | "\u{b3}")
                })
        });
        port || DEVICE_NAMES
            .iter()
            .any(|device| stem.eq_ignore_ascii_case(device))
    })
}

/// `path` without its verbatim prefix, as a user writes it: `C:\work` for `\\?\C:\work`, and
/// `\\host\share` for `\\?\UNC\host\share`.
pub(crate) fn shown(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(VERBATIM_SHARE) {
        return format!("{SHARE}{rest}");
    }
    match path.strip_prefix(VERBATIM) {
        Some(rest) if disk_root(rest).is_some() => rest.to_owned(),
        _ => path.to_owned(),
    }
}

/// [`shown`] for a path that is text, the path itself otherwise.
pub(crate) fn shown_path(path: &Path) -> PathBuf {
    path.to_str()
        .map_or_else(|| path.to_owned(), |text| PathBuf::from(shown(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_path_is_the_same_path_whatever_the_case() {
        assert!(is_inside(r"\\?\C:\Work\App\src\main.rs", r"c:\work\app"));
        assert!(is_inside(r"C:\work\app", r"\\?\C:\Work\App\"));
        assert!(is_inside(r"C:/work/app/src", r"C:\work\app"));
        assert!(is_inside(
            r"\\?\UNC\Nas\Projects\app\a.txt",
            r"\\nas\projects\app"
        ));
        assert!(is_inside(r"C:\École\a", r"C:\école"));
    }

    #[test]
    fn a_path_outside_the_folder_is_not_inside() {
        assert!(!is_inside(r"C:\work\application", r"C:\work\app"));
        assert!(!is_inside(r"C:\work", r"C:\work\app"));
        assert!(!is_inside(r"D:\work\app\a.txt", r"C:\work\app"));
        assert!(!is_inside(r"\\nas\other\app", r"\\nas\projects\app"));
        assert!(!is_inside(r"\\other\projects\app", r"\\nas\projects\app"));
        assert!(!is_inside(r"\\?\UNC\nas\projects\app", r"C:\work"));
        assert!(!is_inside(r"C:\work\app\..\secret", r"C:\work\app"));
        assert!(!is_inside(r"C:\straße\a", r"C:\STRASSE"));
    }

    #[test]
    fn a_device_or_relative_path_is_inside_nothing() {
        for path in [
            r"\\.\pipe\agent",
            r"\\?\pipe\agent",
            r"\??\C:\work\app",
            r"\\.\C:\work\app",
            r"\\?\Volume{0b1c}\work\app",
            r"\\?\GLOBALROOT\Device\HarddiskVolume1\work\app",
            r"work\app",
            r"\work\app",
            r"C:work\app",
        ] {
            assert!(!is_inside(path, r"C:\work\app"), "{path}");
            assert!(!may_open(path, [r"C:\work\app"]), "{path}");
        }
    }

    #[test]
    fn only_the_shares_of_the_allowed_folders_may_be_opened() {
        let allowed = [r"C:\work\app", r"\\?\UNC\nas\projects\shop"];

        assert!(may_open(r"C:\anywhere\else", allowed));
        assert!(may_open(r"\\NAS\Projects\other", allowed));
        assert!(!may_open(r"\\attacker\share\file.txt", allowed));
        assert!(!may_open(r"//attacker/share/file.txt", allowed));
        assert!(!may_open(r"\\?\UNC\attacker\share", allowed));
        assert!(!may_open(r"\\nas\other\file.txt", allowed));
        assert!(!may_open(r"\\attacker", allowed));
    }

    #[test]
    fn device_names_are_found_in_any_folder_with_any_extension() {
        for path in [
            r"C:\work\NUL",
            r"C:\work\con.txt",
            r"C:\work\com1",
            r"C:\work\Lpt9.log",
            "C:\\work\\COM\u{b9}",
            r"C:\work\aux .md",
            r"C:\work\conin$",
            r"nul\file.txt",
            r"C:\work\nul:",
            r"C:\work\COM1:",
            r"C:\work\aux:stream",
        ] {
            assert!(names_a_device(path), "{path}");
        }
        for path in [
            r"C:\work\null",
            r"C:\work\console.txt",
            r"C:\work\com10",
            r"C:\work\com0",
            r"C:\work\lpt",
            r"C:\work\nul_file",
        ] {
            assert!(!names_a_device(path), "{path}");
        }
    }

    #[test]
    fn a_verbatim_path_shows_as_a_user_writes_it() {
        assert_eq!(shown(r"\\?\C:\work\app"), r"C:\work\app");
        assert_eq!(shown(r"\\?\UNC\nas\projects"), r"\\nas\projects");
        assert_eq!(shown(r"C:\work\app"), r"C:\work\app");
        assert_eq!(shown(r"\\?\Volume{0b1c}\work"), r"\\?\Volume{0b1c}\work");
        assert_eq!(
            shown_path(Path::new(r"\\?\D:\shop")),
            PathBuf::from(r"D:\shop")
        );
    }
}

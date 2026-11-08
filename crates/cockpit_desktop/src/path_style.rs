//! The path rules of the machine a daemon runs on. The app on Windows may reach a daemon on Linux
//! over SSH, so a path follows the rules of the machine it comes from, not the app's.
//!
//! The rules work on text, so that Linux tests both. The protocol carries paths as text.

use std::borrow::Cow;
use std::path::Path;

const UNIX_SEPARATOR: char = '/';
const WINDOWS_SEPARATOR: char = '\\';

/// What follows a drive letter, as in `C:\`.
const DRIVE_MARK: char = ':';
/// The prefix of a Windows verbatim path, `\\?\C:\work`, which Windows gives for a resolved path.
const VERBATIM: &str = r"\\?\";

/// The name of the folder itself, which names nothing more.
const CURRENT: &str = ".";

/// The name of the folder above.
const PARENT: &str = "..";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    /// `/` separates names and is the root. Case tells names apart.
    Unix,
    /// `\` and `/` separate names. `C:\`, `C:/` and `\\server\share\` are roots. Case does not
    /// tell names apart. Paths show with `\`.
    Windows,
}

impl PathStyle {
    /// The rules of the platform the app runs on.
    pub const NATIVE: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Unix
    };

    /// The rules of the machine an absolute path comes from: a drive or a `\\` share is Windows',
    /// a leading `/` is Unix's. A relative path gets the app's own.
    pub fn of(path: &Path) -> Self {
        Self::of_text(&path.to_string_lossy())
    }

    pub fn of_text(text: &str) -> Self {
        if has_drive(text) || text.starts_with(r"\\") {
            Self::Windows
        } else if text.starts_with(UNIX_SEPARATOR) {
            Self::Unix
        } else {
            Self::NATIVE
        }
    }

    /// The separator the style shows.
    pub fn separator(self) -> char {
        match self {
            Self::Unix => UNIX_SEPARATOR,
            Self::Windows => WINDOWS_SEPARATOR,
        }
    }

    pub fn is_separator(self, character: char) -> bool {
        match self {
            Self::Unix => character == UNIX_SEPARATOR,
            Self::Windows => character == UNIX_SEPARATOR || character == WINDOWS_SEPARATOR,
        }
    }

    /// The length of the root that starts `text`, separator included: `/`, `C:\`, or
    /// `\\server\share\`. `None` for a relative path, and on Windows for `\folder`, which names
    /// no drive.
    pub fn root_len(self, text: &str) -> Option<usize> {
        match self {
            Self::Unix => text.starts_with(UNIX_SEPARATOR).then_some(1),
            Self::Windows if has_drive(text) => Some(3),
            Self::Windows => {
                let mut characters = text.chars();
                let leading = characters.next().zip(characters.next());
                if !leading.is_some_and(|(first, second)| {
                    self.is_separator(first) && self.is_separator(second)
                }) {
                    return None;
                }
                // The server's name, then the share's.
                let names = self.names_from(text, 2);
                let (share_start, share) = names.get(1)?;
                let end = share_start + share.len();
                let separator = text.get(end..)?.chars().next().map_or(0, char::len_utf8);
                Some(end + separator)
            }
        }
    }

    pub fn is_absolute(self, text: &str) -> bool {
        self.root_len(text).is_some()
    }

    /// The text split after its last separator: the folder part, which ends with the separator or
    /// is empty, and the last segment.
    pub fn split(self, text: &str) -> (&str, &str) {
        match text.rfind(|character| self.is_separator(character)) {
            Some(at) => text.split_at(at + 1),
            None => ("", text),
        }
    }

    /// The names after the root, each with its byte offset, without the empty names that repeated
    /// separators make and without ".".
    fn names_at(self, text: &str) -> Vec<(usize, &str)> {
        self.names_from(text, self.root_len(text).unwrap_or(0))
    }

    fn names_from(self, text: &str, start: usize) -> Vec<(usize, &str)> {
        let mut names = Vec::new();
        let mut name_start = start;
        let rest = text.get(start..).unwrap_or_default();
        let ends = rest
            .char_indices()
            .filter(|(_, character)| self.is_separator(*character))
            .map(|(at, _)| start + at)
            .chain([text.len()]);
        for end in ends {
            let name = text.get(name_start..end).unwrap_or_default();
            if !name.is_empty() && name != CURRENT {
                names.push((name_start, name));
            }
            name_start = end + 1;
        }
        names
    }

    /// The path without its last name, and without the separators before it, the root's aside.
    /// `None` for a root or an empty path. A single relative name's parent is empty.
    pub fn parent(self, path: &str) -> Option<&str> {
        let (start, _) = *self.names_at(path).last()?;
        let root = self.root_len(path).unwrap_or(0);
        let before = path.get(..start)?;
        let kept = before.trim_end_matches(|character| self.is_separator(character));
        path.get(..kept.len().max(root))
    }

    /// The first name after the root.
    pub fn first_name(self, path: &str) -> Option<&str> {
        self.names(path).next()
    }

    /// The names after the root, without the empty names that repeated separators make and
    /// without ".".
    pub fn names(self, path: &str) -> impl Iterator<Item = &str> {
        self.names_at(path).into_iter().map(|(_, name)| name)
    }

    /// The path's names, the last first, without "..".
    pub fn names_from_the_end(self, path: &str) -> Vec<&str> {
        let names: Vec<&str> = self.names(path).filter(|name| *name != PARENT).collect();
        names.into_iter().rev().collect()
    }

    /// Whether the path goes up a folder at some point.
    pub fn goes_up(self, path: &str) -> bool {
        self.names(path).any(|name| name == PARENT)
    }

    /// The length of a drive at the start of `text`, as `C:` in `C:\`: 0 without one.
    pub fn drive_len(self, text: &str) -> usize {
        match self {
            Self::Windows if has_drive(text) => 2,
            Self::Unix | Self::Windows => 0,
        }
    }

    /// The last name, `None` for a root, an empty path, or one that ends with "..".
    pub fn file_name(self, path: &str) -> Option<&str> {
        self.names_at(path)
            .last()
            .map(|(_, name)| *name)
            .filter(|name| *name != PARENT)
    }

    /// Whether two names are the same name.
    pub fn same_name(self, left: &str, right: &str) -> bool {
        match self {
            Self::Unix => left == right,
            Self::Windows => left.chars().map(upper).eq(right.chars().map(upper)),
        }
    }

    fn same_root(self, left: &str, right: &str) -> bool {
        let root = |text: &str| {
            let length = self.root_len(text).unwrap_or(0);
            let root = text.get(..length).unwrap_or_default();
            self.shown(root).into_owned()
        };
        self.same_name(&root(left), &root(right))
    }

    /// The rest of `path` after `base`, when `base` holds it: empty for `base` itself.
    pub fn strip_prefix<'path>(self, path: &'path str, base: &str) -> Option<&'path str> {
        if !self.same_root(path, base) {
            return None;
        }
        let names = self.names_at(path);
        let base_names = self.names_at(base);
        if base_names.len() > names.len() {
            return None;
        }
        let held = names
            .iter()
            .zip(&base_names)
            .all(|((_, name), (_, base_name))| self.same_name(name, base_name));
        if !held {
            return None;
        }
        let rest = match names.get(base_names.len()) {
            Some((start, _)) => path.get(*start..)?,
            None => "",
        };
        Some(rest.trim_end_matches(|character| self.is_separator(character)))
    }

    /// Whether two paths name the same file.
    pub fn same_path(self, left: &str, right: &str) -> bool {
        self.strip_prefix(left, right).is_some_and(str::is_empty)
    }

    /// `name` in the folder `base`.
    pub fn join(self, base: &str, name: &str) -> String {
        if base.is_empty() || base.ends_with(|character| self.is_separator(character)) {
            format!("{base}{name}")
        } else {
            format!("{base}{}{name}", self.separator())
        }
    }

    /// The folder `text` names from `base`, with "." and ".." resolved in both. An absolute text
    /// stands alone; on Windows, a text that starts with a separator starts at `base`'s drive.
    pub fn resolve(self, base: &str, text: &str) -> String {
        let (root, start) = match self.root_len(text) {
            Some(length) => (text.get(..length).unwrap_or_default(), ""),
            None => {
                let length = self.root_len(base).unwrap_or(0);
                let root = base.get(..length).unwrap_or_default();
                let starts_at_root = text.starts_with(|character| self.is_separator(character));
                (root, if starts_at_root { "" } else { base })
            }
        };
        let mut names = Vec::new();
        for name in self.names(start).chain(self.names(text)) {
            if name == PARENT {
                names.pop();
            } else {
                names.push(name);
            }
        }
        let separator = self.separator().to_string();
        format!("{}{}", self.shown(root), names.join(&separator))
    }

    /// The path as the style shows it: with `\` on Windows.
    pub fn shown(self, path: &str) -> Cow<'_, str> {
        if self == Self::Windows
            && let Some(rest) = path.strip_prefix(VERBATIM)
            && has_drive(rest)
        {
            return Cow::Owned(rest.replace(UNIX_SEPARATOR, &WINDOWS_SEPARATOR.to_string()));
        }
        match self {
            Self::Windows if path.contains(UNIX_SEPARATOR) => {
                Cow::Owned(path.replace(UNIX_SEPARATOR, &WINDOWS_SEPARATOR.to_string()))
            }
            Self::Unix | Self::Windows => Cow::Borrowed(path),
        }
    }

    /// A folder as a path field shows it, ending with a separator.
    pub fn folder_text(self, folder: &str) -> String {
        let mut text = self.shown(folder).into_owned();
        if !text.ends_with(|character| self.is_separator(character)) {
            text.push(self.separator());
        }
        text
    }
}

/// A path as the rules of its own machine show it.
pub fn shown(path: &Path) -> String {
    let text = path.to_string_lossy();
    PathStyle::of_text(&text).shown(&text).into_owned()
}

/// Windows maps each character to one upper case character, or leaves it: `ß` stays `ß`.
fn upper(character: char) -> char {
    let mut upper = character.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single,
        _ => character,
    }
}

/// Whether `text` starts with a drive and its root, as `C:\` or `C:/`.
fn has_drive(text: &str) -> bool {
    let mut characters = text.chars();
    let letter = characters
        .next()
        .is_some_and(|letter| letter.is_ascii_alphabetic());
    let mark = characters.next() == Some(DRIVE_MARK);
    let separator = characters
        .next()
        .is_some_and(|separator| PathStyle::Windows.is_separator(separator));
    letter && mark && separator
}

#[cfg(test)]
mod tests {
    use super::*;

    use PathStyle::{Unix, Windows};

    #[test]
    fn an_absolute_path_tells_its_machine() {
        assert_eq!(PathStyle::of_text("/work/app"), Unix);
        assert_eq!(PathStyle::of_text(r"C:\Users\me\app"), Windows);
        assert_eq!(PathStyle::of_text("c:/Users/me"), Windows);
        assert_eq!(PathStyle::of_text(r"\\server\share\app"), Windows);
        assert_eq!(PathStyle::of_text("src/main.rs"), PathStyle::NATIVE);
    }

    #[test]
    fn roots_and_absolute_paths() {
        assert_eq!(Unix.root_len("/work"), Some(1));
        assert_eq!(Unix.root_len(r"C:\work"), None);
        assert!(!Unix.is_absolute("work/app"));
        assert_eq!(Windows.root_len(r"C:\work"), Some(3));
        assert_eq!(Windows.root_len("C:/work"), Some(3));
        assert_eq!(Windows.root_len(r"\\server\share\app"), Some(15));
        assert_eq!(Windows.root_len(r"\\server\share"), Some(14));
        assert_eq!(Windows.root_len(r"\\server"), None);
        assert!(!Windows.is_absolute("/work/app"));
        assert!(!Windows.is_absolute(r"\work"));
        assert!(!Windows.is_absolute("C:work"));
    }

    #[test]
    fn a_text_splits_after_its_last_separator() {
        assert_eq!(Unix.split("/work/app/sr"), ("/work/app/", "sr"));
        assert_eq!(Unix.split(r"C:\work\sr"), ("", r"C:\work\sr"));
        assert_eq!(Windows.split(r"C:\work\app/sr"), (r"C:\work\app/", "sr"));
        assert_eq!(Windows.split("main.rs"), ("", "main.rs"));
    }

    #[test]
    fn a_root_has_no_parent() {
        assert_eq!(Unix.parent("/work/app"), Some("/work"));
        assert_eq!(Unix.parent("/work/"), Some("/"));
        assert_eq!(Unix.parent("/"), None);
        assert_eq!(Unix.parent("src"), Some(""));
        assert_eq!(Unix.parent(""), None);
        assert_eq!(Windows.parent(r"C:\work\app"), Some(r"C:\work"));
        assert_eq!(Windows.parent(r"C:\work"), Some(r"C:\"));
        assert_eq!(Windows.parent("C:/work/"), Some("C:/"));
        assert_eq!(Windows.parent(r"C:\"), None);
        assert_eq!(Windows.parent("C:/"), None);
        assert_eq!(
            Windows.parent(r"\\server\share\app"),
            Some(r"\\server\share\")
        );
        assert_eq!(Windows.parent(r"\\server\share"), None);
        assert_eq!(Windows.file_name(r"C:\work\app.rs"), Some("app.rs"));
        assert_eq!(Windows.file_name(r"C:\"), None);
        assert_eq!(Unix.file_name("/work/.."), None);
    }

    #[test]
    fn a_prefix_strips_by_names_and_on_windows_whatever_their_case() {
        assert_eq!(
            Unix.strip_prefix("/work/app/src/a.rs", "/work/app"),
            Some("src/a.rs")
        );
        assert_eq!(Unix.strip_prefix("/work/app", "/work/app/"), Some(""));
        assert_eq!(Unix.strip_prefix("/work/application", "/work/app"), None);
        assert_eq!(Unix.strip_prefix("/Work/app/a.rs", "/work/app"), None);
        assert_eq!(Unix.strip_prefix("work/app", "/work"), None);
        assert_eq!(
            Windows.strip_prefix(r"c:\users\me\app\src\a.rs", r"C:\Users\me\app"),
            Some(r"src\a.rs")
        );
        assert_eq!(
            Windows.strip_prefix("C:/Users/me/app/src/a.rs", r"C:\Users\me\app"),
            Some("src/a.rs")
        );
        assert_eq!(Windows.strip_prefix(r"D:\Users\me", r"C:\Users"), None);
        assert!(Windows.same_path(r"C:\Users\Me\", "c:/users/me"));
        assert!(!Unix.same_path("/home/Me", "/home/me"));
        // As the daemon compares them: one upper case character for each.
        assert!(Windows.same_name("Ärger", "äRGER"));
        assert!(!Windows.same_name("\u{212A}", "k"), "the Kelvin sign");
        assert!(!Windows.same_name("\u{1E9E}", "ß"), "the capital sharp s");
    }

    #[test]
    fn a_text_resolves_from_a_folder() {
        assert_eq!(Unix.resolve("/work/app", "src/"), "/work/app/src");
        assert_eq!(Unix.resolve("/work/app", "../"), "/work");
        assert_eq!(Unix.resolve("/work/app", "../../../"), "/");
        assert_eq!(Unix.resolve("/work/app", "/etc/"), "/etc");
        assert_eq!(Unix.resolve("/work/app", ""), "/work/app");
        assert_eq!(Unix.resolve("/work/app/../", ""), "/work");
        assert_eq!(Unix.resolve("/work/../../", "app"), "/app");
        assert_eq!(Windows.resolve(r"C:\work\..\", "app"), r"C:\app");
        assert_eq!(
            Windows.resolve(r"C:\work\app", r"src/lib\"),
            r"C:\work\app\src\lib"
        );
        assert_eq!(Windows.resolve(r"C:\work\app", r"..\..\..\"), r"C:\");
        assert_eq!(Windows.resolve(r"C:\work\app", "D:/data/"), r"D:\data");
        assert_eq!(Windows.resolve(r"C:\work\app", r"\data\"), r"C:\data");
    }

    #[test]
    fn windows_paths_show_with_backslashes() {
        assert_eq!(Windows.shown("C:/Users/me"), r"C:\Users\me");
        assert_eq!(Unix.shown(r"/work/a\b"), r"/work/a\b");
        assert_eq!(Windows.folder_text("C:/Users"), r"C:\Users\");
        assert_eq!(Windows.folder_text(r"C:\"), r"C:\");
        assert_eq!(Unix.folder_text("/work"), "/work/");
        assert_eq!(Unix.join("/work", "a.rs"), "/work/a.rs");
        assert_eq!(Windows.join(r"C:\", "a.rs"), r"C:\a.rs");
        assert_eq!(Windows.join(r"C:\work", "a.rs"), r"C:\work\a.rs");
    }

    #[test]
    fn a_verbatim_drive_path_shows_as_a_user_writes_it() {
        assert_eq!(Windows.shown(r"\\?\D:\Images"), r"D:\Images");
        assert_eq!(Windows.shown(r"\\?\UNC\host\share"), r"\\?\UNC\host\share");
        assert_eq!(Unix.shown(r"\\?\D:\Images"), r"\\?\D:\Images");
    }
}

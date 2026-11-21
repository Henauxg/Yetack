//! Shows a file of this computer in the system's file manager: selected in Explorer on Windows
//! and in Finder on macOS. On Linux the file manager opens the file's folder, since no standard
//! way selects a file there.

use std::io;
use std::path::Path;
#[cfg(any(not(any(windows, target_os = "macos")), test))]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Stdio;

#[cfg(target_os = "macos")]
use tokio::process::Command;

/// What the menu item that shows a file says on this system.
pub const SHOW_LABEL: &str = if cfg!(windows) {
    "Show in Explorer"
} else if cfg!(target_os = "macos") {
    "Show in Finder"
} else {
    "Show in the file manager"
};

#[derive(Debug, thiserror::Error)]
pub enum ShowError {
    /// Opening a path on a network share makes Windows send its host a hash of the user's login.
    #[cfg(any(windows, test))]
    #[error("a file on a network share does not open in Explorer")]
    NetworkShare,
    #[cfg(any(windows, test))]
    #[error("no file has a name with a quote")]
    Quote,
    #[cfg(any(not(any(windows, target_os = "macos")), test))]
    #[error("the file is in no folder")]
    NoFolder,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[cfg(not(any(windows, target_os = "macos")))]
    #[error(transparent)]
    Open(#[from] crate::browser::OpenError),
}

/// Shows `path`, absolute, in the file manager.
#[cfg(windows)]
pub async fn show(path: &Path) -> Result<(), ShowError> {
    use std::os::windows::process::CommandExt;

    // Explorer's exit code says nothing: it is 1 even when it shows the file.
    std::process::Command::new("explorer.exe")
        .raw_arg(select_argument(&path.to_string_lossy())?)
        .spawn()?;
    Ok(())
}

/// Explorer's argument that selects the file at `text`. Explorer reads `/select,` and the quoted
/// path as one argument, which the standard quoting would split, so a quote in `text` would end
/// the path and start another argument.
#[cfg(any(windows, test))]
fn select_argument(text: &str) -> Result<String, ShowError> {
    if is_network_share(text) {
        return Err(ShowError::NetworkShare);
    }
    if text.contains('"') {
        return Err(ShowError::Quote);
    }
    Ok(format!("/select,\"{text}\""))
}

#[cfg(target_os = "macos")]
pub async fn show(path: &Path) -> Result<(), ShowError> {
    Command::new("open")
        .arg("-R")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub async fn show(path: &Path) -> Result<(), ShowError> {
    let path = path.to_owned();
    let folder = tokio::task::spawn_blocking(move || folder_of(&path))
        .await
        .map_err(io::Error::other)??;
    crate::browser::open(&folder.to_string_lossy()).await?;
    Ok(())
}

/// The folder that holds `path`, its links resolved. It must be a folder: the opener runs a file
/// with the program its name picks, and the path may come from an agent's text.
#[cfg(any(not(any(windows, target_os = "macos")), test))]
fn folder_of(path: &Path) -> Result<PathBuf, ShowError> {
    let folder = path.parent().ok_or(ShowError::NoFolder)?.canonicalize()?;
    if folder.is_dir() {
        Ok(folder)
    } else {
        Err(ShowError::NoFolder)
    }
}

/// Whether a Windows path may be on a network share: it starts with two separators, `\` or `/`,
/// as `\\host\share`, `//host/share` and `\\?\UNC\host\share` do, and is no verbatim drive path
/// (`\\?\C:\`).
#[cfg(any(windows, test))]
fn is_network_share(text: &str) -> bool {
    const VERBATIM: &str = r"\\?\";
    let is_separator = |character| character == '\\' || character == '/';
    let mut characters = text.chars();
    let two_separators =
        characters.next().is_some_and(is_separator) && characters.next().is_some_and(is_separator);
    let verbatim_drive = text.strip_prefix(VERBATIM).is_some_and(|rest| {
        let mut characters = rest.chars();
        characters
            .next()
            .is_some_and(|letter| letter.is_ascii_alphabetic())
            && characters.next() == Some(':')
    });
    two_separators && !verbatim_drive
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_network_share_is_told_from_a_local_path() {
        assert!(is_network_share(r"\\fileserver\team\notes.txt"));
        assert!(is_network_share(r"\\?\UNC\fileserver\team\notes.txt"));
        assert!(is_network_share("//fileserver/team/notes.txt"));
        assert!(is_network_share(r"\/fileserver\team\notes.txt"));
        assert!(is_network_share(
            r"\\?\GLOBALROOT\Device\Mup\fileserver\team\notes.txt"
        ));
        assert!(!is_network_share(r"\\?\C:\work\notes.txt"));
        assert!(!is_network_share(r"C:\work\notes.txt"));
        assert!(!is_network_share("C:/work/notes.txt"));
    }

    #[test]
    fn a_quote_cannot_add_an_argument_for_explorer() {
        let injected = r#"C:\work\a" /root,\\fileserver\team\ ".txt"#;

        assert!(matches!(select_argument(injected), Err(ShowError::Quote)));
        assert_eq!(
            select_argument(r"C:\work\notes.txt").ok().as_deref(),
            Some(r#"/select,"C:\work\notes.txt""#)
        );
    }

    #[test]
    fn the_folder_to_open_is_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("run.jar");
        std::fs::write(&program, "PK").unwrap();

        let in_program = folder_of(&program.join("notes.md"));
        let in_folder = folder_of(&dir.path().join("notes.md"));

        assert!(
            matches!(in_program, Err(ShowError::NoFolder)),
            "the opener would run {}: {in_program:?}",
            program.display()
        );
        assert_eq!(
            in_folder.ok(),
            Some(dir.path().canonicalize().unwrap()),
            "its links resolved"
        );
    }
}

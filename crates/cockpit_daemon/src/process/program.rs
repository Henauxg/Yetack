//! The command that runs `claude`.
//!
//! On Windows, the default `claude` is the native installer's `claude.exe` when one is on `PATH`
//! or in the installer's folder, and an npm install's `claude.cmd` otherwise. A `.cmd` runs
//! through `cmd.exe`, which reads its command line by rules of its own. A program the daemon's
//! configuration names is run as it is.
//!
//! Every program the daemon starts on Windows starts without a console window: the desktop app
//! that runs the daemon has no console to share, so Windows would open one for each.

use std::ffi::OsString;
use std::io;
use std::path::Path;
#[cfg(any(windows, test))]
use std::path::PathBuf;

use tokio::process::Command;

#[cfg(windows)]
use crate::daemon_config::{DEFAULT_CLAUDE_PROGRAM, HOME_ENV};

/// The native installer's folder, under the home folder.
#[cfg(windows)]
const INSTALLER_FOLDER: [&str; 2] = [".local", "bin"];
#[cfg(any(windows, test))]
const EXE_NAME: &str = "claude.exe";
#[cfg(any(windows, test))]
const NPM_NAME: &str = "claude.cmd";
/// `/d` skips the AutoRun commands of the registry, `/v:off` keeps `!` plain text, and `/s /c`
/// runs the rest of the line once its outer quotes are taken off.
#[cfg(any(windows, test))]
const INTERPRETER_OPTIONS: &str = "/d /v:off /s /c";
/// `cmd.exe` expands `%` even between quotes, and a `"` would end the quoting.
#[cfg(any(windows, test))]
const UNQUOTABLE: [char; 5] = ['"', '%', '\r', '\n', '\0'];

/// Runs `program` with `args`.
#[cfg(unix)]
pub(crate) fn command(program: &Path, args: &[OsString]) -> io::Result<Command> {
    let mut command = Command::new(program);
    command.args(args);
    Ok(command)
}

/// Runs `program` with `args`, through `cmd.exe` for a batch file, without a console window.
/// Fails for an argument that `cmd.exe` cannot take safely.
#[cfg(windows)]
pub(crate) fn command(program: &Path, args: &[OsString]) -> io::Result<Command> {
    let program = if program == Path::new(DEFAULT_CLAUDE_PROGRAM) {
        find_default().unwrap_or_else(|| program.to_owned())
    } else {
        program.to_owned()
    };
    let mut command = if is_batch_file(&program) {
        let line = batch_command_line(&program, args)?;
        let mut command = Command::new(interpreter()?);
        command.raw_arg(line);
        command
    } else {
        let mut command = Command::new(&program);
        command.args(args);
        command
    };
    hide_console(&mut command);
    Ok(command)
}

/// Starts a console program without a console window. It replaces the command's creation flags.
#[cfg(windows)]
pub(crate) fn hide_console(command: &mut Command) {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    command.creation_flags(CREATE_NO_WINDOW);
}

/// The default `claude`, from `PATH` and the home folder.
#[cfg(windows)]
fn find_default() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let folders: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let installer = std::env::var_os(HOME_ENV).map(|home| {
        INSTALLER_FOLDER
            .iter()
            .fold(PathBuf::from(home), |path, name| path.join(name))
    });
    find_claude(&folders, installer.as_deref(), Path::is_file)
}

/// `claude.exe` in a folder of `path`, then in `installer`, then `claude.cmd` in a folder of
/// `path`. A relative folder of `path` names the daemon's current folder, so it is skipped.
#[cfg(any(windows, test))]
fn find_claude(
    path: &[PathBuf],
    installer: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let absolute = || path.iter().filter(|folder| folder.is_absolute());
    let in_path = |name: &str| {
        absolute()
            .map(|folder| folder.join(name))
            .find(|candidate| is_file(candidate))
    };
    let installed = || {
        installer
            .map(|folder| folder.join(EXE_NAME))
            .filter(|candidate| is_file(candidate))
    };
    in_path(EXE_NAME)
        .or_else(installed)
        .or_else(|| in_path(NPM_NAME))
}

#[cfg(windows)]
fn is_batch_file(program: &Path) -> bool {
    program.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    })
}

/// What follows `cmd.exe` to run `program` with `args`: each in quotes, the whole in quotes
/// again for `/s`.
#[cfg(any(windows, test))]
fn batch_command_line(program: &Path, args: &[OsString]) -> io::Result<String> {
    let mut line = format!("{INTERPRETER_OPTIONS} \"");
    line.push_str(&quoted(program.as_os_str())?);
    for arg in args {
        line.push(' ');
        line.push_str(&quoted(arg)?);
    }
    line.push('"');
    Ok(line)
}

/// `word` in double quotes. The batch file hands it on to a program that reads backslashes
/// before a quote as escapes, so a trailing backslash is doubled.
#[cfg(any(windows, test))]
fn quoted(word: &std::ffi::OsStr) -> io::Result<String> {
    let refused = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cmd.exe cannot take {} safely as an argument",
                word.to_string_lossy()
            ),
        )
    };
    let text = word.to_str().ok_or_else(refused)?;
    if text.contains(UNQUOTABLE) {
        return Err(refused());
    }
    let trailing = text.len() - text.trim_end_matches('\\').len();
    Ok(format!("\"{text}{}\"", "\\".repeat(trailing)))
}

/// `cmd.exe` in the system folder, rather than one found on `PATH`.
#[cfg(windows)]
fn interpreter() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::Foundation::MAX_PATH;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    const INTERPRETER: &str = "cmd.exe";

    let mut buffer = [0_u16; MAX_PATH as usize];
    // SAFETY: the buffer holds `MAX_PATH` UTF-16 units, the length passed.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), MAX_PATH) };
    let folder = buffer
        .get(..length as usize)
        .filter(|folder| !folder.is_empty())
        .ok_or_else(io::Error::last_os_error)?;
    Ok(PathBuf::from(OsString::from_wide(folder)).join(INTERPRETER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    const BASE: &str = "/opt";
    #[cfg(windows)]
    const BASE: &str = r"C:\opt";

    fn folder(name: &str) -> PathBuf {
        Path::new(BASE).join(name)
    }

    fn found(path: &[PathBuf], installer: Option<&Path>, files: &[PathBuf]) -> Option<PathBuf> {
        find_claude(path, installer, |candidate| {
            files.iter().any(|file| file == candidate)
        })
    }

    #[test]
    fn the_native_claude_comes_before_an_npm_one() {
        let npm = folder("npm");
        let native = folder("native");
        let path = [npm.clone(), native.clone()];
        let both = [npm.join(NPM_NAME), native.join(EXE_NAME)];

        assert_eq!(found(&path, None, &both), Some(native.join(EXE_NAME)));
        assert_eq!(
            found(&path, None, &[npm.join(NPM_NAME)]),
            Some(npm.join(NPM_NAME))
        );
        assert_eq!(found(&path, None, &[]), None);
    }

    #[test]
    fn the_installers_folder_counts_before_an_npm_claude() {
        let npm = folder("npm");
        let installer = folder("home").join(".local").join("bin");
        let files = [npm.join(NPM_NAME), installer.join(EXE_NAME)];

        let chosen = found(&[npm], Some(&installer), &files);

        assert_eq!(chosen, Some(installer.join(EXE_NAME)));
    }

    #[test]
    fn a_relative_folder_of_the_path_is_skipped() {
        let relative = PathBuf::from("bin");
        let files = [relative.join(EXE_NAME)];

        assert_eq!(found(&[relative], None, &files), None);
    }

    fn line(program: &str, args: &[&str]) -> io::Result<String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        batch_command_line(Path::new(program), &args)
    }

    #[test]
    fn a_batch_file_and_its_arguments_are_quoted_twice_over() {
        let quoted = line(
            r"C:\Users\Jo Doe\AppData\Roaming\npm\claude.cmd",
            &[
                "--output-format",
                "stream-json",
                "--model=opus[1m]",
                "a&b|c<d>e^f(g)",
            ],
        )
        .unwrap();

        assert_eq!(
            quoted,
            r#"/d /v:off /s /c ""C:\Users\Jo Doe\AppData\Roaming\npm\claude.cmd" "--output-format" "stream-json" "--model=opus[1m]" "a&b|c<d>e^f(g)"""#
        );
    }

    #[test]
    fn a_trailing_backslash_is_doubled() {
        let quoted = line(r"C:\npm\claude.cmd", &[r"C:\work\", r"a\b"]).unwrap();

        assert_eq!(
            quoted,
            r#"/d /v:off /s /c ""C:\npm\claude.cmd" "C:\work\\" "a\b"""#
        );
    }

    #[test]
    fn an_argument_cmd_would_change_is_refused() {
        for unsafe_arg in ["say \"hi\"", "%PATH%", "two\nlines", "nul\0byte"] {
            let refused = line(r"C:\npm\claude.cmd", &["--model=opus", unsafe_arg]);

            let error = refused.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{unsafe_arg:?}");
        }
        assert!(line(r"C:\100%\claude.cmd", &[]).is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_batch_file_gets_each_argument_whole() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("echo args.cmd");
        std::fs::write(&script, "@echo off\r\necho [%1] [%2] [%3]\r\n").unwrap();
        let args = ["--model=opus[1m]", "a&b|c<d>e", r"C:\work\"].map(OsString::from);

        let output = command(&script, &args).unwrap().output().await.unwrap();

        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            r#"["--model=opus[1m]"] ["a&b|c<d>e"] ["C:\work\\"]"#
        );
    }
}

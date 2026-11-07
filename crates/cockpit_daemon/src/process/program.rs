//! The command that runs `claude`.
//!
//! On Windows, the default `claude` is the native installer's `claude.exe` when one is on `PATH`
//! or in the installer's folder, and an npm install's `claude.cmd` otherwise. A `.cmd` runs
//! through `cmd.exe`, which reads its command line by rules of its own. A program the daemon's
//! configuration names is run as it is.
//!
//! The sign-in command names the program as a terminal on the daemon's machine can run it: plain
//! `claude` when a terminal's own lookup finds the same program, else its path.
//!
//! Every program the daemon starts on Windows starts without a console window: the desktop app
//! that runs the daemon has no console to share, so Windows would open one for each.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::daemon_config::DEFAULT_CLAUDE_PROGRAM;
#[cfg(windows)]
use crate::daemon_config::HOME_ENV;

/// The native installer's folder, under the home folder.
#[cfg(windows)]
const INSTALLER_FOLDER: [&str; 2] = [".local", "bin"];
#[cfg(any(windows, test))]
const EXE_NAME: &str = "claude.exe";
#[cfg(any(windows, test))]
const NPM_NAME: &str = "claude.cmd";
/// What PowerShell finds for a plain `claude`: its own scripts first, then the extensions of
/// `PATHEXT`.
#[cfg(any(windows, test))]
const POWERSHELL_SCRIPT_EXTENSION: &str = ".ps1";
#[cfg(windows)]
const PATHEXT_ENV: &str = "PATHEXT";
/// Windows' own `PATHEXT`, for a daemon whose environment lacks it.
#[cfg(windows)]
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";
/// `/d` skips the AutoRun commands of the registry, `/v:off` keeps `!` plain text, and `/s /c`
/// runs the rest of the line once its outer quotes are taken off.
#[cfg(any(windows, test))]
const INTERPRETER_OPTIONS: &str = "/d /v:off /s /c";
/// `cmd.exe` expands `%` even between quotes, and a `"` would end the quoting.
#[cfg(any(windows, test))]
const UNQUOTABLE: [char; 5] = ['"', '%', '\r', '\n', '\0'];

/// The program the daemon runs as `claude`, and how a terminal on its machine names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeProgram {
    pub(crate) path: PathBuf,
    pub(crate) name: TerminalName,
}

/// How the daemon starts the program.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Launch {
    /// The program is `claude` itself.
    Direct,
    /// An npm `claude.cmd`, which `cmd.exe` runs: the real `claude` is a child of `cmd.exe`.
    Interpreter,
}

#[cfg(windows)]
impl ClaudeProgram {
    pub(crate) fn launch(&self) -> Launch {
        if is_batch_file(&self.path) {
            Launch::Interpreter
        } else {
            Launch::Direct
        }
    }
}

/// How a terminal on the daemon's machine runs the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalName {
    /// `claude`: the terminal's lookup on `PATH` finds the program the daemon runs.
    Plain,
    Path(PathBuf),
}

/// The program `configured` names. A program other than the default one is named by its path.
/// Looks for files on Windows: it blocks.
#[cfg(unix)]
pub(crate) fn resolve(configured: &Path) -> ClaudeProgram {
    // `Command` looks the default one up on `PATH`, as a shell does.
    let name = if configured == Path::new(DEFAULT_CLAUDE_PROGRAM) {
        TerminalName::Plain
    } else {
        TerminalName::Path(configured.to_owned())
    };
    ClaudeProgram {
        path: configured.to_owned(),
        name,
    }
}

/// The program `configured` names: for the default one, the `claude` found on `PATH` or in the
/// installer's folder. A program other than the default one is named by its path. Looks for
/// files: it blocks.
#[cfg(windows)]
pub(crate) fn resolve(configured: &Path) -> ClaudeProgram {
    if configured != Path::new(DEFAULT_CLAUDE_PROGRAM) {
        return ClaudeProgram {
            path: configured.to_owned(),
            name: TerminalName::Path(configured.to_owned()),
        };
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var_os(HOME_ENV);
    let extensions = std::env::var_os(PATHEXT_ENV);
    resolve_default(&path, home.as_deref(), extensions.as_deref(), Path::is_file)
}

/// The default `claude`, from the `PATH`, home folder and `PATHEXT` given.
#[cfg(windows)]
fn resolve_default(
    path: &std::ffi::OsStr,
    home: Option<&std::ffi::OsStr>,
    extensions: Option<&std::ffi::OsStr>,
    is_file: impl Fn(&Path) -> bool,
) -> ClaudeProgram {
    let folders: Vec<PathBuf> = std::env::split_paths(path).collect();
    let installer = home.map(|home| {
        INSTALLER_FOLDER
            .iter()
            .fold(PathBuf::from(home), |path, name| path.join(name))
    });
    let Some(found) = find_claude(&folders, installer.as_deref(), &is_file) else {
        return ClaudeProgram {
            path: PathBuf::from(DEFAULT_CLAUDE_PROGRAM),
            name: TerminalName::Plain,
        };
    };
    let extensions = extensions
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(DEFAULT_PATHEXT);
    let extensions: Vec<&str> = extensions.split(';').collect();
    let name = terminal_name(&found, &folders, &extensions, &is_file);
    ClaudeProgram { path: found, name }
}

/// `Plain` when PowerShell's lookup of `claude` on `path` finds `found`: in each folder in turn,
/// `claude.ps1`, then `claude` with each of `extensions`. An npm install puts a `claude.ps1`
/// beside its `claude.cmd`, which PowerShell's execution policy may refuse to run, so the npm
/// `claude.cmd` is named by its path.
#[cfg(any(windows, test))]
fn terminal_name(
    found: &Path,
    path: &[PathBuf],
    extensions: &[&str],
    is_file: impl Fn(&Path) -> bool,
) -> TerminalName {
    let script = std::iter::once(POWERSHELL_SCRIPT_EXTENSION);
    let extensions: Vec<String> = script
        .chain(extensions.iter().copied())
        .filter(|extension| !extension.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let candidates = path
        .iter()
        .filter(|folder| folder.is_absolute())
        .flat_map(|folder| {
            let names = extensions
                .iter()
                .map(|extension| format!("{DEFAULT_CLAUDE_PROGRAM}{extension}"));
            names.map(|name| folder.join(name))
        });
    let first = candidates.into_iter().find(|candidate| is_file(candidate));
    if first.as_deref() == Some(found) {
        TerminalName::Plain
    } else {
        TerminalName::Path(found.to_owned())
    }
}

/// Runs the program `configured` names with `args`. Looks for files on Windows: it blocks.
pub(crate) fn command(configured: &Path, args: &[OsString]) -> io::Result<Command> {
    resolved_command(&resolve(configured), args)
}

/// Runs `program` with `args`.
#[cfg(unix)]
pub(crate) fn resolved_command(program: &ClaudeProgram, args: &[OsString]) -> io::Result<Command> {
    let mut command = Command::new(&program.path);
    command.args(args);
    Ok(command)
}

/// Runs `program` with `args`, through `cmd.exe` for a batch file, without a console window.
/// Fails for an argument that `cmd.exe` cannot take safely.
#[cfg(windows)]
pub(crate) fn resolved_command(program: &ClaudeProgram, args: &[OsString]) -> io::Result<Command> {
    let mut command = match program.launch() {
        Launch::Interpreter => {
            let line = batch_command_line(&program.path, args)?;
            let mut command = Command::new(interpreter()?);
            command.raw_arg(line);
            command
        }
        Launch::Direct => {
            let mut command = Command::new(&program.path);
            command.args(args);
            command
        }
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

    /// Windows' own `PATHEXT`, as a terminal reads it.
    const PATHEXT: [&str; 4] = [".COM", ".EXE", ".BAT", ".CMD"];

    fn named(found: &Path, path: &[PathBuf], files: &[PathBuf]) -> TerminalName {
        terminal_name(found, path, &PATHEXT, |candidate| {
            files.iter().any(|file| file == candidate)
        })
    }

    #[cfg(unix)]
    #[test]
    fn the_default_claude_is_named_plainly_and_a_configured_one_by_its_path() {
        let configured = folder("bin").join("claude-dev");

        let default = resolve(Path::new(DEFAULT_CLAUDE_PROGRAM));
        let other = resolve(&configured);

        assert_eq!(default.name, TerminalName::Plain);
        assert_eq!(other.path, configured);
        assert_eq!(other.name, TerminalName::Path(configured.clone()));
    }

    #[test]
    fn a_claude_the_terminal_finds_first_is_named_plainly() {
        let native = folder("native");
        let exe = native.join(EXE_NAME);

        let name = named(&exe, &[folder("tools"), native], std::slice::from_ref(&exe));

        assert_eq!(name, TerminalName::Plain);
    }

    #[test]
    fn a_claude_in_the_installers_folder_alone_is_named_by_its_path() {
        let exe = folder("home").join(".local").join("bin").join(EXE_NAME);

        let name = named(&exe, &[folder("tools")], std::slice::from_ref(&exe));

        assert_eq!(name, TerminalName::Path(exe));
    }

    #[test]
    fn a_claude_behind_another_on_the_path_is_named_by_its_path() {
        let npm = folder("npm");
        let native = folder("native");
        let exe = native.join(EXE_NAME);
        let files = [npm.join(NPM_NAME), exe.clone()];

        let name = named(&exe, &[npm, native], &files);

        assert_eq!(name, TerminalName::Path(exe));
    }

    #[test]
    fn an_npm_claude_beside_its_powershell_script_is_named_by_its_path() {
        let npm = folder("npm");
        let batch = npm.join(NPM_NAME);
        let with_script = [npm.join("claude.ps1"), batch.clone()];

        let beside_script = named(&batch, std::slice::from_ref(&npm), &with_script);
        let alone = named(&batch, &[npm], std::slice::from_ref(&batch));

        assert_eq!(beside_script, TerminalName::Path(batch));
        assert_eq!(alone, TerminalName::Plain);
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

    #[cfg(windows)]
    #[test]
    fn the_default_claude_is_found_and_named_from_the_environment() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let installer = home.join(".local").join("bin");
        let npm = temp.path().join("npm");
        for (folder, name) in [
            (&installer, EXE_NAME),
            (&npm, NPM_NAME),
            (&npm, "claude.ps1"),
        ] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join(name), "").unwrap();
        }
        let path = std::env::join_paths([&npm]).unwrap();
        let installer_on_path = std::env::join_paths([&installer, &npm]).unwrap();
        let resolve = |path: &std::ffi::OsStr, home: Option<&Path>| {
            resolve_default(path, home.map(Path::as_os_str), None, Path::is_file)
        };

        let off_path = resolve(&path, Some(&home));
        let on_path = resolve(&installer_on_path, Some(&home));
        let npm_alone = resolve(&path, None);
        let none = resolve(std::ffi::OsStr::new(""), None);

        let exe = installer.join(EXE_NAME);
        assert_eq!(off_path.path, exe);
        assert_eq!(off_path.name, TerminalName::Path(exe.clone()));
        assert_eq!(on_path.name, TerminalName::Plain);
        assert_eq!(npm_alone.name, TerminalName::Path(npm.join(NPM_NAME)));
        assert_eq!(none.path, PathBuf::from(DEFAULT_CLAUDE_PROGRAM));
        assert_eq!(none.name, TerminalName::Plain);
    }
}

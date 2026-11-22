//! The shells a terminal can run: the default one, and the others the daemon finds on its machine.
//!
//! The default is `YETACK_SHELL`. Without it, it is the account's login shell on Unix, then
//! `$SHELL`, then `/bin/sh`. On Windows, it is PowerShell 7, then Windows PowerShell, then
//! `cmd.exe`. The others come from `/etc/shells` on Unix, and from where Windows and the
//! installers put them on Windows. Finding them reads files: it blocks.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use cockpit_protocol::ShellChoice;

#[cfg(windows)]
use crate::process::program;

/// The shell terminals run when the app names none. It wins over the others.
const SHELL_ENV: &str = "YETACK_SHELL";

/// The user's shell on Unix: the default after the login shell, and what a terminal tells the
/// programs it starts.
#[cfg(unix)]
pub(super) const USER_SHELL_ENV: &str = "SHELL";

/// The name of a shell whose program's name is not text.
const UNNAMED_SHELL: &str = "shell";

/// The shells of `/etc/shells` that the app offers. The others there are restricted, plain POSIX
/// or no shells at all, such as `rbash`, `dash`, `tmux` or `nologin`.
#[cfg(any(unix, test))]
const INTERACTIVE: [&str; 10] = [
    "bash", "zsh", "fish", "nu", "pwsh", "ksh", "mksh", "tcsh", "elvish", "xonsh",
];

/// The shell when nothing names one.
#[cfg(unix)]
const FALLBACK_SHELL: &str = "/bin/sh";

#[cfg(unix)]
const ETC_SHELLS: &str = "/etc/shells";

#[cfg(windows)]
const PWSH: &str = "pwsh.exe";

/// As Windows Terminal starts Git Bash: an interactive login shell reads the profile that sets
/// Git's `PATH`.
#[cfg(windows)]
const GIT_BASH_ARGS: &[&str] = &["--login", "-i"];

/// A program a terminal can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Shell {
    pub(super) program: PathBuf,
    /// What follows the program on its command line.
    pub(super) args: &'static [&'static str],
    /// What the app shows.
    pub(super) name: String,
    pub(super) default: bool,
}

impl Shell {
    fn new(program: PathBuf, name: String) -> Self {
        Self {
            program,
            args: &[],
            name,
            default: false,
        }
    }

    /// The terminal's title until the shell sets one: the program's name, such as "zsh".
    pub(super) fn title(&self) -> &str {
        self.program
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(UNNAMED_SHELL)
    }

    pub(super) fn into_choice(self) -> ShellChoice {
        ShellChoice {
            program: self.program,
            name: self.name,
            default: self.default,
        }
    }
}

/// The shell among `shells` whose program is `program`, or the default one for `None`.
pub(super) fn pick(shells: Vec<Shell>, program: Option<&Path>) -> Result<Shell, String> {
    let mut shells = shells.into_iter();
    match program {
        None => shells
            .find(|shell| shell.default)
            .ok_or_else(|| "this machine has no shell to run".to_owned()),
        Some(program) => shells
            .find(|shell| shell.program == program)
            .ok_or_else(|| format!("{} is not a shell of this machine", program.display())),
    }
}

/// The shells found, in the order the app shows them, one of them the default.
pub(super) fn find() -> Vec<Shell> {
    let mut shells = find_on_this_system();
    // The protocol carries paths as text.
    shells.retain(|shell| shell.program.to_str().is_some());
    shells
}

#[cfg(unix)]
fn find_on_this_system() -> Vec<Shell> {
    let set = |name| {
        std::env::var(name)
            .ok()
            .filter(|value: &String| !value.is_empty())
            .map(PathBuf::from)
    };
    // `$SHELL` comes after the login shell: a `nix develop` shell sets it to a bash built without
    // readline, which shows a prompt's `\[` and `\]` and cannot edit a line.
    let default = set(SHELL_ENV)
        .or_else(login_shell)
        .or_else(|| set(USER_SHELL_ENV))
        .unwrap_or_else(|| PathBuf::from(FALLBACK_SHELL));
    let listed = std::fs::read_to_string(ETC_SHELLS).unwrap_or_default();
    unix_shells(default, &listed, |path| {
        std::fs::canonicalize(path)
            .ok()
            .filter(|file| file.is_file())
    })
}

/// The daemon's user's login shell, as the system's user database gives it (`/etc/passwd`, a
/// directory service, or macOS's), when it names a file that exists.
#[cfg(unix)]
fn login_shell() -> Option<PathBuf> {
    use nix::unistd::User;

    let user = User::from_uid(nix::unistd::getuid()).ok()??;
    usable_login_shell(user.shell)
}

/// `shell` when it names a file, by a path the protocol can carry.
#[cfg(unix)]
fn usable_login_shell(shell: PathBuf) -> Option<PathBuf> {
    Some(shell).filter(|shell| shell.to_str().is_some() && shell.is_file())
}

/// The shells of `listed`, the text of `/etc/shells`, that [`INTERACTIVE`] names, with `default`
/// marked, or put first when it is not among them. `canonical` gives a program's real file, or
/// `None` when it does not exist: two paths to one file are one shell. The default is a listed
/// shell only under the same name, since a shell can act on the name it runs under.
#[cfg(any(unix, test))]
fn unix_shells(
    default: PathBuf,
    listed: &str,
    canonical: impl Fn(&Path) -> Option<PathBuf>,
) -> Vec<Shell> {
    let default_file = canonical(&default);
    let default_name = default.file_name();
    let mut files = Vec::new();
    let mut shells = Vec::new();
    for line in listed.lines().map(str::trim) {
        let program = Path::new(line);
        let Some(name) = program.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        // A comment, or no path.
        if !line.starts_with('/') || !INTERACTIVE.contains(&name) {
            continue;
        }
        let Some(file) = canonical(program) else {
            continue;
        };
        if files.contains(&file) {
            continue;
        }
        let mut shell = Shell::new(program.to_owned(), name.to_owned());
        shell.default =
            default_file.as_ref() == Some(&file) && default_name == Some(OsStr::new(name));
        files.push(file);
        shells.push(shell);
    }
    if !shells.iter().any(|shell| shell.default) {
        let name = default
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(UNNAMED_SHELL)
            .to_owned();
        let mut shell = Shell::new(default, name);
        shell.default = true;
        shells.insert(0, shell);
    }
    shells
}

#[cfg(windows)]
fn find_on_this_system() -> Vec<Shell> {
    use crate::daemon_config::LOCAL_APP_DATA_ENV;

    let set = |name| {
        std::env::var(name)
            .ok()
            .filter(|value: &String| !value.is_empty())
    };
    let system = program::system_folder().unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot find Windows' system folder");
        PathBuf::from(r"C:\Windows\System32")
    });
    let path = set(program::PATH_ENV).unwrap_or_default();
    let machine = WindowsMachine {
        system,
        program_files: set("ProgramFiles").map(PathBuf::from),
        local_programs: set(LOCAL_APP_DATA_ENV).map(|folder| Path::new(&folder).join("Programs")),
        path: std::env::split_paths(&path).collect(),
        has_wsl_distribution: has_wsl_distribution(),
    };
    windows_shells(set(SHELL_ENV).map(PathBuf::from), &machine, Path::is_file)
}

/// Where a Windows machine keeps its shells.
#[cfg(windows)]
struct WindowsMachine {
    /// Windows' system folder, `C:\Windows\System32`.
    system: PathBuf,
    /// `C:\Program Files`.
    program_files: Option<PathBuf>,
    /// The user's own programs, in `%LOCALAPPDATA%\Programs`.
    local_programs: Option<PathBuf>,
    /// The folders of `PATH`.
    path: Vec<PathBuf>,
    /// WSL has a Linux distribution to run. Without one, `wsl.exe` only says how to install one.
    has_wsl_distribution: bool,
}

/// The shells of `machine` that `is_file` finds, with `configured` the default. Without it, the
/// first shell is: PowerShell 7, else Windows PowerShell, else `cmd.exe`.
#[cfg(windows)]
fn windows_shells(
    configured: Option<PathBuf>,
    machine: &WindowsMachine,
    is_file: impl Fn(&Path) -> bool,
) -> Vec<Shell> {
    let mut shells = Vec::new();
    // `PATH` first, as typing `pwsh` finds it, then the installer's folder.
    let on_path = machine
        .path
        .iter()
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(PWSH));
    let installed = machine
        .program_files
        .iter()
        .map(|folder| folder.join("PowerShell").join("7").join(PWSH));
    if let Some(program) = on_path.chain(installed).find(|program| is_file(program)) {
        shells.push(Shell::new(program, "PowerShell 7".to_owned()));
    }
    let windows_powershell = machine
        .system
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if is_file(&windows_powershell) {
        shells.push(Shell::new(
            windows_powershell,
            "Windows PowerShell".to_owned(),
        ));
    }
    // Every Windows has it: it stays the default when nothing else is found.
    shells.push(Shell::new(
        machine.system.join(program::INTERPRETER),
        "Command Prompt".to_owned(),
    ));
    let git_folders = machine.program_files.iter().chain(&machine.local_programs);
    let git_bash = git_folders
        .map(|folder| folder.join("Git").join("bin").join("bash.exe"))
        .find(|program| is_file(program));
    if let Some(program) = git_bash {
        let mut shell = Shell::new(program, "Git Bash".to_owned());
        shell.args = GIT_BASH_ARGS;
        shells.push(shell);
    }
    let wsl = machine.system.join("wsl.exe");
    if machine.has_wsl_distribution && is_file(&wsl) {
        shells.push(Shell::new(wsl, "WSL".to_owned()));
    }
    let Some(configured) = configured else {
        if let Some(first) = shells.first_mut() {
            first.default = true;
        }
        return shells;
    };
    // Windows' paths ignore case, and take `/` for `\`.
    let same = |shell: &Shell| {
        let found = shell.program.components();
        let wanted = configured.components();
        found.clone().count() == wanted.clone().count()
            && found
                .zip(wanted)
                .all(|(found, wanted)| found.as_os_str().eq_ignore_ascii_case(wanted.as_os_str()))
    };
    if let Some(shell) = shells.iter_mut().find(|shell| same(shell)) {
        shell.default = true;
    } else {
        let name = configured
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(UNNAMED_SHELL)
            .to_owned();
        let mut shell = Shell::new(configured, name);
        shell.default = true;
        shells.insert(0, shell);
    }
    shells
}

/// Whether WSL has a default distribution for the daemon's user, which it keeps in the registry.
#[cfg(windows)]
fn has_wsl_distribution() -> bool {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};

    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Lxss");
    let value = wide("DefaultDistribution");
    // SAFETY: both names end with a NUL. With no buffer and no size, the call only checks that the
    // value exists.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    status == ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file system where each of `files` is its own real file, and `links` lead to others.
    fn canonical<'files>(
        files: &'files [&str],
        links: &'files [(&str, &str)],
    ) -> impl Fn(&Path) -> Option<PathBuf> + 'files {
        move |path| {
            let path = path.to_str()?;
            let target = links
                .iter()
                .find(|(link, _)| *link == path)
                .map_or(path, |(_, target)| *target);
            files.contains(&target).then(|| PathBuf::from(target))
        }
    }

    fn summary(shells: &[Shell]) -> Vec<(&str, &str, bool)> {
        shells
            .iter()
            .map(|shell| {
                let program = shell.program.to_str().unwrap();
                (program, shell.name.as_str(), shell.default)
            })
            .collect()
    }

    #[test]
    fn etc_shells_gives_each_interactive_shell_once_with_the_default_marked() {
        let listed = "# /etc/shells: valid login shells\n/bin/sh\n/bin/bash\n/usr/bin/bash\n\
                      /bin/rbash\n/usr/bin/dash\n/usr/bin/tmux\n/usr/bin/zsh\n/bin/zsh\n\
                      /usr/bin/fish\n  \n/usr/local/bin/nu\n";
        let files = canonical(
            &[
                "/usr/bin/bash",
                "/usr/bin/zsh",
                "/usr/bin/fish",
                "/usr/bin/dash",
            ],
            &[
                ("/bin/bash", "/usr/bin/bash"),
                ("/bin/zsh", "/usr/bin/zsh"),
                ("/bin/sh", "/usr/bin/dash"),
            ],
        );

        let shells = unix_shells(PathBuf::from("/bin/zsh"), listed, files);

        assert_eq!(
            summary(&shells),
            [
                ("/bin/bash", "bash", false),
                ("/usr/bin/zsh", "zsh", true),
                ("/usr/bin/fish", "fish", false),
            ]
        );
    }

    #[test]
    fn a_default_that_etc_shells_does_not_list_comes_first() {
        let files = canonical(&["/usr/bin/bash", "/home/sam/.cargo/bin/nu"], &[]);

        let shells = unix_shells(
            PathBuf::from("/home/sam/.cargo/bin/nu"),
            "/usr/bin/bash\n",
            files,
        );

        assert_eq!(
            summary(&shells),
            [
                ("/home/sam/.cargo/bin/nu", "nu", true),
                ("/usr/bin/bash", "bash", false),
            ]
        );
    }

    /// bash named `sh` is a POSIX shell, and named `rbash` a restricted one: a default that is
    /// another name of a listed shell's file runs under its own name.
    #[test]
    fn a_default_that_shares_a_file_with_a_listed_shell_keeps_its_name() {
        let files = canonical(
            &["/usr/bin/bash"],
            &[("/bin/sh", "/usr/bin/bash"), ("/bin/bash", "/usr/bin/bash")],
        );

        let shells = unix_shells(PathBuf::from("/bin/sh"), "/bin/sh\n/bin/bash\n", files);

        assert_eq!(
            summary(&shells),
            [("/bin/sh", "sh", true), ("/bin/bash", "bash", false)]
        );
    }

    /// `YETACK_SHELL` may name a program that is not there: the terminal then says it cannot
    /// start it.
    #[test]
    fn a_missing_default_still_is_the_default() {
        let files = canonical(&["/usr/bin/bash"], &[]);

        let shells = unix_shells(PathBuf::from("/opt/gone/zsh"), "/usr/bin/bash\n", files);

        assert_eq!(
            summary(&shells),
            [
                ("/opt/gone/zsh", "zsh", true),
                ("/usr/bin/bash", "bash", false)
            ]
        );
    }

    #[cfg(windows)]
    fn windows_machine() -> WindowsMachine {
        WindowsMachine {
            system: PathBuf::from(r"C:\Windows\System32"),
            program_files: Some(PathBuf::from(r"C:\Program Files")),
            local_programs: Some(PathBuf::from(r"C:\Users\sam\AppData\Local\Programs")),
            path: vec![
                PathBuf::from(r"C:\Windows\System32"),
                PathBuf::from(r"tools"),
                PathBuf::from(r"C:\Users\sam\.dotnet\tools"),
            ],
            has_wsl_distribution: true,
        }
    }

    #[cfg(windows)]
    fn windows_files(files: &[&str]) -> impl Fn(&Path) -> bool {
        let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
        move |path| files.iter().any(|file| file == path)
    }

    #[cfg(windows)]
    #[test]
    fn windows_offers_every_shell_it_has_with_powershell_7_the_default() {
        let files = windows_files(&[
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\System32\cmd.exe",
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Windows\System32\wsl.exe",
        ]);

        let shells = windows_shells(None, &windows_machine(), files);

        assert_eq!(
            summary(&shells),
            [
                (
                    r"C:\Program Files\PowerShell\7\pwsh.exe",
                    "PowerShell 7",
                    true
                ),
                (
                    r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                    "Windows PowerShell",
                    false
                ),
                (r"C:\Windows\System32\cmd.exe", "Command Prompt", false),
                (r"C:\Program Files\Git\bin\bash.exe", "Git Bash", false),
                (r"C:\Windows\System32\wsl.exe", "WSL", false),
            ]
        );
        let git_bash = shells.iter().find(|shell| shell.name == "Git Bash");
        assert_eq!(git_bash.map(|shell| shell.args), Some(GIT_BASH_ARGS));
    }

    #[cfg(windows)]
    #[test]
    fn powershell_7_on_the_path_comes_before_the_installers_one() {
        let files = windows_files(&[
            r"C:\Users\sam\.dotnet\tools\pwsh.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"tools\pwsh.exe",
        ]);

        let shells = windows_shells(None, &windows_machine(), files);

        assert_eq!(
            summary(&shells),
            [
                (r"C:\Users\sam\.dotnet\tools\pwsh.exe", "PowerShell 7", true),
                (r"C:\Windows\System32\cmd.exe", "Command Prompt", false),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn without_powershell_7_windows_powershell_is_the_default() {
        let files = windows_files(&[
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\System32\cmd.exe",
        ]);

        let shells = windows_shells(None, &windows_machine(), files);

        assert_eq!(
            summary(&shells),
            [
                (
                    r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                    "Windows PowerShell",
                    true
                ),
                (r"C:\Windows\System32\cmd.exe", "Command Prompt", false),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn without_powershell_the_command_prompt_is_the_default() {
        let mut machine = windows_machine();
        machine.has_wsl_distribution = false;
        let files = windows_files(&[
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\wsl.exe",
            r"C:\Users\sam\AppData\Local\Programs\Git\bin\bash.exe",
        ]);

        let shells = windows_shells(None, &machine, files);

        assert_eq!(
            summary(&shells),
            [
                (r"C:\Windows\System32\cmd.exe", "Command Prompt", true),
                (
                    r"C:\Users\sam\AppData\Local\Programs\Git\bin\bash.exe",
                    "Git Bash",
                    false
                ),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_configured_shell_is_the_default_whatever_its_case() {
        let files = windows_files(&[
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\System32\cmd.exe",
        ]);
        let configured = PathBuf::from(r"c:\windows\system32\CMD.EXE");

        let shells = windows_shells(Some(configured), &windows_machine(), files);

        let defaults: Vec<&str> = shells
            .iter()
            .filter(|shell| shell.default)
            .map(|shell| shell.name.as_str())
            .collect();
        assert_eq!(defaults, ["Command Prompt"]);
        assert_eq!(shells.len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn the_configured_shell_is_the_default_whatever_its_separators() {
        let files = windows_files(&[r"C:\Windows\System32\cmd.exe"]);
        let configured = PathBuf::from("c:/Windows/System32/cmd.exe");

        let shells = windows_shells(Some(configured), &windows_machine(), files);

        assert_eq!(
            summary(&shells),
            [(r"C:\Windows\System32\cmd.exe", "Command Prompt", true)]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_configured_shell_found_nowhere_else_comes_first() {
        let files = windows_files(&[r"C:\Windows\System32\cmd.exe"]);
        let configured = PathBuf::from(r"D:\tools\nu.exe");

        let shells = windows_shells(Some(configured), &windows_machine(), files);

        assert_eq!(
            summary(&shells),
            [
                (r"D:\tools\nu.exe", "nu", true),
                (r"C:\Windows\System32\cmd.exe", "Command Prompt", false),
            ]
        );
    }

    #[test]
    fn a_shell_is_picked_by_its_program_or_as_the_default() {
        let mut zsh = Shell::new(PathBuf::from("/usr/bin/zsh"), "zsh".to_owned());
        zsh.default = true;
        let bash = Shell::new(PathBuf::from("/usr/bin/bash"), "bash".to_owned());
        let shells = vec![bash, zsh];

        let default = pick(shells.clone(), None);
        let named = pick(shells.clone(), Some(Path::new("/usr/bin/bash")));
        let unknown = pick(shells, Some(Path::new("/tmp/evil")));

        assert_eq!(default.map(|shell| shell.name), Ok("zsh".to_owned()));
        assert_eq!(named.map(|shell| shell.name), Ok("bash".to_owned()));
        assert_eq!(
            unknown,
            Err(format!(
                "{} is not a shell of this machine",
                Path::new("/tmp/evil").display()
            ))
        );
    }

    /// The daemon's user has a login shell on any machine that runs the tests.
    #[cfg(unix)]
    #[test]
    fn the_users_login_shell_is_a_file() {
        let shell = login_shell();

        assert!(
            shell.is_some(),
            "the user database gives no login shell that is an existing file"
        );
    }

    /// The protocol carries paths as text, so [`find`] leaves out a program that is not UTF-8: such
    /// a login shell gives way to `$SHELL`, else `/bin/sh`, rather than leave no default.
    #[cfg(unix)]
    #[test]
    fn a_login_shell_whose_path_is_not_utf_8_is_passed_over() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let shell = temp.path().join(OsStr::from_bytes(b"zsh\xff"));
        std::fs::write(&shell, "").unwrap();

        let usable = usable_login_shell(shell);

        assert_eq!(usable, None);
    }

    /// Whatever the machine, one shell is the default.
    #[test]
    fn this_machine_has_one_default_shell() {
        let shells = find();

        let defaults = shells.iter().filter(|shell| shell.default).count();
        assert_eq!(defaults, 1, "{shells:?}");
    }
}

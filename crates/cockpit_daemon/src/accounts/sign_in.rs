use std::path::Path;

use cockpit_protocol::{Account, AccountState, SignIn, Timestamp};

use crate::accounts::route;
use crate::daemon_config::{CLAUDE_CONFIG_DIR_ENV, DEFAULT_CLAUDE_PROGRAM};
use crate::process::program::TerminalName;

/// Where Claude Code keeps a login in its folder, outside the macOS Keychain.
const CREDENTIALS_FILE: &str = ".credentials.json";
/// The characters PowerShell reads as a single quote: the ASCII one and four typographic ones.
const POWERSHELL_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'];
/// Claude Code's command that signs in and exits, after `claude`.
const AUTH_LOGIN: &str = "auth login";

/// The shell a sign-in command is written for: the daemon's machine's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignInShell {
    #[cfg_attr(
        all(windows, not(test)),
        expect(dead_code, reason = "a Windows daemon writes for PowerShell")
    )]
    Posix,
    #[cfg_attr(
        not(any(windows, test)),
        expect(dead_code, reason = "only a Windows daemon writes for PowerShell")
    )]
    PowerShell,
}

impl SignInShell {
    #[cfg(windows)]
    pub(crate) const CURRENT: Self = Self::PowerShell;
    #[cfg(not(windows))]
    pub(crate) const CURRENT: Self = Self::Posix;

    /// The command that runs `program auth login` with `folder` as its config folder, or with
    /// none. In PowerShell, the variable is removed after, so the terminal's next `claude` uses
    /// its own folder again.
    pub(crate) fn command(self, folder: Option<&Path>, program: &TerminalName) -> String {
        let program = match program {
            TerminalName::Plain => DEFAULT_CLAUDE_PROGRAM.to_owned(),
            TerminalName::Path(path) => {
                let quoted = self.quoted(&path.to_string_lossy());
                match self {
                    Self::Posix => quoted,
                    // A quoted path is a string to PowerShell: `&` runs it.
                    Self::PowerShell => format!("& {quoted}"),
                }
            }
        };
        let login = format!("{program} {AUTH_LOGIN}");
        let Some(folder) = folder else {
            return login;
        };
        let folder = self.quoted(&folder.to_string_lossy());
        match self {
            Self::Posix => format!("{CLAUDE_CONFIG_DIR_ENV}={folder} {login}"),
            Self::PowerShell => format!(
                "$env:{CLAUDE_CONFIG_DIR_ENV} = {folder}; {login}; Remove-Item Env:{CLAUDE_CONFIG_DIR_ENV}"
            ),
        }
    }

    /// `text` in single quotes, which keep the shell from reading anything in it.
    fn quoted(self, text: &str) -> String {
        match self {
            Self::Posix => format!("'{}'", text.replace('\'', r"'\''")),
            Self::PowerShell => {
                let mut quoted = String::with_capacity(text.len() + 2);
                quoted.push('\'');
                for character in text.chars() {
                    if POWERSHELL_QUOTES.contains(&character) {
                        quoted.push(character);
                    }
                    quoted.push(character);
                }
                quoted.push('\'');
                quoted
            }
        }
    }
}

/// Where Claude Code can keep a login, which decides what a missing credentials file means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginStore {
    File,
    /// The macOS Keychain, or the folder's file when the Keychain cannot be used.
    #[cfg_attr(
        not(any(target_os = "macos", test)),
        expect(dead_code, reason = "only macOS has the Keychain")
    )]
    KeychainOrFile,
}

impl LoginStore {
    #[cfg(target_os = "macos")]
    const CURRENT: Self = Self::KeychainOrFile;
    #[cfg(not(target_os = "macos"))]
    const CURRENT: Self = Self::File;

    fn sign_in(self, has_credentials_file: bool) -> SignIn {
        match (has_credentials_file, self) {
            (true, _) => SignIn::SignedIn,
            (false, Self::File) => SignIn::NotSignedIn,
            (false, Self::KeychainOrFile) => SignIn::Unknown,
        }
    }
}

/// The command that signs in to the account with `program`, the `claude` the daemon runs. The
/// default account's `claude` gets the daemon's own `CLAUDE_CONFIG_DIR`, so its command sets the
/// variable only when the daemon's environment does.
pub(crate) fn sign_in_command(
    account: &Account,
    shell: SignInShell,
    program: &TerminalName,
) -> String {
    let variable = if account.id.is_default() {
        std::env::var_os(CLAUDE_CONFIG_DIR_ENV).map(|_| account.folder.as_path())
    } else {
        Some(account.folder.as_path())
    };
    shell.command(variable, program)
}

/// When the account's credentials file was last written, as by a sign-in. `None` without the
/// file, as on macOS, where the Keychain holds the login. Reads the file's metadata: it blocks.
pub(crate) fn login_written(account: &Account) -> Option<Timestamp> {
    let metadata = std::fs::metadata(account.folder.join(CREDENTIALS_FILE)).ok()?;
    Timestamp::from_system_time(metadata.modified().ok()?)
}

/// The account, whether its folder holds a login, how to sign in to it with `program`, and its
/// route. Looks at the credentials file's metadata, never at its content, and reads the switches
/// of the account's `settings.json`: it blocks.
pub(crate) fn account_state(
    account: Account,
    shell: SignInShell,
    program: &TerminalName,
) -> AccountState {
    let has_credentials_file = account.folder.join(CREDENTIALS_FILE).is_file();
    let sign_in_command = sign_in_command(&account, shell, program);
    AccountState {
        sign_in: LoginStore::CURRENT.sign_in(has_credentials_file),
        sign_in_command,
        route: route(&account),
        account,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{AccountId, CredentialSource};

    use super::*;

    const PLAIN: TerminalName = TerminalName::Plain;

    fn path(text: &str) -> TerminalName {
        TerminalName::Path(PathBuf::from(text))
    }

    #[test]
    fn a_posix_command_quotes_the_folder_in_single_quotes() {
        let plain = SignInShell::Posix.command(Some(Path::new("/home/sam/.claude-work")), &PLAIN);
        let tricky =
            SignInShell::Posix.command(Some(Path::new("/home/sam/it's $HOME `x`")), &PLAIN);

        assert_eq!(
            plain,
            "CLAUDE_CONFIG_DIR='/home/sam/.claude-work' claude auth login"
        );
        assert_eq!(
            tricky,
            r#"CLAUDE_CONFIG_DIR='/home/sam/it'\''s $HOME `x`' claude auth login"#
        );
    }

    #[test]
    fn a_powershell_command_doubles_every_kind_of_single_quote() {
        let plain =
            SignInShell::PowerShell.command(Some(Path::new(r"C:\Users\sam\.claude-work")), &PLAIN);
        let tricky = SignInShell::PowerShell.command(
            Some(Path::new("C:\\Users\\sam\\it's \u{2019}$env:x\u{2018}")),
            &PLAIN,
        );

        assert_eq!(
            plain,
            r"$env:CLAUDE_CONFIG_DIR = 'C:\Users\sam\.claude-work'; claude auth login; Remove-Item Env:CLAUDE_CONFIG_DIR"
        );
        assert_eq!(
            tricky,
            "$env:CLAUDE_CONFIG_DIR = 'C:\\Users\\sam\\it''s \u{2019}\u{2019}$env:x\u{2018}\u{2018}'; claude auth login; Remove-Item Env:CLAUDE_CONFIG_DIR"
        );
    }

    #[test]
    fn a_posix_command_names_a_program_off_the_path_by_its_quoted_path() {
        let alone = SignInShell::Posix.command(None, &path("/opt/claude/bin/claude"));
        let tricky = SignInShell::Posix.command(
            Some(Path::new("/home/sam/.claude-work")),
            &path("/home/sam/it's $HOME/claude"),
        );

        assert_eq!(alone, "'/opt/claude/bin/claude' auth login");
        assert_eq!(
            tricky,
            r"CLAUDE_CONFIG_DIR='/home/sam/.claude-work' '/home/sam/it'\''s $HOME/claude' auth login"
        );
    }

    #[test]
    fn a_powershell_command_runs_a_program_off_the_path_by_its_quoted_path() {
        let native = path(r"C:\Users\sam\.local\bin\claude.exe");
        let npm = path("C:\\Users\\o'neil \u{2018}x\u{2019}\\AppData\\Roaming\\npm\\claude.cmd");

        let alone = SignInShell::PowerShell.command(None, &native);
        let in_folder =
            SignInShell::PowerShell.command(Some(Path::new(r"C:\Users\sam\.claude-work")), &native);
        let tricky = SignInShell::PowerShell.command(None, &npm);

        assert_eq!(alone, r"& 'C:\Users\sam\.local\bin\claude.exe' auth login");
        assert_eq!(
            in_folder,
            r"$env:CLAUDE_CONFIG_DIR = 'C:\Users\sam\.claude-work'; & 'C:\Users\sam\.local\bin\claude.exe' auth login; Remove-Item Env:CLAUDE_CONFIG_DIR"
        );
        assert_eq!(
            tricky,
            "& 'C:\\Users\\o''neil \u{2018}\u{2018}x\u{2019}\u{2019}\\AppData\\Roaming\\npm\\claude.cmd' auth login"
        );
    }

    #[test]
    fn without_a_folder_the_command_is_the_login_alone() {
        assert_eq!(
            SignInShell::Posix.command(None, &PLAIN),
            "claude auth login"
        );
        assert_eq!(
            SignInShell::PowerShell.command(None, &PLAIN),
            "claude auth login"
        );
    }

    #[test]
    fn a_missing_credentials_file_means_no_login_unless_the_keychain_may_hold_it() {
        assert_eq!(LoginStore::File.sign_in(true), SignIn::SignedIn);
        assert_eq!(LoginStore::File.sign_in(false), SignIn::NotSignedIn);
        assert_eq!(LoginStore::KeychainOrFile.sign_in(true), SignIn::SignedIn);
        assert_eq!(LoginStore::KeychainOrFile.sign_in(false), SignIn::Unknown);
    }

    #[test]
    fn an_account_is_signed_in_once_its_folder_holds_the_credentials_file() {
        let temp = tempfile::tempdir().unwrap();
        let account = Account {
            id: AccountId::new(1),
            name: "Work".to_owned(),
            folder: temp.path().join(".claude-work"),
            credentials: CredentialSource::SignIn,
        };

        let before = account_state(account.clone(), SignInShell::Posix, &PLAIN);
        std::fs::create_dir(&account.folder).unwrap();
        std::fs::write(account.folder.join(CREDENTIALS_FILE), "{}").unwrap();
        let after = account_state(account.clone(), SignInShell::Posix, &PLAIN);

        assert_eq!(before.sign_in, LoginStore::CURRENT.sign_in(false));
        assert!(login_written(&account).is_some());
        assert_eq!(after.sign_in, SignIn::SignedIn);
        assert_eq!(
            after.sign_in_command,
            SignInShell::Posix.command(Some(&account.folder), &PLAIN)
        );
        assert_eq!(after.account, account);
    }
}

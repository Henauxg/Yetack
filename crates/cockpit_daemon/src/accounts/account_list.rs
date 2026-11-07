use std::path::{Component, Path, PathBuf};

use cockpit_protocol::{
    Account, AccountAddParams, AccountChangeParams, AccountId, CredentialSource, ErrorBody,
    ErrorCode, FOLDER_SEPARATOR, MAX_ACCOUNT_NAME_CHARS, account_folder_name, enum_or_stored_bool,
};
use serde::{Deserialize, Serialize};

/// The default account's name until the user changes it.
const DEFAULT_NAME: &str = "Default";

/// The accounts, as `settings.json` keeps them. The default account's folder is the daemon's own,
/// so only its name and its credentials are kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AccountList {
    default_name: String,
    /// Files of older daemons hold `default_uses_environment_api_key`, a `bool`, in its place.
    #[serde(
        alias = "default_uses_environment_api_key",
        deserialize_with = "enum_or_stored_bool"
    )]
    default_credentials: CredentialSource,
    added: Vec<Account>,
    /// Ids are never reused, so an agent never moves to a later account.
    next_account: AccountId,
}

impl Default for AccountList {
    fn default() -> Self {
        Self {
            default_name: DEFAULT_NAME.to_owned(),
            default_credentials: CredentialSource::SignIn,
            added: Vec::new(),
            next_account: AccountId::new(1),
        }
    }
}

impl AccountList {
    /// Every account, the default one first, whose folder is `default_folder`.
    pub(crate) fn accounts(&self, default_folder: &Path) -> Vec<Account> {
        let mut accounts = Vec::with_capacity(self.added.len() + 1);
        accounts.push(self.default_account(default_folder));
        accounts.extend(self.added.iter().cloned());
        accounts
    }

    pub(crate) fn get(&self, id: AccountId, default_folder: &Path) -> Option<Account> {
        if id.is_default() {
            return Some(self.default_account(default_folder));
        }
        self.added.iter().find(|account| account.id == id).cloned()
    }

    /// Adds an account. Without a folder, it takes one in `home`, when absolute, named after the
    /// account.
    pub(crate) fn add(
        &mut self,
        params: AccountAddParams,
        home: Option<&Path>,
        default_folder: &Path,
    ) -> Result<Account, ErrorBody> {
        let name = self.free_name(&params.name, None)?;
        let taken = self.folders(default_folder);
        let folder = match params.folder {
            Some(folder) => free_folder(&folder, &taken, FolderCase::CURRENT)?,
            None => {
                let home = home.filter(|home| home.is_absolute()).ok_or_else(|| {
                    invalid("the daemon has no home folder: give the account's folder".to_owned())
                })?;
                proposed_folder(home, &name, &taken, FolderCase::CURRENT)
            }
        };
        let id = self.next_account;
        let next = id
            .next()
            .ok_or_else(|| invalid("no account id is left".to_owned()))?;
        let account = Account {
            id,
            name,
            folder,
            credentials: params.credentials,
        };
        self.added.push(account.clone());
        self.next_account = next;
        Ok(account)
    }

    pub(crate) fn change(
        &mut self,
        params: AccountChangeParams,
        default_folder: &Path,
    ) -> Result<Account, ErrorBody> {
        if self.get(params.account, default_folder).is_none() {
            return Err(not_found(params.account));
        }
        let name = params
            .name
            .map(|name| self.free_name(&name, Some(params.account)))
            .transpose()?;
        if params.account.is_default() {
            if let Some(name) = name {
                self.default_name = name;
            }
            if let Some(credentials) = params.credentials {
                self.default_credentials = credentials;
            }
            return Ok(self.default_account(default_folder));
        }
        let account = self
            .added
            .iter_mut()
            .find(|account| account.id == params.account)
            .ok_or_else(|| not_found(params.account))?;
        if let Some(name) = name {
            account.name = name;
        }
        if let Some(credentials) = params.credentials {
            account.credentials = credentials;
        }
        Ok(account.clone())
    }

    /// Removes an account other than the default one. Its folder stays as it is.
    pub(crate) fn remove(&mut self, id: AccountId) -> Result<(), ErrorBody> {
        if id.is_default() {
            return Err(invalid("the default account cannot be removed".to_owned()));
        }
        let index = self
            .added
            .iter()
            .position(|account| account.id == id)
            .ok_or_else(|| not_found(id))?;
        self.added.remove(index);
        Ok(())
    }

    /// The list `settings.json`'s pinned sample holds.
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        Self {
            default_name: "Personal".to_owned(),
            default_credentials: CredentialSource::EnvironmentApiKey,
            added: vec![Account {
                id: AccountId::new(2),
                name: "Work".to_owned(),
                folder: PathBuf::from("/home/sam/.claude-work"),
                credentials: CredentialSource::SignIn,
            }],
            next_account: AccountId::new(4),
        }
    }

    fn default_account(&self, default_folder: &Path) -> Account {
        Account {
            id: AccountId::DEFAULT,
            name: self.default_name.clone(),
            folder: default_folder.to_owned(),
            credentials: self.default_credentials,
        }
    }

    fn folders<'list>(&'list self, default_folder: &'list Path) -> Vec<&'list Path> {
        let added = self.added.iter().map(|account| account.folder.as_path());
        std::iter::once(default_folder).chain(added).collect()
    }

    /// A name from a client, its whitespace collapsed and trimmed, that no account but `renamed`
    /// has, whatever its case.
    fn free_name(&self, text: &str, renamed: Option<AccountId>) -> Result<String, ErrorBody> {
        let name = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            return Err(invalid("the account's name is empty".to_owned()));
        }
        let chars = name.chars().count();
        if chars > MAX_ACCOUNT_NAME_CHARS {
            return Err(invalid(format!(
                "the account's name has {chars} characters, more than {MAX_ACCOUNT_NAME_CHARS}"
            )));
        }
        if name.chars().any(char::is_control) {
            return Err(invalid(
                "the account's name holds a control character".to_owned(),
            ));
        }
        let lower = name.to_lowercase();
        let default = (AccountId::DEFAULT, self.default_name.as_str());
        let added = self
            .added
            .iter()
            .map(|account| (account.id, account.name.as_str()));
        let taken = std::iter::once(default)
            .chain(added)
            .any(|(id, other)| Some(id) != renamed && other.to_lowercase() == lower);
        if taken {
            return Err(invalid(format!("an account is already named {name}")));
        }
        Ok(name)
    }
}

/// Whether the file system tells folders apart by the case of their names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderCase {
    #[cfg_attr(
        any(windows, all(target_os = "macos", not(test))),
        expect(dead_code, reason = "Windows and macOS ignore case")
    )]
    Matters,
    #[cfg_attr(
        not(any(windows, target_os = "macos", test)),
        expect(dead_code, reason = "only Windows and macOS ignore case")
    )]
    Ignored,
}

impl FolderCase {
    /// The default file systems of Windows and macOS ignore case.
    #[cfg(any(windows, target_os = "macos"))]
    const CURRENT: Self = Self::Ignored;
    #[cfg(not(any(windows, target_os = "macos")))]
    const CURRENT: Self = Self::Matters;

    fn same(self, left: &Path, right: &Path) -> bool {
        match self {
            Self::Matters => left == right,
            Self::Ignored => {
                let names = |path: &Path| -> Vec<String> {
                    path.components()
                        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
                        .collect()
                };
                names(left) == names(right)
            }
        }
    }

    fn taken(self, folder: &Path, taken: &[&Path]) -> bool {
        taken.iter().any(|other| self.same(other, folder))
    }
}

/// A folder from a client: absolute, one line of text, and no other account's.
fn free_folder(folder: &Path, taken: &[&Path], case: FolderCase) -> Result<PathBuf, ErrorBody> {
    if !folder.is_absolute() {
        return Err(invalid(format!(
            "the folder {} is not absolute",
            folder.display()
        )));
    }
    if folder
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(invalid(format!(
            "the folder {} holds `..`",
            folder.display()
        )));
    }
    if folder.to_string_lossy().chars().any(char::is_control) {
        return Err(invalid(
            "the folder's path holds a control character".to_owned(),
        ));
    }
    if case.taken(folder, taken) {
        return Err(invalid(format!(
            "another account uses the folder {}",
            folder.display()
        )));
    }
    Ok(folder.to_owned())
}

/// `.claude-<name>` in `home`, the name made safe for a folder, and a number added when another
/// account has that folder.
fn proposed_folder(home: &Path, name: &str, taken: &[&Path], case: FolderCase) -> PathBuf {
    let folder_name = account_folder_name(name);
    let mut folder = home.join(&folder_name);
    let mut number: u32 = 2;
    while case.taken(&folder, taken) {
        folder = home.join(format!("{folder_name}{FOLDER_SEPARATOR}{number}"));
        number = number.saturating_add(1);
    }
    folder
}

fn invalid(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidRequest, message)
}

fn not_found(id: AccountId) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, format!("no account {}", id.get()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_folder() -> PathBuf {
        PathBuf::from("/home/sam/.claude")
    }

    fn add(list: &mut AccountList, name: &str, folder: Option<&str>) -> Result<Account, ErrorBody> {
        let params = AccountAddParams {
            name: name.to_owned(),
            folder: folder.map(PathBuf::from),
            credentials: CredentialSource::SignIn,
        };
        list.add(params, Some(Path::new("/home/sam")), &default_folder())
    }

    #[test]
    fn a_new_account_gets_the_next_id_and_a_folder_named_after_it() {
        let mut list = AccountList::default();

        let work = add(&mut list, "  Client   Work ", None).unwrap();
        let again = add(&mut list, "client/work", None).unwrap();

        assert_eq!(work.id, AccountId::new(1));
        assert_eq!(work.name, "Client Work");
        assert_eq!(work.folder, Path::new("/home/sam/.claude-client-work"));
        assert_eq!(again.id, AccountId::new(2));
        assert_eq!(again.folder, Path::new("/home/sam/.claude-client-work-2"));
        let ids: Vec<AccountId> = list
            .accounts(&default_folder())
            .iter()
            .map(|account| account.id)
            .collect();
        assert_eq!(ids, [AccountId::DEFAULT, work.id, again.id]);
    }

    #[test]
    fn a_name_must_be_new_whatever_its_case_and_not_too_long() {
        let mut list = AccountList::default();
        add(&mut list, "Work", None).unwrap();

        let same = add(&mut list, "WORK", None).unwrap_err();
        let default = add(&mut list, "default", None).unwrap_err();
        let empty = add(&mut list, " \t", None).unwrap_err();
        let long = add(&mut list, &"w".repeat(MAX_ACCOUNT_NAME_CHARS + 1), None).unwrap_err();

        for error in [same, default, empty, long] {
            assert_eq!(error.code, ErrorCode::InvalidRequest, "{error:?}");
        }
    }

    /// The folders are Unix paths, which Windows does not take as absolute.
    #[cfg(unix)]
    #[test]
    fn a_given_folder_must_be_absolute_plain_and_no_other_accounts() {
        let mut list = AccountList::default();
        add(&mut list, "Work", Some("/srv/claude/work")).unwrap();

        let relative = add(&mut list, "A", Some("claude")).unwrap_err();
        let home = add(&mut list, "B", Some("~/.claude-b")).unwrap_err();
        let parent = add(&mut list, "C", Some("/home/sam/x/../.claude")).unwrap_err();
        let default = add(&mut list, "D", Some("/home/sam/.claude/")).unwrap_err();
        let other = add(&mut list, "E", Some("/srv/claude/work")).unwrap_err();
        let line_break = add(&mut list, "F", Some("/srv/claude/f\nrm")).unwrap_err();

        for error in [relative, home, parent, default, other, line_break] {
            assert_eq!(error.code, ErrorCode::InvalidRequest, "{error:?}");
        }
        assert_eq!(list.accounts(&default_folder()).len(), 2);
    }

    /// Where the file system ignores case, a folder that differs from another account's only in
    /// case is that account's, and a proposal moves past it. The folders are Unix paths.
    #[cfg(unix)]
    #[test]
    fn folders_that_differ_only_in_case_clash_where_case_is_ignored() {
        let work = Path::new("/home/sam/.Claude-Work");
        let taken = [default_folder(), work.to_owned()];
        let taken: Vec<&Path> = taken.iter().map(PathBuf::as_path).collect();
        let given = Path::new("/home/sam/.CLAUDE");
        let home = Path::new("/home/sam");

        let refused = free_folder(given, &taken, FolderCase::Ignored).unwrap_err();
        let accepted = free_folder(given, &taken, FolderCase::Matters).unwrap();
        let proposed = proposed_folder(home, "Work", &taken, FolderCase::Ignored);
        let kept = proposed_folder(home, "Work", &taken, FolderCase::Matters);

        assert_eq!(refused.code, ErrorCode::InvalidRequest);
        assert_eq!(accepted, given);
        assert_eq!(proposed, Path::new("/home/sam/.claude-work-2"));
        assert_eq!(kept, Path::new("/home/sam/.claude-work"));
    }

    /// Windows compares folder names whatever their case, and takes `/` for `\`.
    #[cfg(windows)]
    #[test]
    fn a_windows_folder_is_no_other_accounts_whatever_its_case() {
        let mut list = AccountList::default();
        let default_folder = Path::new(r"C:\Users\Sam\.claude");
        let home = Path::new(r"C:\Users\Sam");
        let mut add = |name: &str, folder: Option<&str>| {
            let params = AccountAddParams {
                name: name.to_owned(),
                folder: folder.map(PathBuf::from),
                credentials: CredentialSource::SignIn,
            };
            list.add(params, Some(home), default_folder)
        };

        let default = add("A", Some(r"c:\users\sam\.CLAUDE")).unwrap_err();
        let slashes = add("B", Some("C:/Users/Sam/.claude")).unwrap_err();
        add("Work", Some(r"C:\Users\Sam\.Claude-Work")).unwrap();
        let proposed = add("work!", None).unwrap();

        assert_eq!(default.code, ErrorCode::InvalidRequest);
        assert_eq!(slashes.code, ErrorCode::InvalidRequest);
        assert_eq!(proposed.folder, Path::new(r"C:\Users\Sam\.claude-work-2"));
    }

    /// A folder `claude` would take as relative to each agent's folder is no account's folder.
    #[test]
    fn a_relative_home_folder_proposes_no_folder() {
        let mut list = AccountList::default();
        let params = AccountAddParams {
            name: "Work".to_owned(),
            folder: None,
            credentials: CredentialSource::SignIn,
        };

        let error = list
            .add(params, Some(Path::new("home")), &default_folder())
            .unwrap_err();

        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert_eq!(list.accounts(&default_folder()).len(), 1);
    }

    #[test]
    fn a_change_renames_or_sets_the_api_key_choice_of_any_account() {
        let mut list = AccountList::default();
        let work = add(&mut list, "Work", None).unwrap();
        let change = |account, name: Option<&str>, credentials| AccountChangeParams {
            account,
            name: name.map(str::to_owned),
            credentials,
        };

        let renamed = list
            .change(
                change(AccountId::DEFAULT, Some("Personal"), None),
                &default_folder(),
            )
            .unwrap();
        let keyed = list
            .change(
                change(work.id, None, Some(CredentialSource::EnvironmentApiKey)),
                &default_folder(),
            )
            .unwrap();
        let same_name = list
            .change(change(work.id, Some("work"), None), &default_folder())
            .unwrap();
        let taken = list
            .change(change(work.id, Some("personal"), None), &default_folder())
            .unwrap_err();
        let missing = list
            .change(change(AccountId::new(9), None, None), &default_folder())
            .unwrap_err();
        let missing_renamed = list
            .change(
                change(AccountId::new(9), Some("Personal"), None),
                &default_folder(),
            )
            .unwrap_err();

        assert_eq!(renamed.name, "Personal");
        assert_eq!(renamed.folder, default_folder());
        assert_eq!(keyed.credentials, CredentialSource::EnvironmentApiKey);
        assert_eq!(keyed.folder, work.folder);
        assert_eq!(same_name.name, "work");
        assert_eq!(taken.code, ErrorCode::InvalidRequest);
        assert_eq!(missing.code, ErrorCode::NotFound);
        assert_eq!(missing_renamed.code, ErrorCode::NotFound);
    }

    #[test]
    fn the_default_account_stays_and_a_removed_id_is_not_reused() {
        let mut list = AccountList::default();
        let work = add(&mut list, "Work", None).unwrap();

        let default = list.remove(AccountId::DEFAULT).unwrap_err();
        list.remove(work.id).unwrap();
        let again = list.remove(work.id).unwrap_err();
        let next = add(&mut list, "Work", None).unwrap();

        assert_eq!(default.code, ErrorCode::InvalidRequest);
        assert_eq!(again.code, ErrorCode::NotFound);
        assert_eq!(next.id, AccountId::new(2));
        assert_eq!(list.get(work.id, &default_folder()), None);
    }
}

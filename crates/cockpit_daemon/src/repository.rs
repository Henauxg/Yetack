//! The git repository an agent's folder belongs to, named by its remote, which the app groups
//! agents by. It reads git's files itself, rather than running `git`, since a summary is built
//! where waiting on a process would hold up the daemon.

use std::fs;
use std::path::{Path, PathBuf};

use cockpit_protocol::Repository;

const GIT_ENTRY: &str = ".git";
/// What a worktree's `.git` file starts with, before the path of its git folder.
const GITDIR_PREFIX: &str = "gitdir:";
/// The file in a worktree's git folder that names the repository's shared git folder.
const COMMONDIR_FILE: &str = "commondir";
const CONFIG_FILE: &str = "config";
const ORIGIN: &str = "origin";

/// The repository of the git checkout that holds `folder`, by its `origin` remote, else by its
/// first remote. `None` outside git, or for a checkout without a remote.
pub(crate) fn of_folder(folder: &Path) -> Option<Repository> {
    linked_remote(folder).map(|remote| Repository::from_remote(&remote.url))
}

/// The remote [`of_folder`] names the repository by.
pub(crate) fn linked_remote(folder: &Path) -> Option<ConfigRemote> {
    let checkout = folder
        .ancestors()
        .find(|ancestor| ancestor.join(GIT_ENTRY).symlink_metadata().is_ok())?;
    let git_dir = git_dir(&checkout.join(GIT_ENTRY))?;
    let config = read_regular(&common_dir(&git_dir).join(CONFIG_FILE))?;
    let remotes = remotes(&config);
    let linked = remotes
        .iter()
        .position(|remote| remote.name == ORIGIN)
        .unwrap_or(0);
    remotes.into_iter().nth(linked)
}

/// Whether the checkout whose top folder is `checkout` is a linked worktree: its `.git` file
/// names a git folder with a `commondir`, which a submodule's git folder lacks.
pub(crate) fn is_linked_worktree(checkout: &Path) -> bool {
    let entry = checkout.join(GIT_ENTRY);
    !entry.is_dir() && git_dir(&entry).is_some_and(|git_dir| git_dir.join(COMMONDIR_FILE).is_file())
}

/// A `[remote "<name>"]` section of a git config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigRemote {
    pub(crate) name: String,
    pub(crate) url: String,
}

/// The git folder: `.git` itself, or the folder a worktree's `.git` file names.
fn git_dir(entry: &Path) -> Option<PathBuf> {
    if entry.is_dir() {
        return Some(entry.to_owned());
    }
    let text = read_regular(entry)?;
    let named = text.trim().strip_prefix(GITDIR_PREFIX)?.trim();
    let named = Path::new(named);
    Some(if named.is_absolute() {
        named.to_owned()
    } else {
        entry.parent()?.join(named)
    })
}

/// The git folder that holds the config: a worktree's names it in `commondir`.
fn common_dir(git_dir: &Path) -> PathBuf {
    match read_regular(&git_dir.join(COMMONDIR_FILE)) {
        Some(text) => {
            let named = Path::new(text.trim());
            if named.is_absolute() {
                named.to_owned()
            } else {
                git_dir.join(named)
            }
        }
        None => git_dir.to_owned(),
    }
}

/// The text of `path` when it is a regular file. Reading anything else, such as a FIFO an
/// agent made, could block the daemon or never end.
fn read_regular(path: &Path) -> Option<String> {
    if !fs::metadata(path).ok()?.is_file() {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Each remote with a `url`, in the file's order.
fn remotes(config: &str) -> Vec<ConfigRemote> {
    let mut remotes: Vec<ConfigRemote> = Vec::new();
    let mut section: Option<String> = None;
    for line in config.lines() {
        let line = line.trim();
        if let Some(header) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = header
                .strip_prefix("remote")
                .map(str::trim)
                .and_then(|name| name.strip_prefix('"'))
                .and_then(|name| name.strip_suffix('"'))
                .map(str::to_owned);
            continue;
        }
        let Some(name) = &section else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") && !remotes.iter().any(|kept| kept.name == *name)
        {
            remotes.push(ConfigRemote {
                name: name.clone(),
                url: value.trim().to_owned(),
            });
        }
    }
    remotes
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://github.com/upstream/billing.git\n[remote \"origin\"]\n\turl = git@github.com:acme/billing.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";

    #[test]
    fn a_folder_inside_a_checkout_names_its_origin() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("billing");
        fs::create_dir_all(checkout.join(".git")).unwrap();
        fs::write(checkout.join(".git/config"), CONFIG).unwrap();
        fs::create_dir_all(checkout.join("src/invoices")).unwrap();

        let repository = of_folder(&checkout.join("src/invoices"));

        assert_eq!(
            repository.as_ref().map(Repository::as_str),
            Some("github.com/acme/billing")
        );
    }

    #[test]
    fn a_worktree_reads_the_config_of_the_repository_it_shares() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("billing");
        let worktree_git = main.join(".git/worktrees/billing-fix");
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(main.join(".git/config"), CONFIG).unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();
        let worktree = temp.path().join("billing-fix");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();

        assert_eq!(
            of_folder(&worktree).as_ref().map(Repository::as_str),
            Some("github.com/acme/billing")
        );
        assert!(is_linked_worktree(&worktree));
        assert!(!is_linked_worktree(&main));
    }

    #[test]
    fn a_submodule_is_no_linked_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let module_git = temp.path().join("billing/.git/modules/ledger");
        fs::create_dir_all(&module_git).unwrap();
        let module = temp.path().join("billing/ledger");
        fs::create_dir_all(&module).unwrap();
        fs::write(module.join(".git"), "gitdir: ../.git/modules/ledger\n").unwrap();

        assert!(!is_linked_worktree(&module));
    }

    #[test]
    fn without_origin_the_first_remote_names_it_and_without_any_nothing_does() {
        let only_upstream = "[remote \"upstream\"]\n\turl = https://github.com/upstream/billing\n";

        assert_eq!(
            remotes(only_upstream),
            [ConfigRemote {
                name: "upstream".to_owned(),
                url: "https://github.com/upstream/billing".to_owned(),
            }]
        );
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(of_folder(temp.path()), None);
        assert_eq!(of_folder(&temp.path().join("missing")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_in_place_of_a_git_file_names_no_repository_at_once() {
        use std::sync::mpsc;
        use std::time::Duration;

        use rustix::fs::{CWD, Mode, mkfifoat};

        let temp = tempfile::tempdir().unwrap();
        let entry_fifo = temp.path().join("notes");
        fs::create_dir_all(&entry_fifo).unwrap();
        mkfifoat(CWD, entry_fifo.join(".git"), Mode::RUSR).unwrap();
        let config_fifo = temp.path().join("billing");
        fs::create_dir_all(config_fifo.join(".git")).unwrap();
        mkfifoat(CWD, config_fifo.join(".git/config"), Mode::RUSR).unwrap();
        let (sender, receiver) = mpsc::channel();

        // Opening a FIFO waits for a writer: the reads run in a thread the test can leave behind.
        std::thread::spawn(move || {
            let _ = sender.send((of_folder(&entry_fifo), of_folder(&config_fifo)));
        });

        let found = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("an answer without a writer on the FIFOs");
        assert_eq!(found, (None, None));
    }
}

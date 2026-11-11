//! The git repository an agent's folder belongs to, named by its remote, which the app groups
//! agents by. It reads git's files itself, rather than running `git`, since a summary is built
//! where waiting on a process would hold up the daemon.

use std::fs;
use std::path::{Path, PathBuf};

use cockpit_protocol::normalized_repository;

const GIT_ENTRY: &str = ".git";
/// What a worktree's `.git` file starts with, before the path of its git folder.
const GITDIR_PREFIX: &str = "gitdir:";
/// The file in a worktree's git folder that names the repository's shared git folder.
const COMMONDIR_FILE: &str = "commondir";
const CONFIG_FILE: &str = "config";
const ORIGIN: &str = "origin";

/// The repository of the git checkout that holds `folder`, by its `origin` remote, else by its
/// first remote. `None` outside git, or for a checkout without a remote.
pub(crate) fn of_folder(folder: &Path) -> Option<String> {
    let checkout = folder
        .ancestors()
        .find(|ancestor| ancestor.join(GIT_ENTRY).symlink_metadata().is_ok())?;
    let git_dir = git_dir(&checkout.join(GIT_ENTRY))?;
    let config = fs::read_to_string(common_dir(&git_dir).join(CONFIG_FILE)).ok()?;
    let remotes = remote_urls(&config);
    let url = remotes
        .iter()
        .find(|(name, _)| name == ORIGIN)
        .or_else(|| remotes.first())
        .map(|(_, url)| url)?;
    Some(normalized_repository(url))
}

/// The git folder: `.git` itself, or the folder a worktree's `.git` file names.
fn git_dir(entry: &Path) -> Option<PathBuf> {
    if entry.is_dir() {
        return Some(entry.to_owned());
    }
    let text = fs::read_to_string(entry).ok()?;
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
    match fs::read_to_string(git_dir.join(COMMONDIR_FILE)) {
        Ok(text) => {
            let named = Path::new(text.trim());
            if named.is_absolute() {
                named.to_owned()
            } else {
                git_dir.join(named)
            }
        }
        Err(_) => git_dir.to_owned(),
    }
}

/// Each `[remote "<name>"]` section's name and `url`, in the file's order.
fn remote_urls(config: &str) -> Vec<(String, String)> {
    let mut remotes = Vec::new();
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
        if key.trim().eq_ignore_ascii_case("url") && !remotes.iter().any(|(kept, _)| kept == name) {
            remotes.push((name.clone(), value.trim().to_owned()));
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

        assert_eq!(repository.as_deref(), Some("github.com/acme/billing"));
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
            of_folder(&worktree).as_deref(),
            Some("github.com/acme/billing")
        );
    }

    #[test]
    fn without_origin_the_first_remote_names_it_and_without_any_nothing_does() {
        let only_upstream = "[remote \"upstream\"]\n\turl = https://github.com/upstream/billing\n";

        assert_eq!(
            remote_urls(only_upstream),
            [(
                "upstream".to_owned(),
                "https://github.com/upstream/billing".to_owned()
            )]
        );
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(of_folder(temp.path()), None);
        assert_eq!(of_folder(&temp.path().join("missing")), None);
    }
}

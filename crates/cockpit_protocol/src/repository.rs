//! A git repository named by its remote, the same way whatever the URL's form, so that two
//! clones or worktrees of one repository on two machines name it alike.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A git repository by its remote, as [`Repository::from_remote`] writes it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Repository(String);

impl Repository {
    /// The repository of a remote's URL, in any of its forms. The URL of a repository written
    /// this way gives it back.
    pub fn from_remote(url: &str) -> Self {
        Self(normalized(url))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its last name: `billing` for `github.com/acme/billing` or `C:\repos\billing`.
    pub fn name(&self) -> &str {
        self.0
            .rsplit(['/', '\\', ':'])
            .find(|name| !name.is_empty())
            .unwrap_or(&self.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// `url` without its scheme, user, port, trailing `.git` and slashes, and with its host in lower
/// case:
///
/// - `git@github.com:acme/billing.git` gives `github.com/acme/billing`;
/// - `https://user@GitHub.com:443/acme/billing/` gives `github.com/acme/billing`;
/// - `ssh://git@github.com/acme/billing` gives `github.com/acme/billing`;
/// - a local path, such as `/srv/git/billing.git`, gives `/srv/git/billing`, and
///   `C:\repos\billing.git` gives `C:\repos\billing`.
fn normalized(url: &str) -> String {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some(("file", path)) => return trimmed(path).to_owned(),
        Some((_, rest)) => match rest.split_once('/') {
            Some((authority, path)) => (authority, path),
            None => (rest, ""),
        },
        // `user@host:path`, the form scp takes. A `:` after a `/` is part of a local path, and
        // one after a drive letter starts a Windows path.
        None => match url.split_once(':') {
            Some((authority, path))
                if !authority.contains('/')
                    && !authority.is_empty()
                    && !is_drive(authority, path) =>
            {
                (authority, path)
            }
            _ => return trimmed(url).to_owned(),
        },
    };
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host).to_lowercase();
    let path = trimmed(path).trim_start_matches('/');
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

/// `path` without its trailing separators and `.git`, as many as it ends with, so that the text
/// of a repository gives that repository again.
fn trimmed(mut path: &str) -> &str {
    loop {
        let rest = path.trim_end_matches(['/', '\\']);
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        if rest == path {
            return path;
        }
        path = rest;
    }
}

/// Whether `C` and `\repos` are a drive letter and the path after it.
fn is_drive(authority: &str, path: &str) -> bool {
    let mut letters = authority.chars();
    let one_letter = letters
        .next()
        .is_some_and(|letter| letter.is_ascii_alphabetic())
        && letters.next().is_none();
    one_letter && path.starts_with(['/', '\\'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_form_of_one_remote_names_it_alike() {
        let forms = [
            "git@github.com:acme/billing.git",
            "https://github.com/acme/billing",
            "https://user@GitHub.com:443/acme/billing/",
            "ssh://git@github.com/acme/billing.git",
            "github.com:acme/billing",
        ];

        for form in forms {
            assert_eq!(normalized(form), "github.com/acme/billing", "{form}");
        }
    }

    #[test]
    fn a_local_remote_keeps_its_path() {
        assert_eq!(normalized("/srv/git/billing.git"), "/srv/git/billing");
        assert_eq!(
            normalized("file:///srv/git/billing.git"),
            "/srv/git/billing"
        );
        assert_eq!(normalized("../billing"), "../billing");
        let windows = Repository::from_remote(r"C:\repos\billing.git");
        assert_eq!(windows.name(), "billing");
        assert_eq!(Repository::from_remote(windows.as_str()), windows);
        assert_eq!(normalized(r"C:\repos\billing.git\"), r"C:\repos\billing");
        assert_eq!(normalized("C:/repos/billing"), "C:/repos/billing");
    }

    #[test]
    fn a_written_repository_reads_back_the_same() {
        let remotes = [
            "/srv/git/billing/.git",
            r"C:\repos\billing\.git",
            "/srv/git/billing.git.git/",
            "git@github.com:acme/billing.git/",
        ];

        for remote in remotes {
            let written = Repository::from_remote(remote);
            assert_eq!(
                Repository::from_remote(written.as_str()),
                written,
                "{remote}"
            );
        }
        assert_eq!(normalized("/srv/git/billing/.git"), "/srv/git/billing");
    }
}

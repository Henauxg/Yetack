//! A git repository named by its remote, the same way whatever the URL's form, so that two
//! clones or worktrees of one repository on two machines name it alike.

/// `url` without its scheme, user, port, trailing `.git` and slashes, and with its host in lower
/// case:
///
/// - `git@github.com:acme/billing.git` gives `github.com/acme/billing`;
/// - `https://user@GitHub.com:443/acme/billing/` gives `github.com/acme/billing`;
/// - `ssh://git@github.com/acme/billing` gives `github.com/acme/billing`;
/// - a local path, such as `/srv/git/billing.git`, gives `/srv/git/billing`.
pub fn normalized_repository(url: &str) -> String {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some(("file", path)) => return trimmed(path).to_owned(),
        Some((_, rest)) => match rest.split_once('/') {
            Some((authority, path)) => (authority, path),
            None => (rest, ""),
        },
        // `user@host:path`, the form scp takes. A `:` after a `/` is part of a local path.
        None => match url.split_once(':') {
            Some((authority, path)) if !authority.contains('/') && !authority.is_empty() => {
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

fn trimmed(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git").unwrap_or(path)
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
            assert_eq!(
                normalized_repository(form),
                "github.com/acme/billing",
                "{form}"
            );
        }
    }

    #[test]
    fn a_local_remote_keeps_its_path() {
        assert_eq!(
            normalized_repository("/srv/git/billing.git"),
            "/srv/git/billing"
        );
        assert_eq!(
            normalized_repository("file:///srv/git/billing.git"),
            "/srv/git/billing"
        );
        assert_eq!(normalized_repository("../billing"), "../billing");
    }
}

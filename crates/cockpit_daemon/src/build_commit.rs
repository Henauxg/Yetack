//! The release and the commit this program was built from, which Cargo and the build script set.

use cockpit_protocol::BuildCommit;

/// The release this program was built from, its Cargo version. The daemon sends it in `Welcome`,
/// and the app compares it with its own, which is this same constant.
pub const RELEASE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit this program was built from, when its build knew it: `yetack` or the app,
/// whichever runs this code. A Nix build of a tree with uncommitted changes sets it empty.
#[must_use]
pub fn build_commit() -> Option<BuildCommit> {
    known_commit(option_env!("YETACK_COMMIT"))
}

/// What `yetack --version` prints after the program's name: the release, then the commit
/// when the build knew it. The app checks a daemon it sent to a server by this text.
#[must_use]
pub fn version_text(release: &str, commit: Option<&BuildCommit>) -> String {
    match commit {
        Some(commit) => format!("{release} ({})", commit.as_str()),
        None => release.to_owned(),
    }
}

fn known_commit(built_with: Option<&str>) -> Option<BuildCommit> {
    built_with
        .filter(|commit| !commit.is_empty())
        .map(|commit| BuildCommit::new(commit.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_text_names_the_commit_the_build_knew() {
        let commit = BuildCommit::new("3f9c2e1".to_owned());

        assert_eq!(version_text("0.2.0", Some(&commit)), "0.2.0 (3f9c2e1)");
        assert_eq!(version_text("0.2.0", None), "0.2.0");
    }

    #[test]
    fn an_empty_commit_is_no_commit() {
        assert_eq!(known_commit(None), None);
        assert_eq!(known_commit(Some("")), None);
        assert_eq!(
            known_commit(Some("3f9c2e1")),
            Some(BuildCommit::new("3f9c2e1".to_owned()))
        );
    }
}

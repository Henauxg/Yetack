//! Where a file is in its git checkout, and which checkouts `agent.git_status` reports on.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::git::GitError;
use crate::read_file::{self, Access, GIT_ENTRY, ReadFileError};

/// What git writes between the names of a path.
const SEPARATOR: &str = "/";
/// The most checkouts `agent.git_status` reports on.
const MAX_CHECKOUTS: usize = 20;

/// A file in a git checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CheckoutFile {
    /// The checkout's top folder, with symlinks resolved.
    pub(super) root: PathBuf,
    /// The file's path from the top folder, as git writes it: names joined by `/`.
    pub(super) path: String,
}

/// [`locate`], in a task that may wait on the file system.
pub(super) async fn locate_in_task(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
) -> Result<CheckoutFile, GitError> {
    tokio::task::spawn_blocking(move || locate(&folder, &touched, home.as_deref(), &path)).await?
}

/// The checkout that holds `path`, absolute or relative to `folder`, the agent's folder, once its
/// symlinks are resolved, so that git reads the file the file view shows. A file that no longer
/// exists is found by its folders that do. `touched` and `home` are as for `agent.read_file`.
fn locate(
    folder: &Path,
    touched: &[PathBuf],
    home: Option<&Path>,
    path: &Path,
) -> Result<CheckoutFile, GitError> {
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(GitError::GoesUp);
    }
    let access = Access::new(folder, touched, home);
    let asked = folder.join(path);
    access.check_before_open(&asked)?;
    let resolved = resolve(&asked)?;
    // A link may lead to a file named `AUX`, which the path opens as the device.
    access.check_before_open(&resolved)?;
    let home: Option<PathBuf> =
        home.map(|home| read_file::canonical(home).unwrap_or_else(|_| home.to_owned()));
    let root = resolved
        .parent()
        .and_then(|parent| read_file::checkout_holding(parent, home.as_deref()));
    let located = root.and_then(|root| {
        let path = git_name(resolved.strip_prefix(&root).ok()?)?;
        Some(CheckoutFile { root, path })
    });
    located.ok_or(GitError::Outside(asked))
}

/// `path` with its symlinks resolved. When it does not exist, as for a file deleted since, its
/// nearest folder that exists is resolved, and the names past it kept.
fn resolve(path: &Path) -> Result<PathBuf, GitError> {
    let mut missing: Vec<&OsStr> = Vec::new();
    let mut existing = path;
    loop {
        match read_file::canonical(existing) {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Err(ReadFileError::Missing(path.to_owned()).into());
                };
                missing.push(name);
                existing = parent;
            }
            Err(error) => return Err(ReadFileError::NotFound(error).into()),
        }
    }
}

/// A path from a checkout's top folder as git writes it. `None` for one that is not plain names
/// in text.
fn git_name(relative: &Path) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        names.push(name.to_str()?);
    }
    (!names.is_empty()).then(|| names.join(SEPARATOR))
}

/// The top folders of the checkouts `agent.git_status` reports on: the one that holds `folder`,
/// the agent's folder, then each one that holds a file of `touched`, most recent first. Each
/// comes once, named as the path it was found by names it, so that its files match the agent's
/// own paths. A checkout that is `/` or holds `home` is left out, as for a search, and so are
/// those past [`MAX_CHECKOUTS`].
pub(super) fn status_roots(
    folder: &Path,
    touched: &[PathBuf],
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let home: Option<PathBuf> =
        home.map(|home| read_file::canonical(home).unwrap_or_else(|_| home.to_owned()));
    let starts = std::iter::once(folder).chain(touched.iter().filter_map(|file| file.parent()));
    let mut looked_up: BTreeSet<&Path> = BTreeSet::new();
    let mut resolved_roots: Vec<PathBuf> = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    for start in starts {
        if roots.len() >= MAX_CHECKOUTS {
            break;
        }
        if !looked_up.insert(start) {
            continue;
        }
        let root = start
            .ancestors()
            .find(|ancestor| ancestor.join(GIT_ENTRY).symlink_metadata().is_ok());
        let Some(root) = root else {
            continue;
        };
        let Ok(resolved) = read_file::canonical(root) else {
            continue;
        };
        if read_file::too_wide(&resolved, home.as_deref()) || resolved_roots.contains(&resolved) {
            continue;
        }
        resolved_roots.push(resolved);
        roots.push(root.to_owned());
    }
    roots
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    /// A temporary folder that stands for the home folder, with a checkout `app` in it.
    struct Layout {
        home: PathBuf,
        app: PathBuf,
        _temp: TempDir,
    }

    impl Layout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = read_file::canonical(temp.path()).unwrap();
            let app = home.join("app");
            std::fs::create_dir_all(app.join(GIT_ENTRY)).unwrap();
            std::fs::create_dir_all(app.join("src")).unwrap();
            std::fs::write(app.join("src").join("main.rs"), "fn main() {}\n").unwrap();
            Self {
                home,
                app,
                _temp: temp,
            }
        }

        fn locate(&self, path: &Path) -> Result<CheckoutFile, GitError> {
            locate(&self.app, &[], Some(&self.home), path)
        }
    }

    #[test]
    fn a_file_is_named_from_its_checkouts_top_folder_with_slashes() {
        let layout = Layout::new();
        let expected = CheckoutFile {
            root: layout.app.clone(),
            path: "src/main.rs".to_owned(),
        };

        let relative = layout.locate(&Path::new("src").join("main.rs")).unwrap();
        let absolute = layout
            .locate(&layout.app.join("src").join("main.rs"))
            .unwrap();
        let deleted = layout
            .locate(&layout.app.join("gone").join("old.rs"))
            .unwrap();

        assert_eq!(relative, expected);
        assert_eq!(absolute, expected);
        assert_eq!(deleted.path, "gone/old.rs");
    }

    #[test]
    fn a_path_outside_a_checkout_or_going_up_is_refused() {
        let layout = Layout::new();
        let loose = layout.home.join("notes.txt");
        std::fs::write(&loose, "notes\n").unwrap();
        // A checkout that holds the home folder holds everything the user owns.
        std::fs::create_dir(layout.home.join(GIT_ENTRY)).unwrap();

        let outside = layout.locate(&loose);
        let up = layout.locate(Path::new("../notes.txt"));

        assert!(matches!(outside, Err(GitError::Outside(_))), "{outside:?}");
        assert!(matches!(up, Err(GitError::GoesUp)), "{up:?}");
    }

    #[test]
    fn status_reports_on_the_agents_checkout_first_then_each_other_once() {
        let layout = Layout::new();
        let other = layout.home.join("lib");
        std::fs::create_dir_all(other.join(GIT_ENTRY)).unwrap();
        let touched = [
            other.join("src").join("one.rs"),
            layout.app.join("src").join("main.rs"),
            other.join("two.rs"),
            layout.home.join("loose").join("notes.txt"),
        ];

        let roots = status_roots(&layout.app.join("src"), &touched, Some(&layout.home));

        assert_eq!(roots, [layout.app.clone(), other]);
    }

    #[test]
    fn status_leaves_out_a_checkout_that_holds_the_home_folder() {
        let layout = Layout::new();
        std::fs::create_dir(layout.home.join(GIT_ENTRY)).unwrap();
        let loose = layout.home.join("notes.txt");

        let roots = status_roots(&layout.home.join("docs"), &[loose], Some(&layout.home));

        assert_eq!(roots, Vec::<PathBuf>::new());
    }
}

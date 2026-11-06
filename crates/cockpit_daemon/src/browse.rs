//! `agent.list_dir` and `agent.find_files`: browsing the folders `agent.read_file` reads in, for
//! the app's "Open file" dialog.
//!
//! A folder lists when, once its symlinks are resolved, it is in the agent's folder or in a git
//! checkout that holds a file the agent read or changed: the folders of [`read_file`]'s module
//! doc. The files it allows alone allow no folder. The listing shows at most [`MAX_ENTRIES`]
//! entries, and reads at most [`MAX_READ`] of a folder's, so that a huge folder answers quickly.
//!
//! `folders.list` lists the folders alone, of any folder the daemon's user can read, for the new
//! agent dialog, within the same bounds.
//!
//! [`read_file`]: crate::read_file

use std::ffi::{CStr, OsStr};
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use cockpit_protocol::{DirEntry, DirListing, EntryKind, ErrorBody, ErrorCode};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;
use tokio::time::Instant;

use crate::read_file::{self, Access, ReadFileError, SEARCH_TIME};

/// The most entries a listing holds.
pub(crate) const MAX_ENTRIES: usize = 2_000;
/// The most entries read from a folder. Past it, the listing is cut.
pub(crate) const MAX_READ: usize = 20_000;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ListDirError {
    #[error("the folder is outside the agent's folder and the checkouts it worked in")]
    Outside,
    #[error("{} does not exist", .0.display())]
    Missing(PathBuf),
    #[error("{} is not a folder", .0.display())]
    NotAFolder(PathBuf),
    #[error("the folder {} is not absolute", .0.display())]
    NotAbsolute(PathBuf),
    #[error("cannot read the folder: {0}")]
    Io(#[source] io::Error),
    #[error(transparent)]
    Access(#[from] ReadFileError),
}

impl ListDirError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::Outside | Self::NotAFolder(_) | Self::NotAbsolute(_) => ErrorCode::InvalidRequest,
            Self::Missing(_) => ErrorCode::NotFound,
            Self::Io(_) => ErrorCode::FileUnreadable,
            Self::Access(error) => return error.to_error_body(),
        };
        ErrorBody::new(code, self.to_string())
    }
}

/// Lists `path`, absolute or relative to `folder`, the agent's folder. `touched` holds the files
/// the agent read or changed, and `home` the home folder, as for `agent.read_file`.
pub(crate) fn list(
    folder: &Path,
    touched: &[PathBuf],
    home: Option<&Path>,
    path: &Path,
) -> Result<DirListing, ListDirError> {
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ListDirError::Outside);
    }
    let access = Access::new(folder, touched, home)?;
    let asked = folder.join(path);
    let resolved = asked.canonicalize().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ListDirError::Missing(asked.clone()),
        _ => ListDirError::Io(error),
    })?;
    let holder = access
        .folder_holding(&resolved)
        .ok_or(ListDirError::Outside)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let opened =
        rustix::fs::open(&resolved, flags, Mode::empty()).map_err(|error| match error {
            Errno::NOTDIR => ListDirError::NotAFolder(asked.clone()),
            other => ListDirError::Io(other.into()),
        })?;
    let opened = File::from(opened);
    // The checks use the open folder, so a swap after them does not matter.
    read_file::ensure_inside(&opened, holder).map_err(|_| ListDirError::Outside)?;
    let (entries, truncated) = read_entries(&opened)?;
    Ok(DirListing {
        path: asked,
        entries,
        truncated,
    })
}

/// The entries of an open folder, its folders first, then the others, each group by name.
fn read_entries(folder: &File) -> Result<(Vec<DirEntry>, bool), ListDirError> {
    let mut dir = Dir::read_from(folder).map_err(|error| ListDirError::Io(error.into()))?;
    let mut entries: Vec<DirEntry> = Vec::new();
    let mut truncated = false;
    while let Some(entry) = dir.read() {
        let entry = entry.map_err(|error| ListDirError::Io(error.into()))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if entries.len() >= MAX_READ {
            truncated = true;
            break;
        }
        entries.push(describe(folder, entry.file_name(), entry.file_type()));
    }
    truncated |= sort_and_cap(&mut entries);
    Ok((entries, truncated))
}

/// Sorts entries as a listing shows them, then keeps the first [`MAX_ENTRIES`]. Returns whether
/// it left some out.
fn sort_and_cap(entries: &mut Vec<DirEntry>) -> bool {
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != EntryKind::Folder,
            entry.name.to_lowercase(),
            entry.name.clone(),
        )
    });
    let cut = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    cut
}

/// Lists the folders in `path`, which must be absolute. A symbolic link to a folder counts as one.
pub(crate) fn list_folders(path: &Path) -> Result<DirListing, ListDirError> {
    if !path.is_absolute() {
        return Err(ListDirError::NotAbsolute(path.to_owned()));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let opened = rustix::fs::open(path, flags, Mode::empty()).map_err(|error| match error {
        Errno::NOENT => ListDirError::Missing(path.to_owned()),
        Errno::NOTDIR => ListDirError::NotAFolder(path.to_owned()),
        other => ListDirError::Io(other.into()),
    })?;
    let (entries, truncated) = read_folders(&File::from(opened))?;
    Ok(DirListing {
        path: path.to_owned(),
        entries,
        truncated,
    })
}

/// The folders of an open folder, by name. Every entry read counts towards [`MAX_READ`], so a
/// folder of many files answers as quickly as for `agent.list_dir`.
fn read_folders(folder: &File) -> Result<(Vec<DirEntry>, bool), ListDirError> {
    let mut dir = Dir::read_from(folder).map_err(|error| ListDirError::Io(error.into()))?;
    let mut folders: Vec<DirEntry> = Vec::new();
    let mut read: usize = 0;
    let mut truncated = false;
    while let Some(entry) = dir.read() {
        let entry = entry.map_err(|error| ListDirError::Io(error.into()))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if read >= MAX_READ {
            truncated = true;
            break;
        }
        read += 1;
        let listed = entry.file_type();
        // The protocol carries paths as text, so no request could reach a folder whose name is
        // not UTF-8.
        let text_name = std::str::from_utf8(name).is_ok();
        // Only these can lead to a folder: the others need no `stat`.
        if text_name
            && matches!(
                listed,
                FileType::Directory | FileType::Symlink | FileType::Unknown
            )
        {
            let described = describe(folder, entry.file_name(), listed);
            if described.kind == EntryKind::Folder {
                folders.push(described);
            }
        }
    }
    truncated |= sort_and_cap(&mut folders);
    Ok((folders, truncated))
}

/// One entry, with what a symbolic link points to and a file's size. An entry that vanished since
/// the folder was read, or a link that leads nowhere, shows as `Other`.
fn describe(folder: &File, name: &CStr, listed: FileType) -> DirEntry {
    let shown_name = OsStr::from_bytes(name.to_bytes())
        .to_string_lossy()
        .into_owned();
    let symlink = listed == FileType::Symlink;
    if listed == FileType::Directory {
        return DirEntry {
            name: shown_name,
            kind: EntryKind::Folder,
            size: None,
            symlink,
        };
    }
    // Follows a link, to say what it points to.
    let stat = rustix::fs::statat(folder, name, AtFlags::empty()).ok();
    let kind = stat.as_ref().map_or(EntryKind::Other, |stat| {
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::Directory => EntryKind::Folder,
            FileType::RegularFile => EntryKind::File,
            FileType::Symlink
            | FileType::Fifo
            | FileType::Socket
            | FileType::CharacterDevice
            | FileType::BlockDevice
            | FileType::Unknown => EntryKind::Other,
        }
    });
    let size = stat
        .filter(|_| kind == EntryKind::File)
        .map(|stat| u64::try_from(stat.st_size).unwrap_or(0));
    DirEntry {
        name: shown_name,
        kind,
        size,
        symlink,
    }
}

/// The files whose name holds `name`, whatever its case, in the agent's folder and the allowed
/// checkouts, as `git ls-files` lists them, within the search's limits.
pub(crate) async fn find_files(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    name: String,
) -> Result<Vec<PathBuf>, ReadFileError> {
    let needle = name.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let (access, roots) = tokio::task::spawn_blocking(move || {
        let access = Access::new(&folder, &touched, home.as_deref())?;
        let roots = access.search_roots(folder);
        Ok::<_, ReadFileError>((access, roots))
    })
    .await
    .map_err(read_file::join_error)??;
    let name_holds = |relative: &Path| {
        relative
            .file_name()
            .is_some_and(|file| file.to_string_lossy().to_lowercase().contains(&needle))
    };
    Ok(read_file::search(&access, roots, name_holds, Instant::now() + SEARCH_TIME).await)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::process::Command;

    use super::*;

    /// Writes `text` at `path`, creating its folders.
    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn names(listing: &DirListing) -> Vec<(&str, EntryKind, Option<u64>, bool)> {
        listing
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind, entry.size, entry.symlink))
            .collect()
    }

    /// The agent's folder beside a worktree it worked in, one it did not, and a loose file it
    /// read. The temporary folder stands for the home folder, so that a checkout above it never
    /// counts.
    struct Layout {
        temp: tempfile::TempDir,
        agent: PathBuf,
        feature: PathBuf,
        untouched: PathBuf,
        loose: PathBuf,
    }

    impl Layout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let layout = Self {
                agent: root.join("app"),
                feature: root.join("app_feature"),
                untouched: root.join("app_other"),
                loose: root.join("notes"),
                temp,
            };
            write(&layout.agent.join("src/main.rs"), "fn main() {}\n");
            write(&layout.feature.join(".git"), "gitdir: /elsewhere\n");
            write(&layout.feature.join("src/app/touched.ts"), "touched\n");
            std::fs::create_dir_all(layout.untouched.join(".git")).unwrap();
            write(&layout.untouched.join("src/secret.ts"), "secret\n");
            write(&layout.loose.join("todo.txt"), "todo\n");
            layout
        }

        fn list(&self, path: &Path) -> Result<DirListing, ListDirError> {
            let touched = [
                self.feature.join("src/app/touched.ts"),
                self.loose.join("todo.txt"),
            ];
            let home = self.temp.path().canonicalize().unwrap();
            list(&self.agent, &touched, Some(&home), path)
        }
    }

    fn assert_outside(result: Result<DirListing, ListDirError>) {
        assert!(matches!(result, Err(ListDirError::Outside)), "{result:?}");
    }

    #[test]
    fn lists_folders_first_then_the_rest_by_name() {
        let layout = Layout::new();
        let src = layout.agent.join("src");
        write(&src.join("b.txt"), "bee");
        write(&src.join("A.md"), "# A\n");
        std::fs::create_dir(src.join("tests")).unwrap();
        symlink(src.join("tests"), src.join("linked")).unwrap();
        symlink(src.join("gone"), src.join("broken")).unwrap();

        let listing = layout.list(Path::new("src")).unwrap();

        assert_eq!(listing.path, layout.agent.join("src"));
        assert!(!listing.truncated);
        assert_eq!(
            names(&listing),
            [
                ("linked", EntryKind::Folder, None, true),
                ("tests", EntryKind::Folder, None, false),
                ("A.md", EntryKind::File, Some(4), false),
                ("b.txt", EntryKind::File, Some(3), false),
                ("broken", EntryKind::Other, None, true),
                ("main.rs", EntryKind::File, Some(13), false),
            ]
        );
    }

    #[test]
    fn lists_the_checkouts_the_agent_worked_in_and_refuses_the_rest() {
        let layout = Layout::new();

        let feature = layout.list(&layout.feature.join("src")).unwrap();

        assert_eq!(names(&feature), [("app", EntryKind::Folder, None, false)]);
        assert_outside(layout.list(&layout.untouched));
        // A file the agent read on its own opens, but its folder does not list.
        assert_outside(layout.list(&layout.loose));
        assert_outside(layout.list(layout.agent.parent().unwrap()));
        assert_outside(layout.list(Path::new("src/..")));
        let missing = layout.list(Path::new("gone")).unwrap_err();
        assert_eq!(missing.to_error_body().code, ErrorCode::NotFound);
        assert_eq!(
            missing.to_string(),
            format!("{} does not exist", layout.agent.join("gone").display())
        );
        assert!(matches!(
            layout.list(Path::new("src/main.rs")),
            Err(ListDirError::NotAFolder(_))
        ));
        assert_eq!(
            ListDirError::Outside.to_string(),
            "the folder is outside the agent's folder and the checkouts it worked in"
        );
    }

    #[test]
    fn a_symlinked_folder_lists_only_where_its_target_is_allowed() {
        let layout = Layout::new();
        symlink(layout.feature.join("src"), layout.agent.join("feature_src")).unwrap();
        symlink(&layout.untouched, layout.agent.join("escape")).unwrap();

        let through_link = layout.list(Path::new("feature_src")).unwrap();

        assert_eq!(through_link.path, layout.agent.join("feature_src"));
        assert_eq!(
            names(&through_link),
            [("app", EntryKind::Folder, None, false)]
        );
        assert_outside(layout.list(Path::new("escape")));
        assert_outside(layout.list(Path::new("escape/src")));
    }

    #[test]
    fn a_large_folder_is_cut_at_the_cap() {
        let layout = Layout::new();
        let many = layout.agent.join("many");
        std::fs::create_dir(&many).unwrap();
        for index in 0..MAX_ENTRIES + 5 {
            std::fs::write(many.join(format!("file-{index:05}.txt")), "").unwrap();
        }

        let listing = layout.list(Path::new("many")).unwrap();

        assert_eq!(listing.entries.len(), MAX_ENTRIES);
        assert!(listing.truncated);
        assert_eq!(
            listing.entries.first().map(|entry| entry.name.as_str()),
            Some("file-00000.txt")
        );
    }

    #[test]
    fn lists_the_folders_alone_hidden_ones_included_by_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for folder in ["beta", "Alpha/inner", ".config"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        write(&root.join("notes.txt"), "notes");
        write(&root.join("elsewhere/target/README.md"), "# Target\n");
        symlink(root.join("elsewhere/target"), root.join("linked")).unwrap();
        symlink(root.join("notes.txt"), root.join("file_link")).unwrap();
        symlink(root.join("gone"), root.join("broken")).unwrap();

        let listing = list_folders(&root).unwrap();

        assert_eq!(listing.path, root);
        assert!(!listing.truncated);
        assert_eq!(
            names(&listing),
            [
                (".config", EntryKind::Folder, None, false),
                ("Alpha", EntryKind::Folder, None, false),
                ("beta", EntryKind::Folder, None, false),
                ("elsewhere", EntryKind::Folder, None, false),
                ("linked", EntryKind::Folder, None, true),
            ]
        );
        let through_link = list_folders(&root.join("linked")).unwrap();
        assert!(through_link.entries.is_empty(), "{through_link:?}");
    }

    #[test]
    fn a_folder_whose_name_is_not_utf8_is_left_out_of_the_folders() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(OsStr::from_bytes(b"caf\xe9"))).unwrap();
        std::fs::create_dir(root.join("cafe")).unwrap();

        let listing = list_folders(&root).unwrap();

        assert_eq!(names(&listing), [("cafe", EntryKind::Folder, None, false)]);
    }

    #[test]
    fn a_folder_listing_refuses_a_relative_path_and_says_what_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        write(&root.join("notes.txt"), "notes");

        for relative in ["projects", "~/projects", "~"] {
            let refused = list_folders(Path::new(relative)).unwrap_err();
            assert!(
                matches!(refused, ListDirError::NotAbsolute(_)),
                "{relative}: {refused:?}"
            );
            assert_eq!(refused.to_error_body().code, ErrorCode::InvalidRequest);
        }
        assert_eq!(
            list_folders(Path::new("~/projects"))
                .unwrap_err()
                .to_string(),
            "the folder ~/projects is not absolute"
        );
        let missing = list_folders(&root.join("gone")).unwrap_err();
        assert_eq!(missing.to_error_body().code, ErrorCode::NotFound);
        assert!(matches!(
            list_folders(&root.join("notes.txt")),
            Err(ListDirError::NotAFolder(_))
        ));
    }

    #[test]
    fn a_folder_listing_sends_at_most_the_cap() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for index in 0..MAX_ENTRIES + 5 {
            std::fs::create_dir(root.join(format!("folder-{index:05}"))).unwrap();
        }

        let listing = list_folders(&root).unwrap();

        assert_eq!(listing.entries.len(), MAX_ENTRIES);
        assert!(listing.truncated);
        assert_eq!(
            listing.entries.first().map(|entry| entry.name.as_str()),
            Some("folder-00000")
        );
    }

    /// The files count towards the entries read, though none is sent.
    #[test]
    fn a_folder_listing_reads_at_most_the_cap() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for index in 0..MAX_READ + 5 {
            std::fs::write(root.join(format!("file-{index:05}.txt")), "").unwrap();
        }

        let listing = list_folders(&root).unwrap();

        assert!(listing.entries.is_empty());
        assert!(listing.truncated);
    }

    #[tokio::test]
    async fn finds_files_by_a_part_of_their_name_whatever_its_case() {
        let layout = Layout::new();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&layout.agent)
            .status()
            .expect("the search tests need git");
        assert!(status.success());
        write(&layout.agent.join("src/lexer.rs"), "");
        write(&layout.agent.join("tests/Lexer_cases.rs"), "");
        write(&layout.agent.join("docs/notes.md"), "");
        let home = Some(layout.temp.path().canonicalize().unwrap());
        let find = |name: &str| {
            find_files(
                layout.agent.clone(),
                Vec::new(),
                home.clone(),
                name.to_owned(),
            )
        };

        let mut found = find("LEXER").await.unwrap();
        found.sort();

        assert_eq!(
            found,
            [
                layout.agent.join("src/lexer.rs"),
                layout.agent.join("tests/Lexer_cases.rs"),
            ]
        );
        assert!(find("  ").await.unwrap().is_empty());
    }
}

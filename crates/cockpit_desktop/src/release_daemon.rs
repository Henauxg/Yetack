//! The daemon of this app's release for a CPU it carries none for, downloaded from the release.
//! The server's script then checks the daemon's version against this app's own build, so the app
//! first asks GitHub which commit the release's tag names: a build between two releases has the
//! earlier one's version, and its release's daemon would fail that check.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::config::Environment;
use crate::prefs::PREFS_FOLDER;
use crate::self_update::{self, DOWNLOAD_TIME, MAX_ANSWER_BYTES};
use crate::state::app_update::{self, CHECKSUMS_NAME, Checksums, MAX_DOWNLOAD_BYTES};
use crate::state::daemon_probe::LinuxCpu;
use crate::update_script::{self, APP_VERSION};

const DOWNLOADS: &str = concat!(
    "https://github.com/",
    app_update::repository!(),
    "/releases/download"
);
const COMMITS: &str = concat!(
    "https://api.github.com/repos/",
    app_update::repository!(),
    "/commits"
);
/// GitHub's answer for a commit, as its SHA alone.
const SHA_ONLY: &str = "application/vnd.github.sha";

/// A commit's SHA in hex, with room to spare.
const MAX_SHA_BYTES: u64 = 128;

/// The folder under the app's own where downloaded daemons stay, so that the next install or
/// update sends them again without a download.
const DAEMONS_FOLDER: &str = "daemons";

/// Joins the daemon's name and the random part that `tempfile` adds, in a download's file name.
const PART_SEPARATOR: &str = ".";

/// Whether this app is its release's own build, once GitHub said it.
static RELEASE_BUILD: OnceLock<bool> = OnceLock::new();

/// Whether this app is its release's own build: the release's tag names its commit. GitHub's
/// answer is kept for the app's run. A question that fails says no, and is asked again the next
/// time, so that a server's screens never promise a daemon the app cannot download.
pub async fn is_release_build() -> bool {
    if let Some(&known) = RELEASE_BUILD.get() {
        return known;
    }
    let asked = tokio::task::spawn_blocking(ask_tag)
        .await
        .map_err(|error| error.to_string())
        .and_then(|asked| asked);
    match asked {
        Ok(answer) => *RELEASE_BUILD.get_or_init(|| answer),
        Err(error) => {
            tracing::warn!("cannot tell whether this app is its release's build: {error}");
            false
        }
    }
}

/// What `is_release_build` last learned, without asking: `false` before it learned anything.
pub fn known_release_build() -> bool {
    RELEASE_BUILD.get() == Some(&true)
}

/// Asks GitHub which commit the release's tag names, and whether it is this app's.
fn ask_tag() -> Result<bool, String> {
    let commit = update_script::app_commit()
        .ok_or("This app does not know the commit it was built from.")?;
    let tag = format!("v{APP_VERSION}");
    let client = self_update::http_client(Some(DOWNLOAD_TIME));
    let tagged = read_text(
        &client,
        &format!("{COMMITS}/{tag}"),
        SHA_ONLY,
        MAX_SHA_BYTES,
    )
    .map_err(|error| format!("Could not ask GitHub for release {tag}: {error}"))?;
    Ok(is_this_build(&tagged, commit.as_str()))
}

/// The daemon of this app's release for `cpu`, in the app's folder: downloaded once, and checked
/// against the release's `SHA256SUMS` each time. `Err` says why there is none, for the user.
pub async fn for_cpu(cpu: LinuxCpu) -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(move || fetch(cpu))
        .await
        .map_err(|error| error.to_string())?
}

fn fetch(cpu: LinuxCpu) -> Result<PathBuf, String> {
    use strum::VariantArray;

    let tag = format!("v{APP_VERSION}");
    let this_build = match RELEASE_BUILD.get() {
        Some(&known) => known,
        None => {
            let answer = ask_tag()?;
            *RELEASE_BUILD.get_or_init(|| answer)
        }
    };
    if !this_build {
        return Err(format!(
            "This app is not the build of release {tag}, so the release's daemon would not match it."
        ));
    }
    let client = self_update::http_client(Some(DOWNLOAD_TIME));
    let name = cpu.release_file_name(APP_VERSION);
    let sums = read_text(
        &client,
        &format!("{DOWNLOADS}/{tag}/{CHECKSUMS_NAME}"),
        "*/*",
        MAX_ANSWER_BYTES,
    )
    .map_err(|error| format!("Could not download the checksums of release {tag}: {error}"))?;
    let sums = Checksums::parse(&sums);
    let sum = sums
        .of(&name)
        .ok_or_else(|| format!("Release {tag} has no daemon for Linux on {}.", cpu.word()))?;
    let folder =
        daemons_folder().ok_or("This computer has no home folder to keep the daemon in.")?;
    fs::create_dir_all(&folder)
        .map_err(|error| format!("Could not create {}: {error}", folder.display()))?;
    let file = folder.join(&name);
    if self_update::sha256_of(&file).is_ok_and(|kept| kept == sum) {
        return Ok(file);
    }
    let url = format!("{DOWNLOADS}/{tag}/{name}");
    download(&client, &url, &file, sum)
        .map_err(|error| format!("Could not download {name}: {error}"))?;
    let kept: Vec<String> = LinuxCpu::VARIANTS
        .iter()
        .map(|cpu| cpu.release_file_name(APP_VERSION))
        .collect();
    remove_other_releases(&folder, &kept);
    Ok(file)
}

/// Whether GitHub's answer names this app's commit. The server's script compares the commit the
/// daemon prints with the app's as text, so a short form of the same commit would fail there.
fn is_this_build(tagged: &str, ours: &str) -> bool {
    tagged.trim() == ours
}

fn daemons_folder() -> Option<PathBuf> {
    Some(
        Environment::from_process()
            .state_base()?
            .join(PREFS_FOLDER)
            .join(DAEMONS_FOLDER),
    )
}

fn read_text(
    client: &ureq::Agent,
    url: &str,
    accept: &str,
    limit: u64,
) -> Result<String, ureq::Error> {
    client
        .get(url)
        .header("Accept", accept)
        .call()?
        .into_body()
        .into_with_config()
        .limit(limit)
        .read_to_string()
}

/// Downloads `url` to `file`, which takes the download's place only once its SHA-256 is `sum`.
fn download(client: &ureq::Agent, url: &str, file: &Path, sum: &str) -> Result<(), String> {
    let body = client
        .get(url)
        .call()
        .map_err(|error| error.to_string())?
        .into_body()
        .into_with_config()
        .limit(MAX_DOWNLOAD_BYTES)
        .reader();
    save(body, file, sum)
}

/// Writes `body` to `file` through a `.part` file of its own beside it, so that two downloads of
/// one daemon at once (the updates of two servers) never write into the same file.
fn save(mut body: impl Read, file: &Path, sum: &str) -> Result<(), String> {
    let mut prefix = file
        .file_name()
        .ok_or("the daemon's path has no file name")?
        .to_owned();
    prefix.push(PART_SEPARATOR);
    let mut part = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".part")
        .tempfile_in(file.parent().unwrap_or(Path::new(".")))
        .map_err(|error| error.to_string())?;
    let read = copy_hashing(&mut body, part.as_file_mut()).map_err(|error| error.to_string())?;
    if read != sum {
        return Err("its checksum is not the release's".to_owned());
    }
    part.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    executable(part.path()).map_err(|error| error.to_string())?;
    match part.persist(file) {
        Ok(_) => Ok(()),
        // Windows replaces no file that a send of the same daemon reads.
        Err(_) if self_update::sha256_of(file).is_ok_and(|kept| kept == sum) => Ok(()),
        Err(error) => Err(error.error.to_string()),
    }
}

/// Deletes what `folder` holds of other releases: only this release's daemons, the `kept` files,
/// are ever sent again. Their `.part` files stay, since a download may be writing one.
fn remove_other_releases(folder: &Path, kept: &[String]) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let keeps = kept.iter().any(|file| {
            name.strip_prefix(file.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(PART_SEPARATOR))
        });
        if !keeps {
            // A file that stays goes at the next download.
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Copies `from` to `to` and returns the SHA-256 of what went through, in lowercase hex.
fn copy_hashing(from: &mut impl Read, to: &mut impl Write) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; 256 * 1024];
    loop {
        let read = from.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let bytes = chunk.get(..read).unwrap_or_default();
        hasher.update(bytes);
        to.write_all(bytes)?;
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(unix)]
fn executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// The server's script makes the file executable there.
#[cfg(not(unix))]
fn executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SHA-256 of "abc".
    const ABC_SUM: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    /// Gives `before`, runs `meanwhile`, then gives `after`.
    struct Paused<'bytes, F: FnOnce()> {
        before: &'bytes [u8],
        meanwhile: Option<F>,
        after: &'bytes [u8],
    }

    impl<F: FnOnce()> Read for Paused<'_, F> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.before.is_empty() {
                return self.before.read(buf);
            }
            if let Some(meanwhile) = self.meanwhile.take() {
                meanwhile();
            }
            self.after.read(buf)
        }
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_the_full_commit_names_this_build() {
        let full = "3f9c2e1a7b0d4c5e6f8091a2b3c4d5e6f7081920";

        assert!(is_this_build(&format!("{full}\n"), full));
        assert!(
            !is_this_build(full, "3f9c2e1"),
            "the script compares the text"
        );
        assert!(!is_this_build("", full));
    }

    #[test]
    fn a_copy_hashes_what_it_writes() {
        let mut written = Vec::new();

        let sum = copy_hashing(&mut &b"abc"[..], &mut written).unwrap();

        assert_eq!(written, b"abc");
        assert_eq!(sum, ABC_SUM);
    }

    #[test]
    fn two_downloads_of_one_daemon_at_once_both_end_with_the_whole_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("yetack-9.1.0-aarch64-linux");
        let body = Paused {
            before: b"ab",
            meanwhile: Some(|| save(&b"abc"[..], &file, ABC_SUM).unwrap()),
            after: b"c",
        };

        save(body, &file, ABC_SUM).unwrap();

        assert_eq!(fs::read(&file).unwrap(), b"abc");
        assert_eq!(names(temp.path()), ["yetack-9.1.0-aarch64-linux"]);
    }

    #[test]
    fn a_download_with_another_checksum_leaves_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("yetack-9.1.0-aarch64-linux");

        let saved = save(&b"abd"[..], &file, ABC_SUM);

        assert_eq!(saved, Err("its checksum is not the release's".to_owned()));
        assert_eq!(names(temp.path()), Vec::<String>::new());
    }

    #[test]
    fn a_download_deletes_the_daemons_of_other_releases() {
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "yetack-9.0.0-aarch64-linux",
            "yetack-9.0.0-x86_64-linux.Ab12Cd.part",
            "yetack-9.1.0-aarch64-linux",
            "yetack-9.1.0-aarch64-linux.Ef56Gh.part",
            "yetack-9.1.0-x86_64-linux",
        ] {
            fs::write(temp.path().join(name), "daemon").unwrap();
        }

        let kept = ["yetack-9.1.0-aarch64-linux", "yetack-9.1.0-x86_64-linux"].map(str::to_owned);
        remove_other_releases(temp.path(), &kept);

        assert_eq!(
            names(temp.path()),
            [
                "yetack-9.1.0-aarch64-linux",
                "yetack-9.1.0-aarch64-linux.Ef56Gh.part",
                "yetack-9.1.0-x86_64-linux"
            ]
        );
    }
}

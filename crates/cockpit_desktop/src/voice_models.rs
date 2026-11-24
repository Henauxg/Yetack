//! The speech models' download: files pinned to a commit, checked by their SHA-256, and kept in
//! the app's state folder. The language in Settings picks the model.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cockpit_voice::{ModelFile, SILERO, SpeechModel};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Environment;
use crate::prefs::{DictationLanguage, PREFS_FOLDER};
use crate::self_update;

const MODELS_FOLDER: &str = "voice-models";
/// The longest a model file may take to download: a speech model is about 700 MB.
const DOWNLOAD_TIME: Duration = Duration::from_secs(4 * 3600);
const READ_CHUNK: usize = 256 * 1024;
/// The progress goes out each time this much more has arrived.
const PROGRESS_STEP: u64 = 1024 * 1024;
/// A file being downloaded ends with this, after the model file's name and random letters.
const PART_SUFFIX: &str = ".part";

/// What a download reports.
#[derive(Debug)]
pub enum DownloadNews {
    /// Bytes downloaded, over all the files.
    Progress(u64),
    Done(Result<(), String>),
}

pub fn folder() -> Option<PathBuf> {
    Some(
        Environment::from_process()
            .state_base()?
            .join(PREFS_FOLDER)
            .join(MODELS_FOLDER),
    )
}

/// The model that transcribes `language`.
pub fn speech_model(language: DictationLanguage) -> SpeechModel {
    match language {
        DictationLanguage::English => SpeechModel::UnifiedEn,
        DictationLanguage::Multilingual => SpeechModel::Ultra,
    }
}

/// The model of the other language.
pub fn other(model: SpeechModel) -> SpeechModel {
    match model {
        SpeechModel::UnifiedEn => SpeechModel::Ultra,
        SpeechModel::Ultra => SpeechModel::UnifiedEn,
    }
}

/// The size of every file `model` needs, together.
pub fn total_size(model: SpeechModel) -> u64 {
    model.files().iter().map(|file| file.size).sum()
}

/// Whether every file `model` needs is in `folder`, whole: its size is right, and a marker beside
/// it holds the hash its download checked. Hashing 700 MB at each start would take seconds.
pub fn present(folder: &Path, model: SpeechModel) -> bool {
    model.files().iter().all(|file| whole(folder, file))
}

fn whole(folder: &Path, file: &ModelFile) -> bool {
    fs::metadata(folder.join(file.name)).is_ok_and(|meta| meta.len() == file.size)
        && fs::read_to_string(marker(folder, file)).is_ok_and(|sum| sum.trim() == file.sha256)
}

fn marker(folder: &Path, file: &ModelFile) -> PathBuf {
    folder.join(format!("{}.sha256", file.name))
}

/// Downloads the files of `model` that `folder` lacks. It stops when `news` closes.
pub fn download(
    folder: &Path,
    model: SpeechModel,
    news: &UnboundedSender<DownloadNews>,
) -> Result<(), String> {
    fs::create_dir_all(folder)
        .map_err(|error| format!("Could not create {}: {error}", folder.display()))?;
    remove_parts(folder);
    let client = self_update::http_client(Some(DOWNLOAD_TIME));
    let mut done = 0;
    for file in &model.files() {
        if !whole(folder, file) {
            download_file(&client, folder, file, done, news)
                .map_err(|error| format!("Could not download {}: {error}", file.name))?;
        }
        done += file.size;
    }
    Ok(())
}

/// The app can end during a download without deleting its `.part` file, which can hold most of
/// a model's 700 MB.
fn remove_parts(folder: &Path) {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!("cannot list {}: {error}", folder.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let part =
            name.ends_with(PART_SUFFIX) && known_files().any(|file| name.starts_with(file.name));
        if part && let Err(error) = fs::remove_file(entry.path()) {
            tracing::warn!("cannot delete {}: {error}", entry.path().display());
        }
    }
}

/// Every file the app downloads, for any model.
fn known_files() -> impl Iterator<Item = ModelFile> {
    SpeechModel::ALL
        .into_iter()
        .map(SpeechModel::file)
        .chain([SILERO])
}

fn download_file(
    client: &ureq::Agent,
    folder: &Path,
    file: &ModelFile,
    before: u64,
    news: &UnboundedSender<DownloadNews>,
) -> Result<(), String> {
    let body = client
        .get(file.url)
        .call()
        .map_err(|error| error.to_string())?
        .into_body()
        .into_with_config()
        .limit(file.size + 1)
        .reader();
    save(folder, file, body, before, news)
}

/// Writes the file through a `.part` file beside it, which takes the file's place only once its
/// size and SHA-256 are the pinned ones.
fn save(
    folder: &Path,
    file: &ModelFile,
    mut body: impl Read,
    before: u64,
    news: &UnboundedSender<DownloadNews>,
) -> Result<(), String> {
    let mut part = tempfile::Builder::new()
        .prefix(file.name)
        .suffix(PART_SUFFIX)
        .tempfile_in(folder)
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; READ_CHUNK];
    let mut read = 0;
    let mut reported = 0;
    loop {
        if news.is_closed() {
            return Err("cancelled".to_owned());
        }
        let count = body.read(&mut chunk).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        let bytes = chunk.get(..count).unwrap_or_default();
        hasher.update(bytes);
        part.write_all(bytes).map_err(|error| error.to_string())?;
        read += count as u64;
        if read - reported >= PROGRESS_STEP {
            reported = read;
            // A closed channel stops the download at the next chunk.
            let _ = news.send(DownloadNews::Progress(before + read));
        }
    }
    if read != file.size {
        return Err(format!("it has {read} bytes, not {}", file.size));
    }
    let sum: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if sum != file.sha256 {
        return Err("its checksum is not the pinned one".to_owned());
    }
    part.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    part.persist(folder.join(file.name))
        .map_err(|error| error.error.to_string())?;
    fs::write(marker(folder, file), file.sha256).map_err(|error| error.to_string())
}

/// Deletes the file of `model`, and Silero's once no other speech model needs it. On Windows, a
/// file the worker still maps cannot go yet: its marker goes after it, so that a file that stays
/// still counts as whole.
pub fn remove(folder: &Path, model: SpeechModel) -> io::Result<()> {
    let others_left = SpeechModel::ALL
        .into_iter()
        .any(|other| other != model && whole(folder, &other.file()));
    let files = if others_left {
        vec![model.file()]
    } else {
        model.files().to_vec()
    };
    for file in &files {
        for path in [folder.join(file.name), marker(folder, file)] {
            match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                Ok(()) | Err(_) => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const ENGLISH: SpeechModel = SpeechModel::UnifiedEn;

    /// Every file of `model` in `folder`, whole, so that a download has nothing to fetch.
    pub(crate) fn whole_files(folder: &Path, model: SpeechModel) {
        for file in &model.files() {
            fs::File::create(folder.join(file.name))
                .and_then(|created| created.set_len(file.size))
                .unwrap();
            fs::write(marker(folder, file), file.sha256).unwrap();
        }
    }

    #[test]
    fn a_file_without_its_marker_is_not_whole() {
        let folder = tempfile::tempdir().unwrap();
        for file in &ENGLISH.files() {
            fs::File::create(folder.path().join(file.name))
                .and_then(|created| created.set_len(file.size))
                .unwrap();
        }
        assert!(!present(folder.path(), ENGLISH));
        for file in &ENGLISH.files() {
            fs::write(marker(folder.path(), file), file.sha256).unwrap();
        }
        assert!(present(folder.path(), ENGLISH));
        assert!(!present(folder.path(), SpeechModel::Ultra));
        assert!(remove(folder.path(), ENGLISH).is_ok());
        assert!(!present(folder.path(), ENGLISH));
    }

    #[test]
    fn removing_one_model_keeps_the_other_and_the_silero_it_needs() {
        let folder = tempfile::tempdir().unwrap();
        whole_files(folder.path(), ENGLISH);
        whole_files(folder.path(), SpeechModel::Ultra);

        assert!(remove(folder.path(), SpeechModel::Ultra).is_ok());
        let english_stays = present(folder.path(), ENGLISH);
        assert!(remove(folder.path(), ENGLISH).is_ok());

        assert!(english_stays);
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
    }

    #[test]
    fn each_language_has_its_model_and_the_other_language_the_other() {
        let english = speech_model(DictationLanguage::English);
        let multilingual = speech_model(DictationLanguage::Multilingual);

        assert_eq!(english, SpeechModel::UnifiedEn);
        assert_eq!(multilingual, SpeechModel::Ultra);
        assert_eq!(other(english), multilingual);
        assert_eq!(other(multilingual), english);
    }

    #[test]
    fn a_download_deletes_the_parts_an_earlier_one_left() {
        let folder = tempfile::tempdir().unwrap();
        whole_files(folder.path(), ENGLISH);
        let left = folder
            .path()
            .join(format!("{}k3XyZq.part", SpeechModel::Ultra.file().name));
        let other = folder.path().join("notes.part");
        fs::write(&left, "half a model").unwrap();
        fs::write(&other, "not a model").unwrap();
        let (news, _heard) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(download(folder.path(), ENGLISH, &news), Ok(()));
        assert!(!left.exists());
        assert!(other.exists());
        assert!(present(folder.path(), ENGLISH));
    }

    const HELLO: ModelFile = ModelFile {
        name: "hello.gguf",
        url: "https://example.com/hello.gguf",
        size: 5,
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    };

    #[test]
    fn a_download_stays_only_with_the_pinned_size_and_checksum() {
        let folder = tempfile::tempdir().unwrap();
        let (news, _heard) = tokio::sync::mpsc::unbounded_channel();
        let names = || -> Vec<String> {
            fs::read_dir(folder.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        };

        let altered = save(folder.path(), &HELLO, "hellO".as_bytes(), 0, &news);
        let short = save(folder.path(), &HELLO, "hell".as_bytes(), 0, &news);
        let long = save(folder.path(), &HELLO, "hello!".as_bytes(), 0, &news);

        assert!(altered.is_err_and(|error| error.contains("checksum")));
        assert!(short.is_err_and(|error| error.contains("4 bytes")));
        assert!(long.is_err_and(|error| error.contains("6 bytes")));
        assert!(names().is_empty(), "no file, marker or part stays");
        assert_eq!(
            save(folder.path(), &HELLO, "hello".as_bytes(), 0, &news),
            Ok(())
        );
        assert!(whole(folder.path(), &HELLO));
        let mut saved = names();
        saved.sort();
        assert_eq!(saved, ["hello.gguf", "hello.gguf.sha256"]);
    }

    #[test]
    fn a_closed_channel_cancels_the_download() {
        let folder = tempfile::tempdir().unwrap();
        let (news, heard) = tokio::sync::mpsc::unbounded_channel();
        drop(heard);

        let saved = save(folder.path(), &HELLO, "hello".as_bytes(), 0, &news);

        assert_eq!(saved, Err("cancelled".to_owned()));
        assert!(!whole(folder.path(), &HELLO));
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_file_that_cannot_go_keeps_its_marker() {
        let folder = tempfile::tempdir().unwrap();
        let file = &ENGLISH.file();
        // A folder in the file's place: deleting it as a file fails on every system.
        fs::create_dir(folder.path().join(file.name)).unwrap();
        fs::write(marker(folder.path(), file), file.sha256).unwrap();

        assert!(remove(folder.path(), ENGLISH).is_err());
        assert!(marker(folder.path(), file).exists());
    }
}

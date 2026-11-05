use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::store::error::StoreError;
use crate::store::state_dir::FILE_MODE;

const TEMPORARY_SUFFIX: &str = ".tmp";

/// Reads a JSON file, or the default value when the file does not exist.
pub(crate) fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(source) => {
            return Err(StoreError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    serde_json::from_slice(&bytes).map_err(|source| StoreError::Malformed {
        path: path.to_owned(),
        source,
    })
}

/// Writes a JSON file so that a reader sees the old content or the new one, never a part.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| StoreError::Encode {
        path: path.to_owned(),
        source,
    })?;
    bytes.push(b'\n');
    write_atomic(path, &bytes).map_err(|source| StoreError::Write {
        path: path.to_owned(),
        source,
    })
}

/// Writes a temporary file beside `path`, syncs it, then renames it over `path`.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(FILE_MODE)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    if let Some(folder) = path.parent() {
        // The rename is durable once the folder is synced.
        File::open(folder)?.sync_all()?;
    }
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(TEMPORARY_SUFFIX);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn writes_then_reads_back_and_leaves_no_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        let value = BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 2)]);

        write_json(&path, &value).unwrap();
        write_json(&path, &value).unwrap();

        let read: BTreeMap<String, u32> = read_json(&path).unwrap();
        assert_eq!(read, value);
        let names: Vec<OsString> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("agents.json")]);
    }

    #[test]
    fn a_missing_file_reads_as_the_default() {
        let temp = tempfile::tempdir().unwrap();

        let read: BTreeMap<String, u32> = read_json(&temp.path().join("missing.json")).unwrap();

        assert!(read.is_empty());
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agents.json");
        std::fs::write(&path, b"{not json").unwrap();

        let read = read_json::<BTreeMap<String, u32>>(&path);

        assert!(
            matches!(read, Err(StoreError::Malformed { .. })),
            "{read:?}"
        );
    }
}

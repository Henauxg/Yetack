//! The Windows calls the file requests make on an open file or folder: where it is once its
//! links are resolved, and a folder's entries, read through the open handle so that a folder
//! swapped for a link after the checks is not the one read.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem;
use std::ops::ControlFlow;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FULL_DIR_INFO, FILE_NAME_NORMALIZED, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
};

/// The longest path Windows gives, in UTF-16 units, with its final NUL.
const MAX_PATH_UNITS: usize = 32_768;
/// Entries are read 64 KiB at a time, in 8-byte words: the records need that alignment.
const ENTRY_BUFFER_WORDS: usize = 8 * 1024;

/// Opens a file or a folder to read. Others may still read, write and delete it.
pub(crate) fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// Where the open file is, with its links resolved, as a verbatim path: `\\?\C:\...` or
/// `\\?\UNC\host\share\...`.
pub(crate) fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer: Vec<u16> = vec![0; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open, and the buffer holds `capacity` UTF-16 units.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        // Without room, the length counts the final NUL; with room, it does not.
        if let Some(path) = buffer.get(..length).filter(|_| length < buffer.len()) {
            return Ok(PathBuf::from(OsString::from_wide(path)));
        }
        if length > MAX_PATH_UNITS {
            return Err(io::Error::other("the file's path is too long"));
        }
        buffer.resize(length, 0);
    }
}

/// One entry of a folder, as the folder lists it: a link is not followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FolderEntry {
    pub(crate) name: OsString,
    attributes: u32,
    /// The size of a file, in bytes.
    pub(crate) size: u64,
    /// What a reparse point is, such as a symbolic link.
    reparse_tag: u32,
}

impl FolderEntry {
    pub(crate) fn is_folder(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }

    /// A symbolic link or a junction. Other reparse points, such as files kept in the cloud,
    /// are what their attributes say.
    pub(crate) fn is_link(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            && matches!(
                self.reparse_tag,
                IO_REPARSE_TAG_SYMLINK | IO_REPARSE_TAG_MOUNT_POINT
            )
    }
}

/// Hands each entry of the open folder `folder` to `each`, but `.` and `..`, until `each` breaks.
pub(crate) fn read_entries(
    folder: &File,
    mut each: impl FnMut(FolderEntry) -> ControlFlow<()>,
) -> io::Result<()> {
    let mut buffer: Vec<u64> = vec![0; ENTRY_BUFFER_WORDS];
    let mut class = FileFullDirectoryRestartInfo;
    loop {
        let size = u32::try_from(buffer.len() * mem::size_of::<u64>()).unwrap_or(u32::MAX);
        // SAFETY: the handle is an open folder, and the buffer holds `size` bytes, aligned for
        // the records this class writes.
        let read = unsafe {
            GetFileInformationByHandleEx(
                folder.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                size,
            )
        };
        if read == 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(code) if u32::try_from(code) == Ok(ERROR_NO_MORE_FILES) => Ok(()),
                _ => Err(error),
            };
        }
        class = FileFullDirectoryInfo;
        // SAFETY: the bytes of the words, which any byte value is valid for.
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr().cast(), buffer.len() * mem::size_of::<u64>())
        };
        if parse_entries(bytes, &mut each).is_break() {
            return Ok(());
        }
    }
}

/// Hands the `FILE_FULL_DIR_INFO` records of `bytes` to `each`. A record that does not fit
/// ends the list.
fn parse_entries(
    bytes: &[u8],
    each: &mut impl FnMut(FolderEntry) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let mut offset: usize = 0;
    loop {
        let Some(record) = bytes.get(offset..) else {
            return ControlFlow::Continue(());
        };
        let Some((next, entry)) = parse_record(record) else {
            return ControlFlow::Continue(());
        };
        let dots = entry.name == "." || entry.name == "..";
        if !dots {
            each(entry)?;
        }
        if next == 0 {
            return ControlFlow::Continue(());
        }
        offset = offset.saturating_add(next);
    }
}

/// The offset of the next record, and the entry.
fn parse_record(record: &[u8]) -> Option<(usize, FolderEntry)> {
    let u32_at = |offset: usize| {
        let bytes = record.get(offset..offset.checked_add(4)?)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    };
    let next = u32_at(mem::offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset))?;
    let size_at = mem::offset_of!(FILE_FULL_DIR_INFO, EndOfFile);
    let size_bytes = record.get(size_at..size_at.checked_add(8)?)?;
    let size = i64::from_le_bytes(size_bytes.try_into().ok()?);
    let attributes = u32_at(mem::offset_of!(FILE_FULL_DIR_INFO, FileAttributes))?;
    let name_bytes = u32_at(mem::offset_of!(FILE_FULL_DIR_INFO, FileNameLength))?;
    // For a reparse point, this field holds its tag.
    let reparse_tag = u32_at(mem::offset_of!(FILE_FULL_DIR_INFO, EaSize))?;
    let name_at = mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
    let name_end = name_at.checked_add(usize::try_from(name_bytes).ok()?)?;
    let units: Vec<u16> = record
        .get(name_at..name_end)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let entry = FolderEntry {
        name: OsString::from_wide(&units),
        attributes,
        size: u64::try_from(size).unwrap_or(0),
        reparse_tag,
    };
    Some((usize::try_from(next).ok()?, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(folder: &Path) -> Vec<FolderEntry> {
        let mut entries = Vec::new();
        read_entries(&open(folder).unwrap(), |entry| {
            entries.push(entry);
            ControlFlow::Continue(())
        })
        .unwrap();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    #[test]
    fn an_open_folder_lists_its_entries_with_their_kind_and_size() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("notes.txt"), "four").unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();

        let entries = read(temp.path());

        let described: Vec<(&OsString, bool, u64)> = entries
            .iter()
            .map(|entry| (&entry.name, entry.is_folder(), entry.size))
            .collect();
        assert_eq!(
            described,
            [
                (&OsString::from("notes.txt"), false, 4),
                (&OsString::from("src"), true, 0)
            ]
        );
        assert!(entries.iter().all(|entry| !entry.is_link()));
    }

    #[test]
    fn a_folder_of_many_entries_is_read_whole_and_stops_when_asked() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..3_000 {
            std::fs::write(temp.path().join(format!("file-{index:05}.txt")), "").unwrap();
        }
        let folder = open(temp.path()).unwrap();

        let mut seen: usize = 0;
        read_entries(&folder, |_| {
            seen += 1;
            ControlFlow::Continue(())
        })
        .unwrap();
        let mut first: usize = 0;
        read_entries(&folder, |_| {
            first += 1;
            if first == 10 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();

        assert_eq!(seen, 3_000);
        assert_eq!(first, 10);
    }

    #[test]
    fn the_final_path_of_an_open_file_is_its_verbatim_path() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        std::fs::write(&path, "notes").unwrap();

        let opened = final_path(&open(&path).unwrap()).unwrap();

        assert_eq!(opened, path.canonicalize().unwrap());
    }
}

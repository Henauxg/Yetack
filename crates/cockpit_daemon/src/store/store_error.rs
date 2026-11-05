use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot create the folder {}", path.display())]
    CreateFolder {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The lock is held: a daemon already serves this state folder.
    #[error("another daemon already runs on {}", path.display())]
    AlreadyRunning { path: PathBuf },
    #[error("cannot lock {}", path.display())]
    Lock {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot read {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not valid", path.display())]
    Malformed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("cannot encode {}", path.display())]
    Encode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("cannot start {}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} started without its pipes", program.display())]
    Pipes { program: PathBuf },
}

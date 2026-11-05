//! The app's own preferences, `$XDG_STATE_HOME/agents-cockpit-desktop/prefs.json`: choices made in
//! the window that last across restarts.
//!
//! ```json
//! {
//!   "file_view": {
//!     "width_share": 0.42,
//!     "wrap": true
//!   }
//! }
//! ```
//!
//! The file is on the machine the app runs on, in a folder apart from the daemon's, which holds a
//! local daemon's socket. A missing or unreadable file means the defaults, and a field the app
//! does not know is ignored.

use std::ffi::OsString;
use std::fs::{DirBuilder, File, OpenOptions, Permissions};
use std::io;
use std::path::{Path, PathBuf};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::config::Environment;
use crate::connection::Cockpit;

const PREFS_FOLDER: &str = "agents-cockpit-desktop";
const PREFS_FILE: &str = "prefs.json";
const TEMPORARY_SUFFIX: &str = ".tmp";

/// The same modes as the daemon's files: only their owner reads them.
const FOLDER_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub file_view: FileViewPrefs,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileViewPrefs {
    /// The file view's width as a share of the window's, between 0 and 1. `None` keeps the
    /// default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_share: Option<f64>,
    /// Long lines wrap instead of scrolling sideways.
    pub wrap: bool,
}

/// The file view's width, as a share of the window's, until it is moved.
const DEFAULT_WIDTH_SHARE: f64 = 0.35;

impl FileViewPrefs {
    /// The file view's width as a share of the window's.
    pub fn share(&self) -> f64 {
        self.width_share.unwrap_or(DEFAULT_WIDTH_SHARE)
    }
}

/// Whether `share` can size a pane: a number strictly between 0 and 1.
pub fn is_share(share: f64) -> bool {
    share > 0.0 && share < 1.0
}

/// Where the preferences live, or `None` without a home folder.
pub fn file(environment: &Environment) -> Option<PathBuf> {
    Some(
        environment
            .state_base()?
            .join(PREFS_FOLDER)
            .join(PREFS_FILE),
    )
}

impl Prefs {
    /// Reads `file`. A missing file, or one that does not read or parse, gives the defaults: a
    /// preference never stops the app.
    pub fn load(file: &Path) -> Self {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!(
                    "cannot read {}, using the defaults: {error}",
                    file.display()
                );
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(prefs) => prefs.checked(),
            Err(error) => {
                tracing::warn!(
                    "{} is not valid, using the defaults: {error}",
                    file.display()
                );
                Self::default()
            }
        }
    }

    /// Drops the values the window cannot use.
    fn checked(mut self) -> Self {
        self.file_view.width_share = self.file_view.width_share.filter(|&share| is_share(share));
        self
    }

    /// Writes `file` so that a reader sees the old content or the new one, never a part: a
    /// temporary file beside it, synced, then renamed over it.
    pub fn save(&self, file: &Path) -> io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        if let Some(folder) = file.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(FOLDER_MODE)
                .create(folder)?;
            // An existing folder keeps its mode otherwise.
            std::fs::set_permissions(folder, Permissions::from_mode(FOLDER_MODE))?;
        }
        let temporary = temporary_path(file);
        let mut out = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(FILE_MODE)
            .open(&temporary)?;
        // The mode above applies only to a file the open creates.
        out.set_permissions(Permissions::from_mode(FILE_MODE))?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        std::fs::rename(&temporary, file)?;
        if let Some(folder) = file.parent() {
            // The rename is durable once the folder is synced.
            File::open(folder)?.sync_all()?;
        }
        Ok(())
    }
}

fn temporary_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().map(OsString::from).unwrap_or_default();
    name.push(TEMPORARY_SUFFIX);
    file.with_file_name(name)
}

/// Writes the state's preferences to `file` each time they change, one write at a time, off the
/// window's thread.
pub fn use_prefs_saver(cockpit: Cockpit, file: Option<PathBuf>) {
    let prefs = use_memo(move || cockpit.state.read().prefs.clone());
    let saver = use_coroutine(move |mut changes: UnboundedReceiver<Prefs>| {
        let file = file.clone();
        async move {
            use futures_util::StreamExt;

            let Some(file) = file else {
                return;
            };
            let mut saved = prefs.peek().clone();
            while let Some(changed) = changes.next().await {
                if changed == saved {
                    continue;
                }
                let target = file.clone();
                let writing = changed.clone();
                match tokio::task::spawn_blocking(move || writing.save(&target)).await {
                    Ok(Ok(())) => saved = changed,
                    Ok(Err(error)) => {
                        tracing::warn!("cannot save {}: {error}", file.display());
                    }
                    Err(error) => tracing::warn!("the preferences were not saved: {error}"),
                }
            }
        }
    });
    use_effect(move || saver.send(prefs()));
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    /// Other programs and other versions of the app read this file: its field names are pinned.
    #[test]
    fn reads_a_hand_written_file() {
        let text = r#"{
  "file_view": {
    "width_share": 0.42,
    "wrap": true
  }
}
"#;

        let prefs: Prefs = serde_json::from_str(text).unwrap();

        assert_eq!(
            prefs,
            Prefs {
                file_view: FileViewPrefs {
                    width_share: Some(0.42),
                    wrap: true,
                },
            }
        );
        assert_eq!(serde_json::to_string_pretty(&prefs).unwrap() + "\n", text);
    }

    #[test]
    fn missing_fields_take_their_default_and_unknown_ones_are_ignored() {
        let prefs: Prefs =
            serde_json::from_str(r#"{"file_view": {"wrap": true}, "theme": "light"}"#).unwrap();

        assert_eq!(prefs.file_view.width_share, None);
        assert!(prefs.file_view.wrap);
    }

    #[test]
    fn a_missing_or_broken_file_gives_the_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let broken = temp.path().join(PREFS_FILE);
        std::fs::write(&broken, "{not json").unwrap();

        assert_eq!(
            Prefs::load(&temp.path().join("missing.json")),
            Prefs::default()
        );
        assert_eq!(Prefs::load(&broken), Prefs::default());
    }

    #[test]
    fn a_share_out_of_range_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(PREFS_FILE);
        std::fs::write(
            &file,
            r#"{"file_view": {"width_share": 1.5, "wrap": true}}"#,
        )
        .unwrap();

        let prefs = Prefs::load(&file);

        assert_eq!(prefs.file_view.width_share, None);
        assert!(prefs.file_view.wrap);
    }

    #[test]
    fn saves_privately_and_reads_back() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);
        let file = folder.join(PREFS_FILE);
        let prefs = Prefs {
            file_view: FileViewPrefs {
                width_share: Some(0.3),
                wrap: true,
            },
        };

        prefs.save(&file).unwrap();
        prefs.save(&file).unwrap();

        assert_eq!(Prefs::load(&file), prefs);
        let mode = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o777;
        assert_eq!(mode(&folder), FOLDER_MODE);
        assert_eq!(mode(&file), FILE_MODE);
        let names: Vec<OsString> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from(PREFS_FILE)]);
    }

    /// The mode given at creation does not reach a file that already exists, such as a temporary
    /// file a crash left behind.
    #[test]
    fn a_leftover_temporary_file_does_not_widen_the_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(PREFS_FILE);
        let leftover = temporary_path(&file);
        std::fs::write(&leftover, "{").unwrap();
        std::fs::set_permissions(&leftover, Permissions::from_mode(0o644)).unwrap();

        Prefs::default().save(&file).unwrap();

        let mode = std::fs::metadata(&file).unwrap().mode() & 0o777;
        assert_eq!(mode, FILE_MODE);
        assert_eq!(Prefs::load(&file), Prefs::default());
    }

    #[test]
    fn the_file_is_apart_from_the_daemons_folder() {
        let environment = Environment {
            home: Some(PathBuf::from("/home/someone")),
            config_home: None,
            state_home: None,
        };

        assert_eq!(
            file(&environment),
            Some(PathBuf::from(
                "/home/someone/.local/state/agents-cockpit-desktop/prefs.json"
            ))
        );
        assert_eq!(file(&Environment::default()), None);
    }
}

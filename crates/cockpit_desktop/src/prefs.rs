//! The app's own preferences, `$XDG_STATE_HOME/agents-cockpit-desktop/prefs.json`: choices made in
//! the window that last across restarts.
//!
//! ```json
//! {
//!   "file_view": {
//!     "width_share": 0.42,
//!     "wrap": true
//!   },
//!   "sidebar": {
//!     "filter": "needs_you"
//!   },
//!   "accounts": {
//!     "colors": {
//!       "build-box": { "1": "violet" }
//!     }
//!   }
//! }
//! ```
//!
//! The daemon keeps no color for an account, so each viewer chooses them here, per daemon machine
//! and account id.
//!
//! The file is on the machine the app runs on, in a folder apart from the daemon's, which holds a
//! local daemon's socket. A missing or unreadable file means the defaults, and a field the app
//! does not know is ignored.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use cockpit_protocol::{AccountId, AgentStatus, AgentSummary, Mark};
use dioxus::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::{EnumString, IntoStaticStr, VariantArray};

use crate::config::Environment;
use crate::connection::Cockpit;

pub const PREFS_FOLDER: &str = "agents-cockpit-desktop";
const PREFS_FILE: &str = "prefs.json";
const TEMPORARY_SUFFIX: &str = ".tmp";

/// The same modes as the daemon's files: only their owner reads them.
#[cfg(unix)]
const FOLDER_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub file_view: FileViewPrefs,
    pub sidebar: SidebarPrefs,
    pub accounts: AccountPrefs,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountPrefs {
    /// The colors chosen, by the daemon's machine as the app names it, then by account id.
    pub colors: BTreeMap<String, BTreeMap<AccountId, AccountColor>>,
}

impl AccountPrefs {
    /// The account's chosen color, else the one its id gives.
    pub fn color(&self, machine: &str, account: AccountId) -> AccountColor {
        self.colors
            .get(machine)
            .and_then(|colors| colors.get(&account))
            .copied()
            .unwrap_or_else(|| AccountColor::for_id(account))
    }

    pub fn set_color(&mut self, machine: &str, account: AccountId, color: AccountColor) {
        self.colors
            .entry(machine.to_owned())
            .or_default()
            .insert(account, color);
    }
}

/// The colors an account can take: none is a status color (teal, amber, red, the greys).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum AccountColor {
    Blue,
    Violet,
    Pink,
    Sand,
    Slate,
    Green,
}

impl AccountColor {
    /// An account's color until one is chosen: the default account blue, the next violet, and
    /// so on.
    pub fn for_id(account: AccountId) -> Self {
        let count = u32::try_from(Self::VARIANTS.len()).unwrap_or(1);
        let index = usize::try_from(account.get() % count).unwrap_or(0);
        Self::VARIANTS.get(index).copied().unwrap_or(Self::Blue)
    }

    pub fn css(self) -> &'static str {
        match self {
            Self::Blue => "#6ea8fe",
            Self::Violet => "#b197fc",
            Self::Pink => "#f08cc0",
            Self::Sand => "#d4b483",
            Self::Slate => "#9fb0c8",
            Self::Green => "#8fd3a8",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Violet => "Violet",
            Self::Pink => "Pink",
            Self::Sand => "Sand",
            Self::Slate => "Slate",
            Self::Green => "Green",
        }
    }
}

impl Serialize for AccountColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as blue rather than
/// costing the whole file.
impl<'de> Deserialize<'de> for AccountColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or(Self::Blue))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarPrefs {
    /// The agents the sidebar shows, by status. It hides rows and never changes their order.
    pub filter: StatusFilter,
}

/// The sidebar's filter, in the order of its chips.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum StatusFilter {
    #[default]
    All,
    NeedsYou,
    Working,
    Idle,
    Exited,
    /// The agents marked to handle, whatever their status.
    ToHandle,
}

impl StatusFilter {
    /// The status the filter keeps, or `None` for all of them and for the marked ones.
    pub fn status(self) -> Option<AgentStatus> {
        match self {
            Self::All | Self::ToHandle => None,
            Self::NeedsYou => Some(AgentStatus::NeedsYou),
            Self::Working => Some(AgentStatus::Working),
            Self::Idle => Some(AgentStatus::Idle),
            Self::Exited => Some(AgentStatus::Exited),
        }
    }

    pub fn admits(self, summary: &AgentSummary) -> bool {
        match self {
            Self::ToHandle => summary.mark == Mark::ToHandle,
            _ => self.status().is_none_or(|kept| kept == summary.status),
        }
    }
}

impl Serialize for StatusFilter {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as `All` rather
/// than costing the whole file.
impl<'de> Deserialize<'de> for StatusFilter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
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

        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        if let Some(folder) = file.parent() {
            create_private_folder(folder)?;
        }
        let temporary = temporary_path(file);
        let mut out = create_private_file(&temporary)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        std::fs::rename(&temporary, file)?;
        // A folder opens as a file on Unix only.
        #[cfg(unix)]
        if let Some(folder) = file.parent() {
            // The rename is durable once the folder is synced.
            File::open(folder)?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn create_private_folder(folder: &Path) -> io::Result<()> {
    use std::fs::{DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    DirBuilder::new()
        .recursive(true)
        .mode(FOLDER_MODE)
        .create(folder)?;
    // An existing folder keeps its mode otherwise.
    std::fs::set_permissions(folder, Permissions::from_mode(FOLDER_MODE))
}

/// The folder takes the permissions of the one it is in: under `%LOCALAPPDATA%`, the user's own.
#[cfg(windows)]
fn create_private_folder(folder: &Path) -> io::Result<()> {
    std::fs::create_dir_all(folder)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::fs::Permissions;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(FILE_MODE)
        .open(path)?;
    // The mode above applies only to a file the open creates.
    file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    Ok(file)
}

/// The file takes the permissions of its folder.
#[cfg(windows)]
fn create_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
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
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    use super::*;

    /// Other programs and other versions of the app read this file: its field names are pinned.
    #[test]
    fn reads_a_hand_written_file() {
        let text = r#"{
  "file_view": {
    "width_share": 0.42,
    "wrap": true
  },
  "sidebar": {
    "filter": "needs_you"
  },
  "accounts": {
    "colors": {
      "build-box": {
        "0": "sand",
        "1": "violet"
      }
    }
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
                sidebar: SidebarPrefs {
                    filter: StatusFilter::NeedsYou,
                },
                accounts: AccountPrefs {
                    colors: BTreeMap::from([(
                        "build-box".to_owned(),
                        BTreeMap::from([
                            (AccountId::DEFAULT, AccountColor::Sand),
                            (AccountId::new(1), AccountColor::Violet),
                        ]),
                    )]),
                },
            }
        );
        assert_eq!(serde_json::to_string_pretty(&prefs).unwrap() + "\n", text);
    }

    #[test]
    fn an_account_takes_its_chosen_color_else_the_one_its_id_gives() {
        let mut prefs = AccountPrefs::default();
        prefs.set_color("build-box", AccountId::new(1), AccountColor::Green);
        let words: Vec<&str> = AccountColor::VARIANTS
            .iter()
            .map(|&color| color.into())
            .collect();
        let unknown: AccountColor = serde_json::from_str(r#""teal""#).unwrap();

        assert_eq!(
            prefs.color("build-box", AccountId::DEFAULT),
            AccountColor::Blue
        );
        assert_eq!(
            prefs.color("build-box", AccountId::new(1)),
            AccountColor::Green
        );
        assert_eq!(
            prefs.color("this machine", AccountId::new(1)),
            AccountColor::Violet
        );
        assert_eq!(
            prefs.color("build-box", AccountId::new(8)),
            AccountColor::Pink
        );
        assert_eq!(words, ["blue", "violet", "pink", "sand", "slate", "green"]);
        assert_eq!(unknown, AccountColor::Blue);
    }

    #[test]
    fn missing_fields_take_their_default_and_unknown_ones_are_ignored() {
        let prefs: Prefs =
            serde_json::from_str(r#"{"file_view": {"wrap": true}, "theme": "light"}"#).unwrap();

        assert_eq!(prefs.file_view.width_share, None);
        assert!(prefs.file_view.wrap);
        assert_eq!(prefs.sidebar.filter, StatusFilter::All);
    }

    #[test]
    fn the_filter_words_are_pinned_and_an_unknown_one_reads_as_all() {
        let words: Vec<String> = StatusFilter::VARIANTS
            .iter()
            .map(|filter| {
                serde_json::to_value(filter)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let unknown: Prefs = serde_json::from_str(
            r#"{"file_view": {"width_share": 0.3}, "sidebar": {"filter": "paused"}}"#,
        )
        .unwrap();

        assert_eq!(
            words,
            ["all", "needs_you", "working", "idle", "exited", "to_handle"]
        );
        assert_eq!(unknown.sidebar.filter, StatusFilter::All);
        assert_eq!(unknown.file_view.width_share, Some(0.3));
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

    #[cfg(unix)]
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
            sidebar: SidebarPrefs {
                filter: StatusFilter::Exited,
            },
            accounts: AccountPrefs::default(),
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

    /// Windows renames over an existing file too.
    #[cfg(windows)]
    #[test]
    fn saves_over_the_last_save_and_reads_back() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);
        let file = folder.join(PREFS_FILE);
        let prefs = Prefs {
            file_view: FileViewPrefs {
                width_share: Some(0.3),
                wrap: true,
            },
            sidebar: SidebarPrefs {
                filter: StatusFilter::Exited,
            },
            accounts: AccountPrefs::default(),
        };

        Prefs::default().save(&file).unwrap();
        prefs.save(&file).unwrap();

        assert_eq!(Prefs::load(&file), prefs);
        let names: Vec<OsString> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from(PREFS_FILE)]);
    }

    /// The mode given at creation does not reach a file that already exists, such as a temporary
    /// file a crash left behind.
    #[cfg(unix)]
    #[test]
    fn a_leftover_temporary_file_does_not_widen_the_mode() {
        use std::fs::Permissions;
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
            home: Some(PathBuf::from(crate::config::tests::HOME)),
            config_home: None,
            state_home: None,
        };
        #[cfg(unix)]
        let expected = "/home/someone/.local/state/agents-cockpit-desktop/prefs.json";
        #[cfg(windows)]
        let expected = r"C:\Users\someone\AppData\Local\agents-cockpit-desktop\prefs.json";

        assert_eq!(file(&environment), Some(PathBuf::from(expected)));
        assert_eq!(file(&Environment::default()), None);
    }
}

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
//!     "filter": "needs_you",
//!     "width": 300
//!   },
//!   "files_pane": {
//!     "new_agents": "closed",
//!     "agents": {
//!       "build-box": { "4": "open" }
//!     },
//!     "width": 280
//!   },
//!   "blocks": {
//!     "order": ["running", "tasks", "mcp", "messages"]
//!   },
//!   "accounts": {
//!     "colors": {
//!       "build-box": { "1": "violet" }
//!     }
//!   },
//!   "notifications": {
//!     "app": "all",
//!     "agents": {
//!       "build-box": { "4": "off", "7": "needs_you" }
//!     }
//!   },
//!   "chat": {
//!     "comments": "hidden",
//!     "last_message": "hidden",
//!     "steer_hold": "three_seconds",
//!     "esc_stop": "at_once"
//!   }
//! }
//! ```
//!
//! The daemon keeps no color for an account, so each viewer chooses them here, per daemon machine
//! and account id. Notifications are this desktop's too: per daemon machine and the daemon's
//! agent id.
//!
//! The file is on the machine the app runs on, in a folder apart from the daemon's, which holds a
//! local daemon's socket. A missing or unreadable file means the defaults, and a field the app
//! does not know is ignored.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cockpit_protocol::{AccountId, AgentId, AgentStatus, AgentSummary, Mark};
use dioxus::prelude::*;
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::{EnumString, IntoStaticStr, VariantArray};

use crate::config::Environment;
use crate::connection::Cockpit;
use crate::keymap::Keymap;
use crate::state::Block;

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
    pub files_pane: FilesPanePrefs,
    pub blocks: BlocksPrefs,
    pub accounts: AccountPrefs,
    pub notifications: NotificationPrefs,
    pub chat: ChatPrefs,
    /// The shortcuts changed in the Settings dialog.
    pub keys: Keymap,
    /// The sidebar's order when it groups by project or not at all, top first. Grouped by
    /// machine, each daemon keeps its own order instead. An agent missing from it is newer, and
    /// shows above the others.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub agent_order: Vec<AgentKey>,
    #[serde(skip_serializing_if = "ProjectsPrefs::is_empty")]
    pub projects: ProjectsPrefs,
}

/// The projects the user kept, and the agents moved to a project by hand. A repository no kept
/// project claims makes a temporary project, which the app computes and never saves.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectsPrefs {
    /// In the sidebar's order.
    pub list: Vec<Project>,
    /// The id the next project takes: ids are never taken again, so a deleted project's moved
    /// agents never land in a new one.
    pub next_id: u32,
    /// By daemon machine, then by the daemon's agent id: the project an agent was moved to, or
    /// `None` for "No project". It wins over the rules.
    pub agents: BTreeMap<String, BTreeMap<AgentId, Option<ProjectId>>>,
}

impl ProjectsPrefs {
    fn is_empty(&self) -> bool {
        self.list.is_empty() && self.agents.is_empty() && self.next_id == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(u32);

impl ProjectId {
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub color: AccountColor,
    /// An agent belongs to the first project with a rule that holds its folder.
    #[serde(default, deserialize_with = "known_rules")]
    pub rules: Vec<ProjectRule>,
}

/// The rules this version knows: a kind from another version of the app is left out, rather
/// than costing the whole file.
fn known_rules<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ProjectRule>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Read {
        Known(ProjectRule),
        Unknown(IgnoredAny),
    }
    let rules = Vec::<Read>::deserialize(deserializer)?;
    Ok(rules
        .into_iter()
        .filter_map(|rule| match rule {
            Read::Known(rule) => Some(rule),
            Read::Unknown(_) => None,
        })
        .collect())
}

/// What a project holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRule {
    /// A git repository by its remote, as `normalized_repository` writes it: every clone and
    /// worktree of it, on every machine.
    Repository(String),
    /// A folder and the folders inside it, by its absolute path. A path that ends with `*` holds
    /// every folder whose path starts with the rest.
    Folder(String),
}

/// An agent as the preferences name it: its daemon machine's name and the daemon's agent id,
/// which last across the app's restarts, unlike the app's own ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentKey {
    pub machine: String,
    pub agent: AgentId,
}

/// What the sidebar's panels group: each machine's agents, or one list.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum GroupBy {
    #[default]
    Machine,
    Project,
    None,
}

impl GroupBy {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Serialize for GroupBy {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know reads as `Machine`, rather than costing the whole file.
impl<'de> Deserialize<'de> for GroupBy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatPrefs {
    /// Where a new comment on a quote shows while it waits to be sent.
    pub comments: CommentPlacement,
    /// The bubble over the chat with the user's last message, from the eye in the Messages block.
    pub last_message: LastMessage,
    /// How long a message steered to a busy agent waits in the app, where Cancel can still stop it.
    pub steer_hold: SteerHold,
    /// What Esc does to a busy agent once nothing is left open.
    pub esc_stop: EscStop,
}

/// What Esc does to the selected agent while it works or waits for you, once nothing is left open
/// for it to close.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum EscStop {
    /// A dialog asks before the agent stops.
    #[default]
    Ask,
    /// The agent stops at once.
    AtOnce,
}

impl Serialize for EscStop {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as the default.
impl<'de> Deserialize<'de> for EscStop {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
}

/// How long the app holds a message steered to a busy agent before it sends it: once sent, Claude
/// Code holds it, and nothing takes it back.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum SteerHold {
    /// The message leaves at once.
    #[default]
    Off,
    ThreeSeconds,
    FiveSeconds,
    TenSeconds,
}

impl SteerHold {
    pub fn duration(self) -> Duration {
        let seconds = match self {
            Self::Off => 0,
            Self::ThreeSeconds => 3,
            Self::FiveSeconds => 5,
            Self::TenSeconds => 10,
        };
        Duration::from_secs(seconds)
    }
}

impl Serialize for SteerHold {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as the default.
impl<'de> Deserialize<'de> for SteerHold {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
}

/// Whether the user's last message shows over the chat once it scrolled out above.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum LastMessage {
    #[default]
    Shown,
    Hidden,
}

impl LastMessage {
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::Shown => Self::Hidden,
            Self::Hidden => Self::Shown,
        }
    }
}

impl Serialize for LastMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as the default.
impl<'de> Deserialize<'de> for LastMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
}

/// Where a comment on a quote shows in the chat, from the Settings dialog.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum CommentPlacement {
    /// In a note beside its passage, while the chat has room for one.
    #[default]
    Beside,
    /// As its quote's number alone: hovering the number shows it.
    Hidden,
}

impl Serialize for CommentPlacement {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as the default.
impl<'de> Deserialize<'de> for CommentPlacement {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPrefs {
    /// The app's level. `None` until a choice in the window: the config file's
    /// `[notifications]` gives it until then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<NotificationLevel>,
    /// The agents whose level is not `All`, by daemon machine, then by the daemon's agent id.
    pub agents: BTreeMap<String, BTreeMap<AgentId, NotificationLevel>>,
}

impl NotificationPrefs {
    pub fn agent(&self, machine: &str, agent: AgentId) -> NotificationLevel {
        self.agents
            .get(machine)
            .and_then(|levels| levels.get(&agent))
            .copied()
            .unwrap_or_default()
    }

    /// Sets the agent's level. `All` removes its entry, and the machine's once it is empty.
    pub fn set_agent(&mut self, machine: &str, agent: AgentId, level: NotificationLevel) {
        if level != NotificationLevel::All {
            self.agents
                .entry(machine.to_owned())
                .or_default()
                .insert(agent, level);
            return;
        }
        if let Some(levels) = self.agents.get_mut(machine) {
            levels.remove(&agent);
            if levels.is_empty() {
                self.agents.remove(machine);
            }
        }
    }
}

/// Which notifications an agent, or the whole app, raises.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum NotificationLevel {
    #[default]
    All,
    /// Only when the agent needs you, not when it finishes.
    NeedsYou,
    Off,
}

impl Serialize for NotificationLevel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know reads as `All`: a lost preference is better than a lost
/// file.
impl<'de> Deserialize<'de> for NotificationLevel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
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
    /// The sidebar's width in pixels, between `MIN_SIDEBAR_WIDTH` and `MAX_SIDEBAR_WIDTH`.
    /// `None` keeps the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u16>,
    #[serde(skip_serializing_if = "GroupBy::is_default")]
    pub group_by: GroupBy,
}

/// The sidebar's width, in pixels, until it is moved.
pub const DEFAULT_SIDEBAR_WIDTH: u16 = 248;
/// The narrowest sidebar that still shows a row's title, time and status.
pub const MIN_SIDEBAR_WIDTH: u16 = 200;
pub const MAX_SIDEBAR_WIDTH: u16 = 480;

impl SidebarPrefs {
    /// The sidebar's width in pixels.
    pub fn width(&self) -> u16 {
        self.width.unwrap_or(DEFAULT_SIDEBAR_WIDTH)
    }
}

/// The floating blocks at the chat's top right.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BlocksPrefs {
    /// Their order, top to bottom, by their words. Empty keeps the default order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

/// The blocks' order until the user moves one: the messages sent come last.
pub const DEFAULT_BLOCK_ORDER: [Block; 4] =
    [Block::Tasks, Block::Running, Block::Mcp, Block::Messages];

impl BlocksPrefs {
    /// Every block once, top to bottom. A word from another version of the app is skipped, and a
    /// block the saved order lacks takes its default place after the others.
    pub fn order(&self) -> Vec<Block> {
        let mut order: Vec<Block> = Vec::with_capacity(DEFAULT_BLOCK_ORDER.len());
        let saved = self
            .order
            .iter()
            .filter_map(|word| word.parse::<Block>().ok());
        for block in saved.chain(DEFAULT_BLOCK_ORDER) {
            if !order.contains(&block) {
                order.push(block);
            }
        }
        order
    }

    pub fn set_order(&mut self, order: &[Block]) {
        self.order = order
            .iter()
            .map(|block| <&str>::from(*block).to_owned())
            .collect();
    }
}

/// The files pane: the tree of the selected agent's folder, at the window's right edge. Each
/// agent keeps the pane open or closed; one width serves them all.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesPanePrefs {
    /// The pane of an agent that has none in `agents`, from the Settings dialog.
    pub new_agents: PaneState,
    /// Each agent's pane once opened or closed, by daemon machine, then by the daemon's agent id.
    pub agents: BTreeMap<String, BTreeMap<AgentId, PaneState>>,
    /// The pane's width in pixels, between `MIN_FILES_WIDTH` and `MAX_FILES_WIDTH`. `None` keeps
    /// the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u16>,
}

/// Whether an agent's files pane shows.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum PaneState {
    Open,
    #[default]
    Closed,
}

impl PaneState {
    pub fn toggled(self) -> Self {
        match self {
            Self::Open => Self::Closed,
            Self::Closed => Self::Open,
        }
    }
}

impl Serialize for PaneState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.into())
    }
}

/// A word this version does not know, from another version of the app, reads as the default.
impl<'de> Deserialize<'de> for PaneState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let word = String::deserialize(deserializer)?;
        Ok(word.parse().unwrap_or_default())
    }
}

/// The files pane's width, in pixels, until it is moved.
pub const DEFAULT_FILES_WIDTH: u16 = 260;
/// The narrowest pane that still shows a name beside its marks.
pub const MIN_FILES_WIDTH: u16 = 180;
pub const MAX_FILES_WIDTH: u16 = 520;

impl FilesPanePrefs {
    /// The pane's width in pixels.
    pub fn width(&self) -> u16 {
        self.width.unwrap_or(DEFAULT_FILES_WIDTH)
    }

    pub fn agent(&self, machine: &str, agent: AgentId) -> PaneState {
        self.agents
            .get(machine)
            .and_then(|panes| panes.get(&agent))
            .copied()
            .unwrap_or(self.new_agents)
    }

    pub fn set_agent(&mut self, machine: &str, agent: AgentId, pane: PaneState) {
        self.agents
            .entry(machine.to_owned())
            .or_default()
            .insert(agent, pane);
    }

    pub fn forget_agent(&mut self, machine: &str, agent: AgentId) {
        self.keep_agents(machine, |kept| kept != agent);
    }

    /// Keeps the panes of the machine's agents that `keep` names.
    pub fn keep_agents(&mut self, machine: &str, keep: impl Fn(AgentId) -> bool) {
        if let Some(panes) = self.agents.get_mut(machine) {
            panes.retain(|&agent, _| keep(agent));
            if panes.is_empty() {
                self.agents.remove(machine);
            }
        }
    }

    pub fn forget_machine(&mut self, machine: &str) {
        self.agents.remove(machine);
    }
}

/// Whether `width` can size the files pane.
pub fn is_files_width(width: u16) -> bool {
    (MIN_FILES_WIDTH..=MAX_FILES_WIDTH).contains(&width)
}

/// Whether `width` can size the sidebar.
pub fn is_sidebar_width(width: u16) -> bool {
    (MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH).contains(&width)
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
        self.sidebar.width = self.sidebar.width.filter(|&width| is_sidebar_width(width));
        self.files_pane.width = self.files_pane.width.filter(|&width| is_files_width(width));
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
    use crate::keymap::Command;

    /// Other programs and other versions of the app read this file: its field names are pinned.
    #[test]
    fn reads_a_hand_written_file() {
        let text = r#"{
  "file_view": {
    "width_share": 0.42,
    "wrap": true
  },
  "sidebar": {
    "filter": "needs_you",
    "width": 300,
    "group_by": "project"
  },
  "files_pane": {
    "new_agents": "open",
    "agents": {
      "build-box": {
        "4": "closed"
      }
    },
    "width": 280
  },
  "blocks": {
    "order": [
      "running",
      "tasks",
      "mcp",
      "messages"
    ]
  },
  "accounts": {
    "colors": {
      "build-box": {
        "0": "sand",
        "1": "violet"
      }
    }
  },
  "notifications": {
    "app": "needs_you",
    "agents": {
      "build-box": {
        "4": "off"
      }
    }
  },
  "chat": {
    "comments": "hidden",
    "last_message": "hidden",
    "steer_hold": "three_seconds",
    "esc_stop": "at_once"
  },
  "keys": {
    "switcher": "Ctrl+j"
  },
  "agent_order": [
    {
      "machine": "build-box",
      "agent": 4
    }
  ],
  "projects": {
    "list": [
      {
        "id": 0,
        "name": "billing",
        "color": "violet",
        "rules": [
          {
            "repository": "github.com/acme/billing"
          },
          {
            "folder": "/srv/billing-*"
          }
        ]
      }
    ],
    "next_id": 1,
    "agents": {
      "build-box": {
        "4": 0,
        "5": null
      }
    }
  }
}
"#;

        let prefs: Prefs = serde_json::from_str(text).unwrap();
        let mut switched = Keymap::default();
        switched
            .set(Command::Switcher, "Ctrl+j".parse().unwrap())
            .unwrap();

        assert_eq!(
            prefs,
            Prefs {
                file_view: FileViewPrefs {
                    width_share: Some(0.42),
                    wrap: true,
                },
                sidebar: SidebarPrefs {
                    filter: StatusFilter::NeedsYou,
                    width: Some(300),
                    group_by: GroupBy::Project,
                },
                files_pane: FilesPanePrefs {
                    new_agents: PaneState::Open,
                    agents: BTreeMap::from([(
                        "build-box".to_owned(),
                        BTreeMap::from([(AgentId::new(4), PaneState::Closed)]),
                    )]),
                    width: Some(280),
                },
                blocks: BlocksPrefs {
                    order: ["running", "tasks", "mcp", "messages"]
                        .map(str::to_owned)
                        .to_vec(),
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
                notifications: NotificationPrefs {
                    app: Some(NotificationLevel::NeedsYou),
                    agents: BTreeMap::from([(
                        "build-box".to_owned(),
                        BTreeMap::from([(AgentId::new(4), NotificationLevel::Off)]),
                    )]),
                },
                chat: ChatPrefs {
                    comments: CommentPlacement::Hidden,
                    last_message: LastMessage::Hidden,
                    steer_hold: SteerHold::ThreeSeconds,
                    esc_stop: EscStop::AtOnce,
                },
                keys: switched,
                agent_order: vec![AgentKey {
                    machine: "build-box".to_owned(),
                    agent: AgentId::new(4),
                }],
                projects: ProjectsPrefs {
                    list: vec![Project {
                        id: ProjectId::new(0),
                        name: "billing".to_owned(),
                        color: AccountColor::Violet,
                        rules: vec![
                            ProjectRule::Repository("github.com/acme/billing".to_owned()),
                            ProjectRule::Folder("/srv/billing-*".to_owned()),
                        ],
                    }],
                    next_id: 1,
                    agents: BTreeMap::from([(
                        "build-box".to_owned(),
                        BTreeMap::from([
                            (AgentId::new(4), Some(ProjectId::new(0))),
                            (AgentId::new(5), None),
                        ]),
                    )]),
                },
            }
        );
        assert_eq!(serde_json::to_string_pretty(&prefs).unwrap() + "\n", text);
    }

    #[test]
    fn an_agent_keeps_its_pane_and_the_others_take_the_new_agents_one() {
        let mut prefs = FilesPanePrefs::default();
        prefs.set_agent("build-box", AgentId::new(4), PaneState::Open);
        let set = prefs.agent("build-box", AgentId::new(4));
        let other = prefs.agent("build-box", AgentId::new(5));
        prefs.forget_agent("build-box", AgentId::new(4));
        let old_switch: FilesPanePrefs =
            serde_json::from_str(r#"{"open": true, "width": 300}"#).unwrap();

        assert_eq!((set, other), (PaneState::Open, PaneState::Closed));
        assert!(prefs.agents.is_empty());
        assert_eq!(old_switch.new_agents, PaneState::Closed);
        assert_eq!(old_switch.width, Some(300));
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
    fn a_rule_of_an_unknown_kind_is_left_out_and_the_file_still_reads() {
        let prefs: Prefs = serde_json::from_str(
            r#"{
                "chat": {"comments": "hidden"},
                "projects": {"list": [{
                    "id": 0,
                    "name": "billing",
                    "color": "violet",
                    "rules": [{"branch": "main"}, {"folder": "/work/billing"}]
                }]}
            }"#,
        )
        .unwrap();

        assert_eq!(prefs.chat.comments, CommentPlacement::Hidden);
        assert_eq!(
            prefs.projects.list[0].rules,
            [ProjectRule::Folder("/work/billing".to_owned())]
        );
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
    fn the_comment_placements_are_pinned_and_an_unknown_one_reads_as_beside() {
        let words: Vec<&str> = CommentPlacement::VARIANTS
            .iter()
            .map(|&placement| placement.into())
            .collect();
        let hidden: Prefs = serde_json::from_str(r#"{"chat": {"comments": "hidden"}}"#).unwrap();
        let unknown: Prefs =
            serde_json::from_str(r#"{"chat": {"comments": "folded"}, "sidebar": {"width": 300}}"#)
                .unwrap();

        assert_eq!(words, ["beside", "hidden"]);
        assert_eq!(hidden.chat.comments, CommentPlacement::Hidden);
        assert_eq!(unknown.chat.comments, CommentPlacement::Beside);
        assert_eq!(unknown.sidebar.width, Some(300));
    }

    #[test]
    fn the_last_message_words_are_pinned_and_it_shows_by_default() {
        let words: Vec<&str> = LastMessage::VARIANTS
            .iter()
            .map(|&shown| shown.into())
            .collect();
        let hidden: Prefs =
            serde_json::from_str(r#"{"chat": {"last_message": "hidden"}}"#).unwrap();
        let unknown: Prefs =
            serde_json::from_str(r#"{"chat": {"last_message": "faded"}}"#).unwrap();

        assert_eq!(words, ["shown", "hidden"]);
        assert_eq!(hidden.chat.last_message, LastMessage::Hidden);
        assert_eq!(unknown.chat.last_message, LastMessage::Shown);
        assert_eq!(Prefs::default().chat.last_message, LastMessage::Shown);
        assert_eq!(LastMessage::Shown.toggled(), LastMessage::Hidden);
    }

    #[test]
    fn the_esc_stop_words_are_pinned_and_esc_asks_by_default() {
        let words: Vec<&str> = EscStop::VARIANTS
            .iter()
            .map(|&choice| choice.into())
            .collect();
        let at_once: Prefs = serde_json::from_str(r#"{"chat": {"esc_stop": "at_once"}}"#).unwrap();
        let older: Prefs = serde_json::from_str(r#"{"chat": {"comments": "hidden"}}"#).unwrap();
        let unknown: Prefs = serde_json::from_str(r#"{"chat": {"esc_stop": "never"}}"#).unwrap();

        assert_eq!(words, ["ask", "at_once"]);
        assert_eq!(at_once.chat.esc_stop, EscStop::AtOnce);
        assert_eq!(older.chat.esc_stop, EscStop::Ask);
        assert_eq!(unknown.chat.esc_stop, EscStop::Ask);
    }

    #[test]
    fn the_group_by_words_are_pinned_and_an_unknown_one_reads_as_machine() {
        let words: Vec<&str> = GroupBy::VARIANTS
            .iter()
            .map(|&group_by| group_by.into())
            .collect();
        let unknown: Prefs =
            serde_json::from_str(r#"{"sidebar": {"group_by": "folder"}}"#).unwrap();

        assert_eq!(words, ["machine", "project", "none"]);
        assert_eq!(unknown.sidebar.group_by, GroupBy::Machine);
    }

    #[test]
    fn the_notification_words_are_pinned_and_an_unknown_one_reads_as_all() {
        let words: Vec<&str> = NotificationLevel::VARIANTS
            .iter()
            .map(|&level| level.into())
            .collect();
        let prefs: Prefs = serde_json::from_str(
            r#"{"notifications": {"app": "off", "agents": {"build-box": {"4": "needs_you", "7": "muted"}}}}"#,
        )
        .unwrap();

        assert_eq!(words, ["all", "needs_you", "off"]);
        assert_eq!(prefs.notifications.app, Some(NotificationLevel::Off));
        assert_eq!(
            prefs.notifications.agent("build-box", AgentId::new(4)),
            NotificationLevel::NeedsYou
        );
        assert_eq!(
            prefs.notifications.agent("build-box", AgentId::new(7)),
            NotificationLevel::All
        );
    }

    #[test]
    fn an_agent_back_to_all_leaves_no_entry() {
        let mut notifications = NotificationPrefs::default();
        notifications.set_agent("build-box", AgentId::new(4), NotificationLevel::Off);
        notifications.set_agent("build-box", AgentId::new(4), NotificationLevel::All);

        assert_eq!(notifications, NotificationPrefs::default());
        let text = serde_json::to_string(&notifications).unwrap();
        assert_eq!(text, r#"{"agents":{}}"#);
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
    fn a_saved_block_order_skips_unknown_words_and_ends_with_the_missing_blocks() {
        let blocks = BlocksPrefs {
            order: ["mcp", "timeline", "mcp", "messages"]
                .map(str::to_owned)
                .to_vec(),
        };

        assert_eq!(
            blocks.order(),
            [Block::Mcp, Block::Messages, Block::Tasks, Block::Running]
        );
    }

    #[test]
    fn a_width_out_of_range_is_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join(PREFS_FILE);
        std::fs::write(
            &file,
            r#"{"file_view": {"width_share": 1.5, "wrap": true}, "sidebar": {"width": 2000}}"#,
        )
        .unwrap();

        let prefs = Prefs::load(&file);

        assert_eq!(prefs.file_view.width_share, None);
        assert!(prefs.file_view.wrap);
        assert_eq!(prefs.sidebar.width, None);
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
                width: Some(220),
                group_by: GroupBy::None,
            },
            files_pane: FilesPanePrefs {
                new_agents: PaneState::Open,
                agents: BTreeMap::new(),
                width: Some(300),
            },
            blocks: BlocksPrefs::default(),
            accounts: AccountPrefs::default(),
            notifications: NotificationPrefs::default(),
            chat: ChatPrefs::default(),
            keys: Keymap::default(),
            agent_order: Vec::new(),
            projects: ProjectsPrefs::default(),
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
                width: Some(220),
                group_by: GroupBy::None,
            },
            files_pane: FilesPanePrefs {
                new_agents: PaneState::Open,
                agents: BTreeMap::new(),
                width: Some(300),
            },
            ..Prefs::default()
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

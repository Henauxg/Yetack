//! What the window shows besides the daemon's data: the selected agent, open menus and dialogs,
//! drafts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use cockpit_protocol::{AgentId, Effort, Image, PendingId, PermissionMode};

/// How many error notes stay on screen; an older one goes when a new one comes.
const MAX_ERRORS: usize = 4;

/// The blocks at the top right of the chat. Each folds to one line, or becomes a badge when the
/// file view is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Block {
    Tasks,
    Running,
    Mcp,
}

/// A menu or card that closes when another opens, or on Esc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Popover {
    Usage,
    Changes,
    Model,
    Mode,
    Palette,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    Switcher(Switcher),
    NewAgent(NewAgentForm),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Switcher {
    pub query: String,
    /// An index into the filtered agents.
    pub highlighted: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NewAgentTab {
    #[default]
    Start,
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewAgentForm {
    pub tab: NewAgentTab,
    pub folder: String,
    /// `None` keeps Claude Code's default, as do `effort` and `mode`.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub mode: Option<PermissionMode>,
    pub prompt: String,
    /// A start or resume request is on its way.
    pub starting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Diff,
    WholeFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    /// Relative to the agent's folder when the file is inside it.
    pub path: PathBuf,
    /// The line a `path:line` link marks.
    pub line: Option<u32>,
    pub mode: ViewMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolFilter {
    #[default]
    All,
    ReadOnly,
    Destructive,
    Used,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpView {
    /// The server whose detail is open.
    pub detail: Option<String>,
    pub show_hidden: bool,
    pub filter: ToolFilter,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Draft {
    pub text: String,
    pub images: Vec<Image>,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }
}

/// The answers being chosen on a question card, one entry per asked question.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuestionDraft {
    /// The indexes of the picked options.
    pub picks: Vec<BTreeSet<usize>>,
    /// The "Other" fields.
    pub other: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    pub query: String,
    /// An index into the filtered commands.
    pub highlighted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorNote {
    pub id: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    pub selected: Option<AgentId>,
    /// Agents that finished while another one was selected: bold title and a white dot.
    pub unread: BTreeSet<AgentId>,
    pub window_focused: bool,
    /// The floating blocks shown unfolded.
    pub open_blocks: BTreeSet<Block>,
    /// The block shown as a popover from its badge, while the file view is open.
    pub badge: Option<Block>,
    pub popover: Option<Popover>,
    pub dialog: Option<Dialog>,
    pub file_views: BTreeMap<AgentId, FileView>,
    pub mcp: McpView,
    pub drafts: BTreeMap<AgentId, Draft>,
    pub questions: BTreeMap<PendingId, QuestionDraft>,
    pub palette: Palette,
    pub errors: Vec<ErrorNote>,
    next_error: u32,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            selected: None,
            unread: BTreeSet::new(),
            window_focused: true,
            open_blocks: BTreeSet::from([Block::Tasks, Block::Running]),
            badge: None,
            popover: None,
            dialog: None,
            file_views: BTreeMap::new(),
            mcp: McpView::default(),
            drafts: BTreeMap::new(),
            questions: BTreeMap::new(),
            palette: Palette::default(),
            errors: Vec::new(),
            next_error: 0,
        }
    }
}

impl UiState {
    /// Shows an error note until it is dismissed or pushed out by newer ones.
    pub fn report(&mut self, text: String) {
        self.errors.push(ErrorNote {
            id: self.next_error,
            text,
        });
        self.next_error = self.next_error.wrapping_add(1);
        let excess = self.errors.len().saturating_sub(MAX_ERRORS);
        self.errors.drain(..excess);
    }

    pub fn dismiss(&mut self, id: u32) {
        self.errors.retain(|note| note.id != id);
    }

    pub fn draft(&self, agent: AgentId) -> Option<&Draft> {
        self.drafts.get(&agent)
    }

    pub fn draft_mut(&mut self, agent: AgentId) -> &mut Draft {
        self.drafts.entry(agent).or_default()
    }

    pub fn file_view(&self, agent: AgentId) -> Option<&FileView> {
        self.file_views.get(&agent)
    }

    /// Closes every popover and the dialog. Returns whether anything was open.
    pub fn close_overlays(&mut self) -> bool {
        let open = self.popover.is_some() || self.dialog.is_some() || self.badge.is_some();
        self.popover = None;
        self.dialog = None;
        self.badge = None;
        open
    }

    pub fn toggle_popover(&mut self, popover: Popover) {
        self.popover = if self.popover == Some(popover) {
            None
        } else {
            Some(popover)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_notes_keep_the_newest_and_can_be_dismissed() {
        let mut ui = UiState::default();
        for index in 0..6 {
            ui.report(format!("error {index}"));
        }

        let texts: Vec<&str> = ui.errors.iter().map(|note| note.text.as_str()).collect();
        assert_eq!(texts, ["error 2", "error 3", "error 4", "error 5"]);

        let first = ui.errors.first().map(|note| note.id).unwrap();
        ui.dismiss(first);
        assert_eq!(ui.errors.len(), 3);
    }

    #[test]
    fn a_popover_toggles_and_replaces_another() {
        let mut ui = UiState::default();

        ui.toggle_popover(Popover::Usage);
        ui.toggle_popover(Popover::Changes);
        assert_eq!(ui.popover, Some(Popover::Changes));

        ui.toggle_popover(Popover::Changes);
        assert_eq!(ui.popover, None);
    }
}

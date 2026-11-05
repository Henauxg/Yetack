//! What the window shows besides the daemon's data: the selected agent, open menus and dialogs.
//! Drafts are apart, in `drafts`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use cockpit_protocol::{AgentId, Effort, ItemId, PermissionMode};

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
    Palette(PaletteOrigin),
}

/// How the command palette opened, which decides when it closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteOrigin {
    /// A `/` typed at the start of the composer. The palette closes when the composer's text no
    /// longer starts with `/`.
    Typed,
    /// The composer's slash button. The palette stays open until Esc, a pick, or a click
    /// elsewhere.
    Button,
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

/// A menu of the new agent dialog, apart from the composer's: the composer's would open behind
/// the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMenu {
    /// The model and the effort.
    Model,
    Mode,
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
    pub menu: Option<FormMenu>,
    /// What the "Resume a session" tab's search field holds.
    pub session_query: String,
    /// An index into the sessions the query leaves.
    pub session_highlighted: usize,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Error,
    /// What an action did, such as a copy to the clipboard.
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorNote {
    pub id: u32,
    pub text: String,
    pub kind: NoteKind,
}

/// An agent's title being edited. The field's text, which follows the keyboard, is in the drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleEdit {
    pub agent: AgentId,
    /// The title the field starts from.
    pub title: String,
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
    /// The agents whose background block lists every finished task, not only the latest.
    pub finished_tasks_unfolded: BTreeSet<AgentId>,
    pub popover: Option<Popover>,
    pub dialog: Option<Dialog>,
    pub file_views: BTreeMap<AgentId, FileView>,
    pub mcp: McpView,
    /// The agent title the header shows as a text field.
    pub title_edit: Option<TitleEdit>,
    /// The notes at the bottom left: errors, and what an action did.
    pub errors: Vec<ErrorNote>,
    next_error: u32,
    /// The agents whose chat is scrolled up, each with the last entry it held when it left its
    /// bottom (`None` when it held none). The entries after it arrived since.
    pub chat_scrolled_up: BTreeMap<AgentId, Option<ItemId>>,
    /// The first entry that arrived while the agent's chat was scrolled up, once the chat went
    /// back down: the "New" divider sits above it until a message is sent or another agent opens.
    pub chat_new_from: BTreeMap<AgentId, ItemId>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            selected: None,
            unread: BTreeSet::new(),
            window_focused: true,
            open_blocks: BTreeSet::from([Block::Tasks, Block::Running]),
            badge: None,
            finished_tasks_unfolded: BTreeSet::new(),
            popover: None,
            dialog: None,
            file_views: BTreeMap::new(),
            mcp: McpView::default(),
            title_edit: None,
            errors: Vec::new(),
            next_error: 0,
            chat_scrolled_up: BTreeMap::new(),
            chat_new_from: BTreeMap::new(),
        }
    }
}

impl UiState {
    /// Shows an error note until it is dismissed or pushed out by newer ones.
    pub fn report(&mut self, text: String) {
        self.note(text, NoteKind::Error);
    }

    /// Shows what an action did, in a note like an error's.
    pub fn inform(&mut self, text: String) {
        self.note(text, NoteKind::Info);
    }

    fn note(&mut self, text: String, kind: NoteKind) {
        self.errors.push(ErrorNote {
            id: self.next_error,
            text,
            kind,
        });
        self.next_error = self.next_error.wrapping_add(1);
        let excess = self.errors.len().saturating_sub(MAX_ERRORS);
        self.errors.drain(..excess);
    }

    pub fn dismiss(&mut self, id: u32) {
        self.errors.retain(|note| note.id != id);
    }

    pub fn file_view(&self, agent: AgentId) -> Option<&FileView> {
        self.file_views.get(&agent)
    }

    pub fn toggle_finished_tasks(&mut self, agent: AgentId) {
        if !self.finished_tasks_unfolded.remove(&agent) {
            self.finished_tasks_unfolded.insert(agent);
        }
    }

    /// Closes every popover and the dialog, or only the dialog's menu when one is open, and
    /// cancels a title edit. Returns whether anything was open.
    pub fn close_overlays(&mut self) -> bool {
        if let Some(Dialog::NewAgent(form)) = &mut self.dialog
            && form.menu.is_some()
        {
            form.menu = None;
            return true;
        }
        let open = self.popover.is_some()
            || self.dialog.is_some()
            || self.badge.is_some()
            || self.title_edit.is_some();
        self.popover = None;
        self.dialog = None;
        self.badge = None;
        self.title_edit = None;
        open
    }

    /// Whether a click outside the open popover closes it. The palette opened by typing `/` stays
    /// open while you type in the composer.
    pub fn popover_closes_on_click(&self) -> bool {
        self.popover
            .is_some_and(|popover| popover != Popover::Palette(PaletteOrigin::Typed))
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
    fn escape_closes_the_new_agent_menu_before_the_dialog() {
        let mut ui = UiState {
            dialog: Some(Dialog::NewAgent(NewAgentForm {
                menu: Some(FormMenu::Mode),
                ..NewAgentForm::default()
            })),
            ..UiState::default()
        };

        assert!(ui.close_overlays());
        assert!(matches!(&ui.dialog, Some(Dialog::NewAgent(form)) if form.menu.is_none()));

        assert!(ui.close_overlays());
        assert_eq!(ui.dialog, None);
    }

    #[test]
    fn the_finished_tasks_fold_per_agent() {
        let mut ui = UiState::default();
        let (first, second) = (AgentId::new(1), AgentId::new(2));

        ui.toggle_finished_tasks(first);
        assert!(ui.finished_tasks_unfolded.contains(&first));
        assert!(!ui.finished_tasks_unfolded.contains(&second));

        ui.toggle_finished_tasks(first);
        assert!(ui.finished_tasks_unfolded.is_empty());
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

    #[test]
    fn a_click_elsewhere_closes_the_palette_only_when_the_button_opened_it() {
        let closes = |popover| {
            let ui = UiState {
                popover,
                ..UiState::default()
            };
            ui.popover_closes_on_click()
        };

        assert!(closes(Some(Popover::Palette(PaletteOrigin::Button))));
        assert!(closes(Some(Popover::Model)));
        assert!(!closes(Some(Popover::Palette(PaletteOrigin::Typed))));
        assert!(!closes(None));
    }
}

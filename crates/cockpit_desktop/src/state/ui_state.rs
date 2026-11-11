//! What the window shows besides the daemon's data: the selected agent, open menus and dialogs.
//! Drafts are apart, in `drafts`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cockpit_protocol::{
    AccountId, AgentId, ClaudeSessionId, DirListing, Effort, Image, ItemId, PermissionMode,
};
use serde::Serialize;

use super::accounts::{AccountsView, AddAccountForm};
use super::agent_map::MapView;
use super::app_state::Loadable;
use crate::keymap::{Chord, Command, Refusal};
use crate::machine::MachineId;

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
    /// The machine's load, from the header's Load chip.
    Load,
    Model,
    Mode,
    Palette(PaletteOrigin),
    /// The grid of emoji that can start the selected agent's title.
    TitleEmoji,
    /// A sidebar row's menu, opened by a right-click at `x`, `y` in the window's pixels.
    RowMenu {
        agent: AgentId,
        x: i32,
        y: i32,
    },
    /// The file view's list of its open files.
    FileList,
    /// A row's menu in the files pane, opened by a right-click at `x`, `y` in the window's pixels.
    /// The tree holds the row.
    TreeMenu {
        agent: AgentId,
        x: i32,
        y: i32,
    },
    /// The selected agent's notification levels, from its header tag.
    AgentNotifications,
    /// The app's notification levels, from the sidebar's bell.
    AppNotifications,
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

/// A dialog. What its fields hold is in the drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    Switcher,
    NewAgent(NewAgentForm),
    OpenFile(OpenFileView),
    /// Asks before closing an agent that works, waits for you, or holds an unsent draft.
    CloseAgent(AgentId),
    /// The agent and every subagent it started (Ctrl+M).
    AgentMap(MapView),
    /// Every Claude account, from the sidebar's foot.
    Accounts(AccountsView),
    AddAccount(AddAccountForm),
    /// The machines the app reaches, with a form that adds a server.
    Machines(MachinesView),
    /// "Where should agents run?" for this computer, while servers are in use.
    NoDaemon,
    /// The update screens of a server whose daemon refuses the app.
    DaemonUpdate(MachineId),
    /// An image over the whole window, from a message or the file view.
    ImageView(ImageView),
    /// Forks or moves an agent's conversation to another working folder, from its row's menu.
    WorkingFolder(WorkingFolderForm),
    /// The app's settings, from the gear in the sidebar's foot.
    Settings(SettingsView),
}

/// The Settings dialog: its tab, and the shortcut it records.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SettingsView {
    pub tab: SettingsTab,
    /// The command whose new shortcut the dialog waits for.
    pub recording: Option<Command>,
    /// The last chord that could not run its command, and why.
    pub refused: Option<RefusedChord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Keyboard,
}

/// A chord a command could not take: pressed while recording, or the default a reset asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedChord {
    pub command: Command,
    pub chord: Chord,
    pub refusal: Refusal,
}

/// The image the full view shows, with the file it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageView {
    pub agent: AgentId,
    /// The file it comes from. `None` for an image attached to a message.
    pub path: Option<PathBuf>,
    pub image: Arc<Image>,
}

/// What the working folder dialog does with the agent's conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderAction {
    /// A new agent carries it on in the folder, under a new Claude session.
    Fork,
    /// The agent itself runs in the folder, on the same session.
    Move,
}

/// The working folder dialog. Its field's text is the new agent dialog's draft: one dialog shows
/// at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingFolderForm {
    pub agent: AgentId,
    pub action: FolderAction,
    /// The agent's machine, whose folders the field lists.
    pub machine: MachineId,
    /// The folders under the field, while it lists some.
    pub folders: Option<FolderList>,
    /// The request is on its way.
    pub sending: bool,
    /// Why the last try failed, shown in the dialog.
    pub error: Option<String>,
}

/// The folder field of the open dialog: the machine whose folders it lists, and its list.
pub struct FolderField<'dialog> {
    pub machine: MachineId,
    pub folders: &'dialog mut Option<FolderList>,
}

/// The "Machines" dialog's form for a new server, and why the last one was refused.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachinesView {
    pub name: String,
    pub host: String,
    pub remote_command: String,
    pub error: Option<String>,
}

/// The "Open file" dialog's data: the folder its path field names, with what the daemon listed
/// there, and the project's files for a bare name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFileView {
    /// The agent whose folders the dialog browses.
    pub agent: AgentId,
    /// Absolute.
    pub folder: PathBuf,
    pub listing: Loadable<DirListing>,
    /// `None` for a path.
    pub search: Option<FileSearch>,
}

/// The bare name searched for in the project, with the files found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearch {
    pub name: String,
    pub found: Loadable<Vec<PathBuf>>,
}

impl OpenFileView {
    pub fn new(agent: AgentId, folder: PathBuf) -> Self {
        Self {
            agent,
            folder,
            listing: Loadable::NotRequested,
            search: None,
        }
    }
}

impl Dialog {
    /// The agent whose leaving closes the dialog: the one it asks to close, maps, forks or moves,
    /// or whose image it shows.
    pub fn agent(&self) -> Option<AgentId> {
        match self {
            Self::CloseAgent(agent) => Some(*agent),
            Self::AgentMap(view) => Some(view.agent),
            Self::ImageView(view) => Some(view.agent),
            Self::WorkingFolder(form) => Some(form.agent),
            Self::NewAgent(_)
            | Self::Switcher
            | Self::OpenFile(_)
            | Self::Accounts(_)
            | Self::AddAccount(_)
            | Self::Machines(_)
            | Self::NoDaemon
            | Self::DaemonUpdate(_)
            | Self::Settings(_) => None,
        }
    }
}

/// Where the chat puts an entry it scrolls to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevealPlace {
    /// At the top of the view, or as high as the chat can scroll it.
    Top,
    /// In the middle of the view, or its top at the view's top when it is taller.
    Middle,
}

/// A chat entry the chat's scroll script scrolls to and highlights for a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatReveal {
    /// Tells this reveal from the last one, so that the same entry asked again scrolls again.
    pub serial: u64,
    pub agent: AgentId,
    /// A top-level item, which has an entry of its own in the chat.
    pub item: ItemId,
    pub place: RevealPlace,
}

/// The agent closed last, which the undo note brings back for a few seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedAgent {
    /// Tells this close from the next one, whose note replaces it.
    pub serial: u32,
    pub agent: AgentId,
    pub title: String,
    /// `None` for an agent Claude Code gave no session yet: nothing can bring it back.
    pub session: Option<ClaudeSessionId>,
    /// The account whose folder holds the session.
    pub account: AccountId,
    /// The agent that was just below it, which it goes back above.
    pub before: Option<AgentId>,
}

/// A closed agent's session on its way back. Once it is an agent again, it goes back to its
/// place, above `before`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reopening {
    pub session: ClaudeSessionId,
    pub before: Option<AgentId>,
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
    /// The Claude account the agent runs with.
    Account,
    /// The model and the effort.
    Model,
    Mode,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewAgentForm {
    pub tab: NewAgentTab,
    /// The machine the agent starts on, whose folders and sessions the dialog lists.
    pub machine: MachineId,
    /// `None` keeps Claude Code's default, as do `effort` and `mode`.
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub mode: Option<PermissionMode>,
    /// The account picked in the menu. `None` follows the folder: the account of the last agent
    /// there, else the default one.
    pub account: Option<AccountId>,
    pub menu: Option<FormMenu>,
    /// A start or resume request is on its way.
    pub starting: bool,
    /// The folders under the folder field, in the recent folders' place. `None` until the field is
    /// typed in.
    pub folders: Option<FolderList>,
}

/// The folder the new agent dialog's folder field names, with the folders the daemon listed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderList {
    /// Absolute.
    pub folder: PathBuf,
    pub listing: Loadable<DirListing>,
}

impl FolderList {
    pub fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            listing: Loadable::NotRequested,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Diff,
    WholeFile,
    /// A markdown file rendered as the chat renders the agent's text.
    Preview,
}

/// The extensions of the files the file view renders, lowercase.
const MARKDOWN_EXTENSIONS: [&str; 2] = ["md", "markdown"];

/// Whether the file view can render `path` as markdown, from its extension.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MARKDOWN_EXTENSIONS
                .iter()
                .any(|markdown| extension.eq_ignore_ascii_case(markdown))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    /// The shown file, relative to the agent's folder when the file is inside it.
    pub path: PathBuf,
    /// The line a `path:line` link marks.
    pub line: Option<u32>,
    pub mode: ViewMode,
    /// Files opened from a link or a pick, besides the changed files, in the order they opened.
    pub opened: Vec<PathBuf>,
    /// Changed files whose tab the user closed. Opening one again brings its tab back.
    pub closed: BTreeSet<PathBuf>,
}

impl FileView {
    pub fn new(path: PathBuf, line: Option<u32>, mode: ViewMode) -> Self {
        Self {
            path,
            line,
            mode,
            opened: Vec::new(),
            closed: BTreeSet::new(),
        }
    }

    /// The tabs, each once: the changed files, then the opened files, without the closed ones.
    /// The shown file always has its tab.
    pub fn tabs(&self, changed: &[PathBuf]) -> Vec<PathBuf> {
        let mut tabs: Vec<PathBuf> = Vec::new();
        let open = changed.iter().chain(&self.opened);
        for path in open.chain([&self.path]) {
            let shown = *path == self.path || !self.closed.contains(path);
            if shown && !tabs.contains(path) {
                tabs.push(path.clone());
            }
        }
        tabs
    }
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
    pub place: TitlePlace,
    /// The field selects its text when the edit starts. After a move to the other place it puts
    /// the caret at the end, so typing goes on after the text instead of replacing it.
    pub select_all: bool,
    /// The window lost the focus while the field had it, which keeps the edit open until the
    /// field takes the focus back. Selecting another agent meanwhile cancels the edit.
    pub kept_open: bool,
}

/// Where the title field shows: one place at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlePlace {
    Header,
    /// The agent's row in the sidebar, from F2 or a double-click on the row's title.
    Sidebar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    pub selected: Option<AgentId>,
    /// Agents that finished while another one was selected, or that their row's menu marked
    /// unread (the selected one too): bold title and a white dot, until they are selected.
    pub unread: BTreeSet<AgentId>,
    /// The machines whose agents the sidebar folds under their header.
    pub folded_machines: BTreeSet<MachineId>,
    pub window_focused: bool,
    /// The floating blocks shown unfolded.
    pub open_blocks: BTreeSet<Block>,
    /// The block shown as a popover from its badge, while the file view is open.
    pub badge: Option<Block>,
    /// The agents whose background block lists the finished tasks, not only the running ones.
    pub finished_tasks_unfolded: BTreeSet<AgentId>,
    pub popover: Option<Popover>,
    pub dialog: Option<Dialog>,
    pub file_views: BTreeMap<AgentId, FileView>,
    pub mcp: McpView,
    /// The agent title the header or a sidebar row shows as a text field.
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
    /// Counts the sends that take the chat to its bottom: the chat's scroll script jumps at each.
    pub chat_jumps: u64,
    /// The next id of a message on its way, one count for every agent.
    pub next_sending: u64,
    pub chat_reveal: Option<ChatReveal>,
    /// Agents the app closed and took off its list, until the daemon says they are removed:
    /// their events in between are not applied.
    pub closing: BTreeSet<AgentId>,
    /// The undo note's agent.
    pub closed: Option<ClosedAgent>,
    next_closed: u32,
    pub reopening: Option<Reopening>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            selected: None,
            unread: BTreeSet::new(),
            folded_machines: BTreeSet::new(),
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
            chat_jumps: 0,
            next_sending: 0,
            chat_reveal: None,
            closing: BTreeSet::new(),
            closed: None,
            next_closed: 0,
            reopening: None,
        }
    }
}

impl UiState {
    /// Scrolls `agent`'s chat to the entry of the top-level `item`, puts it at `place`, and
    /// highlights it.
    pub fn reveal_in_chat(&mut self, agent: AgentId, item: ItemId, place: RevealPlace) {
        let serial = self
            .chat_reveal
            .map_or(0, |last| last.serial.wrapping_add(1));
        self.chat_reveal = Some(ChatReveal {
            serial,
            agent,
            item,
            place,
        });
    }

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

    /// Shows the undo note for an agent just closed, in place of the last one's.
    pub fn note_closed(
        &mut self,
        agent: AgentId,
        title: String,
        session: Option<ClaudeSessionId>,
        account: AccountId,
        before: Option<AgentId>,
    ) {
        self.closed = Some(ClosedAgent {
            serial: self.next_closed,
            agent,
            title,
            session,
            account,
            before,
        });
        self.next_closed = self.next_closed.wrapping_add(1);
    }

    /// Hides the undo note of the close numbered `serial`, unless a later close replaced it.
    pub fn expire_closed(&mut self, serial: u32) {
        self.closed.take_if(|closed| closed.serial == serial);
    }

    pub fn file_view(&self, agent: AgentId) -> Option<&FileView> {
        self.file_views.get(&agent)
    }

    pub fn toggle_finished_tasks(&mut self, agent: AgentId) {
        if !self.finished_tasks_unfolded.remove(&agent) {
            self.finished_tasks_unfolded.insert(agent);
        }
    }

    /// Closes what belongs to the agent shown: its popovers, its badge and its MCP server detail.
    pub fn leave_agent(&mut self) {
        self.popover = None;
        self.badge = None;
        self.mcp.detail = None;
        self.mcp.filter = ToolFilter::All;
    }

    /// Closes the dialog. The "Accounts" and "Add an account" dialogs go back to the new agent
    /// dialog they were opened from, as it was.
    pub fn close_dialog(&mut self) {
        self.dialog = match self.dialog.take() {
            Some(
                Dialog::Accounts(AccountsView {
                    from_new_agent: Some(form),
                    ..
                })
                | Dialog::AddAccount(AddAccountForm {
                    from_new_agent: Some(form),
                    ..
                }),
            ) => Some(Dialog::NewAgent(form)),
            Some(_) | None => None,
        };
    }

    /// Whether the new agent dialog is open, or an "Accounts" dialog goes back to it.
    pub fn holds_new_agent_form(&self) -> bool {
        match &self.dialog {
            Some(Dialog::NewAgent(_)) => true,
            Some(Dialog::Accounts(AccountsView { from_new_agent, .. }))
            | Some(Dialog::AddAccount(AddAccountForm { from_new_agent, .. })) => {
                from_new_agent.is_some()
            }
            Some(_) | None => false,
        }
    }

    /// The open dialog's folder field: the new agent dialog's, or the working folder dialog's.
    pub fn folder_field(&mut self) -> Option<FolderField<'_>> {
        match &mut self.dialog {
            Some(Dialog::NewAgent(form)) => Some(FolderField {
                machine: form.machine,
                folders: &mut form.folders,
            }),
            Some(Dialog::WorkingFolder(form)) => Some(FolderField {
                machine: form.machine,
                folders: &mut form.folders,
            }),
            Some(_) | None => None,
        }
    }

    /// The new agent dialog's form: the open dialog's, or the one an "Accounts" dialog goes back
    /// to, which a daemon's answer may still change.
    pub fn new_agent_form_mut(&mut self) -> Option<&mut NewAgentForm> {
        match &mut self.dialog {
            Some(Dialog::NewAgent(form)) => Some(form),
            Some(Dialog::Accounts(AccountsView { from_new_agent, .. }))
            | Some(Dialog::AddAccount(AddAccountForm { from_new_agent, .. })) => {
                from_new_agent.as_mut()
            }
            Some(_) | None => None,
        }
    }

    /// The agent the new agent dialog asked for started: the dialog closes, and an "Accounts"
    /// dialog no longer goes back to it.
    pub fn new_agent_started(&mut self) {
        match &mut self.dialog {
            Some(Dialog::NewAgent(_)) => self.dialog = None,
            Some(Dialog::Accounts(AccountsView { from_new_agent, .. }))
            | Some(Dialog::AddAccount(AddAccountForm { from_new_agent, .. })) => {
                *from_new_agent = None;
            }
            Some(_) | None => {}
        }
    }

    /// Closes every popover and the dialog, or only the start tab's menu or folder list when one
    /// is open, and cancels a title edit. Returns whether anything was open.
    pub fn close_overlays(&mut self) -> bool {
        if let Some(Dialog::NewAgent(form)) = &mut self.dialog
            && form.tab == NewAgentTab::Start
            && (form.menu.is_some() || form.folders.is_some())
        {
            if form.menu.take().is_none() {
                form.folders = None;
            }
            return true;
        }
        let open = self.popover.is_some()
            || self.dialog.is_some()
            || self.badge.is_some()
            || self.title_edit.is_some();
        self.popover = None;
        self.close_dialog();
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
    fn markdown_files_are_known_by_their_extension() {
        assert!(is_markdown(Path::new("docs/plan.md")));
        assert!(is_markdown(Path::new("NOTES.MD")));
        assert!(is_markdown(Path::new("guide.markdown")));
        assert!(!is_markdown(Path::new("src/md.rs")));
        assert!(!is_markdown(Path::new("md")));
        assert!(!is_markdown(Path::new("archive.md.gz")));
    }

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
    fn escape_on_the_resume_tab_closes_the_dialog() {
        let mut ui = UiState {
            dialog: Some(Dialog::NewAgent(NewAgentForm {
                tab: NewAgentTab::Resume,
                folders: Some(FolderList::new(PathBuf::from("/home/dev"))),
                ..NewAgentForm::default()
            })),
            ..UiState::default()
        };

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
        ui.toggle_popover(Popover::Load);
        assert_eq!(ui.popover, Some(Popover::Load));

        ui.toggle_popover(Popover::Load);
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

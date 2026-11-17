//! What you type before it is sent: the composer's drafts, the command palette's filter, the
//! answers being chosen on question cards, and the fields of the dialogs. The window holds them in
//! a signal of their own, apart from the agents' data, so that typing does not render the
//! transcript again.

use std::collections::{BTreeMap, BTreeSet};

use cockpit_protocol::{AgentId, Image, PendingId};

use super::find::Finds;
use super::new_agent_palette::OpenPalette;
use super::quotes::{Quote, QuoteEditor};
use super::utf16_offset::Utf16Offset;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Draft {
    pub text: String,
    pub images: Vec<Image>,
    /// The passages of the chat the message quotes, in the chat's order.
    pub quotes: Vec<Quote>,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty() && self.quotes.is_empty()
    }
}

/// The answers being chosen on a question card, one entry per asked question.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuestionDraft {
    /// The indexes of the picked options.
    pub picks: Vec<BTreeSet<usize>>,
    /// The "Other" fields.
    pub other: Vec<String>,
    /// The index of the question on screen, which the number keys answer.
    pub shown: usize,
}

impl QuestionDraft {
    /// Whether question `asked` has a pick or an "Other" text.
    pub fn has_answer(&self, asked: usize) -> bool {
        let picked = self.picks.get(asked).is_some_and(|picks| !picks.is_empty());
        let typed = self
            .other
            .get(asked)
            .is_some_and(|text| !text.trim().is_empty());
        picked || typed
    }
}

/// A "/" word in the composer's draft, as byte offsets: from its "/" to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashWord {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Palette {
    pub query: String,
    /// The "/" word the caret is in, when typing opened the palette: a pick replaces it.
    pub word: Option<SlashWord>,
    /// An index into the filtered commands.
    pub highlighted: usize,
    /// Up or Down moved the highlight since the filter last changed. Left and Right change the
    /// highlighted setting only then, and else move the filter's caret.
    pub moved: bool,
}

/// The new agent dialog's fields, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewAgentDraft {
    pub folder: String,
    /// An index into the rows under the folder field. `None` until a filter or a key picks a row.
    pub folder_highlighted: Option<usize>,
    /// The first message.
    pub prompt: String,
    /// The agent's name. Empty lets the daemon name it, after the first message or the folder.
    pub name: String,
    /// What the "Resume a session" tab's search field holds.
    pub session_query: String,
    /// An index into the sessions the query leaves.
    pub session_highlighted: usize,
    /// The "/" palette over the first message, while it is open.
    pub palette: Option<OpenPalette>,
    /// The start of the "/" word Esc closed the palette on: it stays closed for that word.
    pub palette_dismissed: Option<usize>,
}

/// The "Add an account" dialog's fields, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddAccountDraft {
    pub name: String,
    pub folder: String,
    /// The folder was typed in: it no longer follows the name.
    pub folder_typed: bool,
}

/// The agent switcher's search, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SwitcherDraft {
    pub query: String,
    /// An index into the agents the query leaves.
    pub highlighted: usize,
}

/// The file view's list of open files, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileListDraft {
    pub query: String,
    /// An index into the files the query leaves.
    pub highlighted: usize,
}

/// The "Open file" dialog's path field, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OpenFileDraft {
    pub path: String,
    /// An index into the dialog's rows. `None` until a filter or a key picks a row.
    pub highlighted: Option<usize>,
    /// A key moved the highlight since the text last changed.
    pub chosen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Drafts {
    pub composer: BTreeMap<AgentId, Draft>,
    /// Counts, per agent, the times the app changed a draft's text itself, as opposed to typing.
    /// The composer takes the draft's text at each, and else leaves its text to the keyboard: a
    /// render that lags behind typing would put an older text back.
    composer_writes: BTreeMap<AgentId, u64>,
    /// Where the caret goes in the text the app wrote last. Without one, it goes at the end.
    composer_carets: BTreeMap<AgentId, Utf16Offset>,
    /// Per agent, the start of the "/" word whose palette Esc closed: it stays closed until
    /// another word, or until the app writes that agent's draft.
    pub palette_dismissed: BTreeMap<AgentId, usize>,
    pub questions: BTreeMap<PendingId, QuestionDraft>,
    pub palette: Palette,
    /// The text of the title field, the header's or a sidebar row's, while a title is edited.
    pub title: String,
    pub new_agent: NewAgentDraft,
    pub add_account: AddAccountDraft,
    pub switcher: SwitcherDraft,
    pub open_file: OpenFileDraft,
    pub file_list: FileListDraft,
    /// The field where a comment on a quote is written, while it is open.
    pub quote_editor: Option<QuoteEditor>,
    /// The last quote id or opening of the comment field given out.
    pub(super) quote_serial: u32,
    /// What each agent's finds look for (Ctrl+F).
    pub finds: Finds,
}

impl Drafts {
    pub fn draft(&self, agent: AgentId) -> Option<&Draft> {
        self.composer.get(&agent)
    }

    pub fn draft_mut(&mut self, agent: AgentId) -> &mut Draft {
        self.composer.entry(agent).or_default()
    }

    /// For a draft text the app changed, so that the composer shows it when it shows `agent`.
    pub fn draft_written(&mut self, agent: AgentId) {
        self.composer_carets.remove(&agent);
        self.palette_dismissed.remove(&agent);
        let writes = self.composer_writes.entry(agent).or_default();
        *writes = writes.wrapping_add(1);
    }

    /// As `draft_written`, with the caret at `caret`.
    pub fn draft_written_at(&mut self, agent: AgentId, caret: Utf16Offset) {
        self.draft_written(agent);
        self.composer_carets.insert(agent, caret);
    }

    /// Where the caret goes in the text the app wrote last; `None` at its end.
    pub fn written_caret(&self, agent: AgentId) -> Option<Utf16Offset> {
        self.composer_carets.get(&agent).copied()
    }

    /// Typing moved on from the text the app wrote.
    pub fn draft_typed(&mut self, agent: AgentId) {
        self.composer_carets.remove(&agent);
    }

    pub fn composer_writes(&self, agent: AgentId) -> u64 {
        self.composer_writes
            .get(&agent)
            .copied()
            .unwrap_or_default()
    }

    /// Drops the drafts of the agents `keep` refuses.
    pub fn retain_agents(&mut self, keep: impl Fn(AgentId) -> bool) {
        self.composer.retain(|&agent, _| keep(agent));
        self.composer_writes.retain(|&agent, _| keep(agent));
        self.composer_carets.retain(|&agent, _| keep(agent));
        self.finds.retain(&keep);
        if self
            .quote_editor
            .as_ref()
            .is_some_and(|editor| !keep(editor.agent))
        {
            self.quote_editor = None;
        }
    }
}

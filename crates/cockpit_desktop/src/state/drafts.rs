//! What you type before it is sent: the composer's drafts, the command palette's filter, the
//! answers being chosen on question cards, and the fields of the dialogs. The window holds them in
//! a signal of their own, apart from the agents' data, so that typing does not render the
//! transcript again.

use std::collections::{BTreeMap, BTreeSet};

use cockpit_protocol::{AgentId, Image, PendingId};

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
    /// Up or Down moved the highlight since the filter last changed. Left and Right change the
    /// highlighted setting only then, and else move the filter's caret.
    pub moved: bool,
}

/// The new agent dialog's fields, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewAgentDraft {
    pub folder: String,
    /// The first message.
    pub prompt: String,
    /// What the "Resume a session" tab's search field holds.
    pub session_query: String,
    /// An index into the sessions the query leaves.
    pub session_highlighted: usize,
}

/// The agent switcher's search, while it is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SwitcherDraft {
    pub query: String,
    /// An index into the agents the query leaves.
    pub highlighted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Drafts {
    pub composer: BTreeMap<AgentId, Draft>,
    /// Counts, per agent, the times the app changed a draft's text itself, as opposed to typing.
    /// The composer takes the draft's text at each, and else leaves its text to the keyboard: a
    /// render that lags behind typing would put an older text back.
    composer_writes: BTreeMap<AgentId, u64>,
    pub questions: BTreeMap<PendingId, QuestionDraft>,
    pub palette: Palette,
    /// The text of the title field, the header's or a sidebar row's, while a title is edited.
    pub title: String,
    pub new_agent: NewAgentDraft,
    pub switcher: SwitcherDraft,
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
        let writes = self.composer_writes.entry(agent).or_default();
        *writes = writes.wrapping_add(1);
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
    }
}

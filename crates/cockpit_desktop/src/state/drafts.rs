//! What you type before it is sent: the composer's drafts, the command palette's filter and the
//! answers being chosen on question cards. The window holds them in a signal of their own, apart
//! from the agents' data, so that typing does not render the transcript again.

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
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Drafts {
    pub composer: BTreeMap<AgentId, Draft>,
    /// Counts the times the app changed a draft's text itself, as opposed to typing. The composer
    /// takes the draft's text at each, and else leaves its text to the keyboard: a render that
    /// lags behind typing would put an older text back.
    pub composer_writes: u64,
    pub questions: BTreeMap<PendingId, QuestionDraft>,
    pub palette: Palette,
    /// The text of the header's title field while a title is edited.
    pub title: String,
}

impl Drafts {
    pub fn draft(&self, agent: AgentId) -> Option<&Draft> {
        self.composer.get(&agent)
    }

    pub fn draft_mut(&mut self, agent: AgentId) -> &mut Draft {
        self.composer.entry(agent).or_default()
    }

    /// For a draft text the app changed, so that the composer shows it.
    pub fn draft_written(&mut self) {
        self.composer_writes = self.composer_writes.wrapping_add(1);
    }
}

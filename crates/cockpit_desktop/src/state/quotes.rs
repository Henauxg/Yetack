//! Quotes of the chat that wait in an agent's draft, each with a comment, and the field where a
//! comment is written. They go out with the agent's next message, as Markdown quotes, each
//! followed by its comment.

use cockpit_protocol::{AgentId, ItemId};
use serde::{Deserialize, Serialize};

use super::drafts::Drafts;
use super::utf16_offset::Utf16Offset;
use crate::prefs::CommentPlacement;

/// Names a quote among its agent's quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QuoteId(u32);

impl QuoteId {
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Where a passage sits in the chat: the transcript entry that shows it, and its start and end in
/// that entry's text, as the page's script counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QuoteAnchor {
    pub item: ItemId,
    pub start: Utf16Offset,
    pub end: Utf16Offset,
}

/// A passage the user selected in the chat, as the page's script sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passage {
    pub anchor: QuoteAnchor,
    /// The entry's text between the anchor's offsets. The script finds the passage again by it
    /// when the entry's text moved.
    pub raw: String,
    /// The text as the page shows it, with its line breaks: the message quotes this one.
    pub text: String,
    /// The passage lies in code or in a command's input or output, which the message quotes as
    /// code.
    pub code: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub id: QuoteId,
    pub passage: Passage,
    pub comment: String,
    /// Beside: its comment shows in a note beside the passage, while the chat has room for one.
    /// Hidden: only its number does.
    pub placement: CommentPlacement,
}

/// What the comment field writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorTarget {
    /// A passage just selected: Enter adds its quote.
    New(Passage),
    /// A quote's comment, which Enter replaces.
    Existing(QuoteId),
}

/// The comment field, while it is open. One shows at a time, for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteEditor {
    pub agent: AgentId,
    pub target: EditorTarget,
    /// What the field holds.
    pub text: String,
    /// Tells this opening of the field from the ones before, on the same passage too.
    pub opening: u32,
}

/// Opens the comment field on `passage`, in place of the one open.
pub fn start(drafts: &mut Drafts, agent: AgentId, passage: Passage) {
    let opening = drafts.next_serial();
    drafts.quote_editor = Some(QuoteEditor {
        agent,
        target: EditorTarget::New(passage),
        text: String::new(),
        opening,
    });
}

/// Opens the comment field on the comment of `agent`'s quote `id`.
pub fn edit(drafts: &mut Drafts, agent: AgentId, id: QuoteId) {
    let Some(quote) = quote(drafts, agent, id) else {
        return;
    };
    let text = quote.comment.clone();
    let opening = drafts.next_serial();
    drafts.quote_editor = Some(QuoteEditor {
        agent,
        target: EditorTarget::Existing(id),
        text,
        opening,
    });
}

pub fn type_comment(drafts: &mut Drafts, text: String) {
    if let Some(editor) = &mut drafts.quote_editor {
        editor.text = text;
    }
}

/// Adds the quote the field was opened on, or changes the comment it was opened on, and closes
/// the field. A new quote's comment takes `placement`.
pub fn commit(drafts: &mut Drafts, placement: CommentPlacement) {
    let Some(editor) = drafts.quote_editor.take() else {
        return;
    };
    let comment = editor.text.trim().to_owned();
    match editor.target {
        EditorTarget::New(passage) => {
            let id = QuoteId(drafts.next_serial());
            let quotes = &mut drafts.draft_mut(editor.agent).quotes;
            // In the chat's order, so the numbers read from top to bottom.
            let at = quotes.partition_point(|quote| quote.passage.anchor <= passage.anchor);
            quotes.insert(
                at,
                Quote {
                    id,
                    passage,
                    comment,
                    placement,
                },
            );
        }
        EditorTarget::Existing(id) => {
            if let Some(quote) = quote_mut(drafts, editor.agent, id) {
                quote.comment = comment;
            }
        }
    }
}

/// Closes the comment field of `shown`, the agent on screen, without a change. Another agent's
/// field stays: it does not show. Returns whether a field closed.
pub fn cancel(drafts: &mut Drafts, shown: Option<AgentId>) -> bool {
    let open = drafts
        .quote_editor
        .as_ref()
        .is_some_and(|editor| Some(editor.agent) == shown);
    if open {
        drafts.quote_editor = None;
    }
    open
}

pub fn remove(drafts: &mut Drafts, agent: AgentId, id: QuoteId) {
    if let Some(draft) = drafts.composer.get_mut(&agent) {
        draft.quotes.retain(|quote| quote.id != id);
    }
    let editing = drafts
        .quote_editor
        .as_ref()
        .is_some_and(|editor| editor.agent == agent && editor.target == EditorTarget::Existing(id));
    if editing {
        drafts.quote_editor = None;
    }
}

/// Shows the comment of `agent`'s quote `id` beside its passage, or hides it behind its number.
pub fn set_placement(
    drafts: &mut Drafts,
    agent: AgentId,
    id: QuoteId,
    placement: CommentPlacement,
) {
    if let Some(quote) = quote_mut(drafts, agent, id) {
        quote.placement = placement;
    }
}

/// The number the chat and the composer show for the quote at `index` in its draft.
pub fn number(index: usize) -> usize {
    index + 1
}

/// The message `quotes` and `typed` make: each quote as a Markdown quote, followed by its comment,
/// then the typed text.
pub fn message(quotes: &[Quote], typed: &str) -> String {
    let mut parts = Vec::new();
    for quote in quotes {
        parts.push(quoted(&quote.passage));
        if !quote.comment.is_empty() {
            parts.push(quote.comment.clone());
        }
    }
    let typed = typed.trim();
    if !typed.is_empty() {
        parts.push(typed.to_owned());
    }
    parts.join("\n\n")
}

/// `passage` as a Markdown quote: `>` on each line, its code in a fence long enough for the
/// backticks it holds.
fn quoted(passage: &Passage) -> String {
    let text = passage.text.trim_matches(['\n', '\r']);
    let mut lines = Vec::new();
    let fence = passage
        .code
        .then(|| "`".repeat(longest_backtick_run(text).max(2) + 1));
    if let Some(fence) = &fence {
        lines.push(format!("> {fence}"));
    }
    for line in text.lines() {
        // Code keeps its indent, inside its fence.
        let line = if passage.code {
            line.trim_end()
        } else {
            line.trim()
        };
        if line.is_empty() {
            lines.push(">".to_owned());
        } else {
            lines.push(format!("> {line}"));
        }
    }
    if let Some(fence) = &fence {
        lines.push(format!("> {fence}"));
    }
    lines.join("\n")
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for character in text.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

fn quote(drafts: &Drafts, agent: AgentId, id: QuoteId) -> Option<&Quote> {
    drafts
        .draft(agent)?
        .quotes
        .iter()
        .find(|quote| quote.id == id)
}

fn quote_mut(drafts: &mut Drafts, agent: AgentId, id: QuoteId) -> Option<&mut Quote> {
    drafts
        .composer
        .get_mut(&agent)?
        .quotes
        .iter_mut()
        .find(|quote| quote.id == id)
}

impl Drafts {
    /// A number no quote and no opening of the field had in this run.
    fn next_serial(&mut self) -> u32 {
        self.quote_serial = self.quote_serial.wrapping_add(1);
        self.quote_serial
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn passage(item: u32, start: usize, text: &str) -> Passage {
        Passage {
            anchor: QuoteAnchor {
                item: ItemId::new(item),
                start: Utf16Offset::new(start),
                end: Utf16Offset::new(start + text.len()),
            },
            raw: text.to_owned(),
            text: text.to_owned(),
            code: false,
        }
    }

    fn add(drafts: &mut Drafts, passage: Passage, comment: &str) {
        start(drafts, agent(), passage);
        type_comment(drafts, comment.to_owned());
        commit(drafts, CommentPlacement::Beside);
    }

    fn quotes(drafts: &Drafts) -> &[Quote] {
        drafts
            .draft(agent())
            .map_or(&[], |draft| draft.quotes.as_slice())
    }

    #[test]
    fn quotes_keep_the_chat_order_whatever_order_they_were_added_in() {
        let mut drafts = Drafts::default();

        add(&mut drafts, passage(7, 40, "later in the entry"), "second");
        add(&mut drafts, passage(9, 0, "a later entry"), "third");
        add(&mut drafts, passage(7, 3, "earlier in the entry"), "first");

        let comments: Vec<&str> = quotes(&drafts)
            .iter()
            .map(|quote| quote.comment.as_str())
            .collect();
        assert_eq!(comments, ["first", "second", "third"]);
        assert_eq!(drafts.quote_editor, None);
    }

    #[test]
    fn the_placement_decides_whether_a_new_comment_shows_beside_its_passage() {
        let mut drafts = Drafts::default();
        add(&mut drafts, passage(1, 0, "shown"), "a");
        start(&mut drafts, agent(), passage(2, 0, "hidden"));
        commit(&mut drafts, CommentPlacement::Hidden);

        let placements: Vec<CommentPlacement> = quotes(&drafts)
            .iter()
            .map(|quote| quote.placement)
            .collect();

        assert_eq!(
            placements,
            [CommentPlacement::Beside, CommentPlacement::Hidden]
        );
    }

    #[test]
    fn editing_a_comment_replaces_it_and_cancel_leaves_it() {
        let mut drafts = Drafts::default();
        add(&mut drafts, passage(1, 0, "passage"), "first thought");
        let id = quotes(&drafts)[0].id;

        edit(&mut drafts, agent(), id);
        assert_eq!(
            drafts
                .quote_editor
                .as_ref()
                .map(|editor| editor.text.as_str()),
            Some("first thought")
        );
        type_comment(&mut drafts, "  second thought  ".to_owned());
        commit(&mut drafts, CommentPlacement::Hidden);

        edit(&mut drafts, agent(), id);
        type_comment(&mut drafts, "dropped".to_owned());
        assert!(cancel(&mut drafts, Some(agent())));
        assert!(!cancel(&mut drafts, Some(agent())));

        assert_eq!(quotes(&drafts).len(), 1);
        assert_eq!(quotes(&drafts)[0].comment, "second thought");
        assert_eq!(
            quotes(&drafts)[0].placement,
            CommentPlacement::Beside,
            "an edit keeps where the comment shows"
        );
    }

    /// The page keeps a field's text while its element stays: each opening must tell it apart.
    #[test]
    fn each_opening_of_the_field_is_a_new_one_even_on_the_same_passage() {
        let mut drafts = Drafts::default();
        start(&mut drafts, agent(), passage(1, 0, "same"));
        type_comment(&mut drafts, "first thought".to_owned());
        let first = drafts.quote_editor.as_ref().map(|editor| editor.opening);

        start(&mut drafts, agent(), passage(1, 0, "same"));
        let second = drafts.quote_editor.as_ref().map(|editor| editor.opening);

        assert_ne!(first, second);
        assert_eq!(
            drafts
                .quote_editor
                .as_ref()
                .map(|editor| editor.text.as_str()),
            Some("")
        );
    }

    #[test]
    fn removing_the_quote_being_edited_closes_the_field() {
        let mut drafts = Drafts::default();
        add(&mut drafts, passage(1, 0, "one"), "a");
        add(&mut drafts, passage(2, 0, "two"), "b");
        let first = quotes(&drafts)[0].id;
        edit(&mut drafts, agent(), first);

        remove(&mut drafts, agent(), first);

        assert_eq!(drafts.quote_editor, None);
        assert_eq!(quotes(&drafts).len(), 1);
        assert_eq!(quotes(&drafts)[0].comment, "b");
    }

    #[test]
    fn an_agent_that_leaves_takes_its_open_field_with_it() {
        let mut drafts = Drafts::default();
        start(&mut drafts, agent(), passage(1, 0, "gone with it"));

        drafts.retain_agents(|kept| kept != agent());

        assert_eq!(drafts.quote_editor, None);
    }

    #[test]
    fn a_quote_makes_the_draft_worth_sending() {
        let mut drafts = Drafts::default();
        add(&mut drafts, passage(1, 0, "only a quote"), "");

        assert!(drafts.draft(agent()).is_some_and(|draft| !draft.is_empty()));
    }

    #[test]
    fn the_message_quotes_each_passage_before_its_comment_then_the_typed_text() {
        let mut drafts = Drafts::default();
        add(
            &mut drafts,
            passage(1, 0, "a slow host cannot push\nanother out"),
            "Per host too?",
        );
        add(&mut drafts, passage(2, 0, "dropped when read"), "");

        let message = message(quotes(&drafts), "  Otherwise go ahead.\n");

        assert_eq!(
            message,
            "> a slow host cannot push\n> another out\n\nPer host too?\n\n> dropped when read\n\nOtherwise go ahead."
        );
    }

    /// Four spaces at a line's start would make the quote's line a code block.
    #[test]
    fn quoted_prose_drops_the_indent_of_its_lines() {
        let mut drafts = Drafts::default();
        add(
            &mut drafts,
            passage(
                1,
                0,
                "    indented line
  next",
            ),
            "",
        );

        assert_eq!(
            message(quotes(&drafts), ""),
            "> indented line
> next"
        );
    }

    #[test]
    fn quoted_code_keeps_its_blank_lines_in_a_fence_longer_than_its_backticks() {
        let mut code = passage(1, 0, "");
        code.text = "let a = 1;\n\nlet b = \"```\";\n".to_owned();
        code.code = true;
        let quote = Quote {
            id: QuoteId(1),
            passage: code,
            comment: "Why?".to_owned(),
            placement: CommentPlacement::Beside,
        };

        let message = message(&[quote], "");

        assert_eq!(
            message,
            "> ````\n> let a = 1;\n>\n> let b = \"```\";\n> ````\n\nWhy?"
        );
    }
}

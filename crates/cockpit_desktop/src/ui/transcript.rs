//! The conversation: user bubbles, agent text, tool calls, inline diffs, subagents and the cards
//! that wait for an answer. The steps of a turn sit on a vertical line, each with a dot whose
//! color says where the step stands.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use cockpit_protocol::{
    AgentId, AgentStatus, AnswerState, ItemBody, ItemId, SignInWait, ToolCall, ToolCallStatus,
    TranscriptItem, TurnEnd, TurnOutcome, UserMessage,
};
use dioxus::prelude::*;
use serde::Serialize;
use serde_json::Value;

use crate::connection::{Clock, Cockpit};
use crate::keymap::Command;
use crate::render::{self, Diff, Found, Language, OutputPart, SearchTool, diff_lines};
use crate::state::{self, AgentEntry, HistoryState, TurnStart, accounts, actions, file_edits};
use crate::ui::accounts::SignInCard;
use crate::ui::cards::{ApprovalCard, PlanCard, QuestionCard};
use crate::ui::chat_scroll::{
    self, CHAT_SCROLL_ID, ENTRY_CLASS, FOLD_BUTTON_CLASS, JumpToLatest, NewDivider,
};
use crate::ui::code::{LineMarks, LineNumbers, diff_row};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::last_instruction::LastInstruction;
use crate::ui::loading::TranscriptSkeleton;
use crate::ui::markdown::{MARKDOWN_CLASS, MarkdownView};
use crate::ui::tool_rows::{self, ExploredView, Row, SkillView};
use crate::{bridge, format};

/// What a call's line says while it runs, before its time or its count of calls.
const RUNNING_WORD: &str = "running";

/// Screen lines a folded IN or OUT row keeps, wrapped lines counted.
const IN_FOLD_LINES: u8 = 3;
const OUT_FOLD_LINES: u8 = 5;

/// Screen lines a long message you sent keeps while folded.
const MESSAGE_FOLD_LINES: u8 = 8;

/// The classes the fold script looks for: an IN or OUT row, the box that cuts its text, and the
/// label it writes on the row's button.
const IO_ROW_CLASS: &str = "io-row";
const IO_CLIP_CLASS: &str = "io-clip";
const FOLD_LABEL_CLASS: &str = "fold-more";
/// Marks a row that keeps its last screen lines when folded.
const FOLD_LAST_CLASS: &str = "fold-last";

/// The same for a message you sent: the bubble, and the box that cuts its text.
const MESSAGE_ROW_CLASS: &str = "message-fold";
/// A message you sent. The quotes' script finds the quotes in it.
pub const USER_BUBBLE_CLASS: &str = "user-bubble";
const MESSAGE_CLIP_CLASS: &str = "message-clip";

/// The CSS property in which a row tells the fold script how many screen lines it keeps.
const FOLD_LINES_PROPERTY: &str = "--fold-lines";

/// Folds IN and OUT rows, and long messages you sent, by their lines on screen, which only the
/// page knows: wrapping depends on the width. It marks `data-tall` each row whose text takes more
/// screen lines than the `--fold-lines` the row sets, and writes on its button how many lines the
/// fold hides. A message's Markdown has gaps between its blocks, so the script counts its lines
/// of text and cuts it where its last kept line ends. A row that keeps its last lines starts them
/// where a line of its text starts, unless that line alone takes more than the kept screen lines.
/// The script sets on each clip the height of
/// its kept lines (`--fold-height`) and of its whole text (`--full-height`), and a fold or an
/// unfold animates the clip between the two. A ResizeObserver measures each row's text when it
/// appears and when its width or its text changes, so a resize of the window renders nothing.
/// Message fields are serde's names for `FoldClasses`.
const FOLD_SCRIPT: &str = r#"
const classes = await dioxus.recv();
if (!window.agentsCockpitFolds) {
  window.agentsCockpitFolds = true;
  const textSelector = `.${classes.clip} > pre, .${classes.message_clip} > .${classes.markdown}`;
  // Where each screen line of the text ends, from the text's top. A table row's cells overlap
  // and count as one line.
  const lineBottoms = (text) => {
    const top = text.getBoundingClientRect().top;
    const bottoms = [];
    const range = document.createRange();
    const walker = document.createTreeWalker(text, NodeFilter.SHOW_TEXT);
    while (walker.nextNode() !== null) {
      range.selectNodeContents(walker.currentNode);
      for (const rect of range.getClientRects()) {
        const last = bottoms.length - 1;
        if (rect.width === 0) {
          continue;
        } else if (last >= 0 && rect.top - top < bottoms[last]) {
          bottoms[last] = Math.max(bottoms[last], rect.bottom - top);
        } else {
          bottoms.push(rect.bottom - top);
        }
      }
    }
    return bottoms;
  };
  // Where the kept last lines start, from the text's top: at the start of the earliest line from
  // which the rest of the text fits in `keptHeight`. Lines are walked from the end.
  const lastLinesTop = (text, height, keptHeight) => {
    const top = text.getBoundingClientRect().top;
    const range = document.createRange();
    const walker = document.createTreeWalker(text, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode() !== null) {
      nodes.push(walker.currentNode);
    }
    let start = null;
    for (let index = nodes.length - 1; index >= 0; index--) {
      const node = nodes[index];
      // `lastIndexOf` reads a start of -1 as 0 and would find the same break again: the walk ends
      // after 0.
      for (let at = node.data.lastIndexOf("\n"); at >= 0; at = at > 0 ? node.data.lastIndexOf("\n", at - 1) : -1) {
        // A break that ends its node starts its line in the next node.
        const [lineNode, lineAt] = at + 1 < node.data.length ? [node, at + 1] : [nodes[index + 1], 0];
        if (lineNode === undefined || lineNode.data.length === 0) {
          continue;
        }
        range.setStart(lineNode, lineAt);
        range.setEnd(lineNode, lineAt + 1);
        const rect = range.getClientRects()[0];
        if (rect === undefined) {
          continue;
        }
        const lineTop = rect.top - top;
        if (height - lineTop > keptHeight + 1) {
          return start ?? height - keptHeight;
        }
        start = lineTop;
      }
    }
    return start ?? height - keptHeight;
  };
  const measure = (text, height) => {
    const row = text.closest(`.${classes.row}, .${classes.message_row}`);
    if (row === null) {
      return;
    }
    const kept = parseInt(getComputedStyle(row).getPropertyValue(classes.lines_property), 10);
    const last = row.classList.contains(classes.last);
    let hidden;
    let foldHeight;
    if (row.matches(`.${classes.message_row}`)) {
      const bottoms = lineBottoms(text);
      hidden = bottoms.length - kept;
      foldHeight = bottoms[kept - 1];
    } else {
      const lineHeight = parseFloat(getComputedStyle(text).lineHeight);
      hidden = Math.round(height / lineHeight) - kept;
      foldHeight = kept * lineHeight;
      if (last && hidden > 0) {
        const start = lastLinesTop(text, height, foldHeight);
        hidden = Math.round(start / lineHeight);
        foldHeight = height - start;
      }
    }
    if (!(hidden > 0)) {
      delete row.dataset.tall;
      return;
    }
    const clip = text.parentElement;
    clip.style.setProperty("--fold-height", `${Math.ceil(foldHeight)}px`);
    clip.style.setProperty("--full-height", `${Math.ceil(height)}px`);
    row.dataset.tall = "";
    const label = row.querySelector(`.${classes.label}`);
    if (label !== null) {
      const lines = hidden === 1 ? "line" : "lines";
      label.textContent = last ? `Show ${hidden} earlier ${lines}` : `Show ${hidden} ${lines} more`;
    }
  };
  // A height measured again applies at once, before the transition comes back: an unfolded clip
  // that trailed its text would cut its last lines while the window resizes.
  const sizes = new ResizeObserver((entries) => {
    const clips = entries.map((entry) => entry.target.parentElement);
    clips.forEach((clip) => clip.style.setProperty("transition", "none"));
    for (const entry of entries) {
      measure(entry.target, entry.contentRect.height);
    }
    void document.body.offsetHeight;
    clips.forEach((clip) => clip.style.removeProperty("transition"));
  });
  const watch = (node, added) => {
    if (!(node instanceof Element)) {
      return;
    }
    const texts = node.matches(textSelector) ? [node] : node.querySelectorAll(textSelector);
    for (const text of texts) {
      if (added) {
        sizes.observe(text);
      } else {
        sizes.unobserve(text);
      }
    }
  };
  new MutationObserver((records) => {
    for (const record of records) {
      record.removedNodes.forEach((node) => watch(node, false));
      record.addedNodes.forEach((node) => watch(node, true));
    }
  }).observe(document.body, { childList: true, subtree: true });
  watch(document.body, true);
}
"#;

/// Diff rows an inline edit shows before "open in the file view".
const EDIT_PREVIEW_ROWS: usize = 8;

/// Unchanged lines kept around each change of an inline edit.
const EDIT_CONTEXT_LINES: usize = 2;

/// The prefix and separator of an MCP tool's name, as in `mcp__server__tool`.
const MCP_TOOL_PREFIX: &str = "mcp__";
const MCP_TOOL_SEPARATOR: &str = "__";

/// The input fields of a shell command, as Claude Code's `Bash` tool sends them.
const COMMAND_FIELD: &str = "command";
const DESCRIPTION_FIELD: &str = "description";

/// The input field of the line `Read` starts at.
const OFFSET_FIELD: &str = "offset";

/// The tags Claude Code puts around a tool's error.
const TOOL_ERROR_OPEN: &str = "<tool_use_error>";
const TOOL_ERROR_CLOSE: &str = "</tool_use_error>";

/// The rows a chat draws in the frame it opens, enough to fill the window. The older rows follow
/// in chunks, one per frame: drawing a long conversation at once keeps the window from answering
/// a click for about 100 ms.
const FIRST_ROWS: usize = 25;
/// The older rows each following frame adds.
const CHUNK_ROWS: usize = 40;

/// How far up the chat draws the selected agent's rows, past the frame it opens in. A row's item
/// marks the edge, so a row the working agent adds below moves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drawn {
    /// From this item's row on.
    From(AgentId, ItemId),
    All(AgentId),
}

/// The next step after the chat drew `drawn` of `rows`, the ids of its rows, oldest first: the
/// next chunk's edge, or all of them. A chat that opens on a place scrolled up draws all at once,
/// since its skeleton shows until then.
fn next_drawn(
    agent: AgentId,
    drawn: Option<Drawn>,
    rows: &[ItemId],
    scrolled_up: bool,
) -> Option<Drawn> {
    let first = match drawn {
        Some(Drawn::All(shown)) if shown == agent => return None,
        Some(Drawn::From(shown, from)) if shown == agent => {
            rows.iter().position(|id| *id >= from).unwrap_or(rows.len())
        }
        _ if scrolled_up => 0,
        _ => rows.len().saturating_sub(FIRST_ROWS),
    };
    let next = first.saturating_sub(CHUNK_ROWS);
    Some(match rows.get(next) {
        Some(id) if next > 0 => Drawn::From(agent, *id),
        _ => Drawn::All(agent),
    })
}

/// Which rows of the main conversation the chat draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShownRows {
    All,
    /// The latest rows, from this index on, in the frame the chat opens.
    Latest(usize),
    /// No row but the skeleton, in the frame the chat opens on a place scrolled up: the latest
    /// rows may not hold that place.
    Skeleton,
}

impl ShownRows {
    /// The rows of `agent`'s chat, whose rows have the ids `rows`, oldest first, as far as
    /// `drawn` reached. Only the opening frame looks at `scrolled_up`: a scroll while the chunks
    /// come keeps the rows drawn.
    fn of(agent: AgentId, drawn: Option<Drawn>, rows: &[ItemId], scrolled_up: bool) -> Self {
        match drawn {
            Some(Drawn::All(shown)) if shown == agent => Self::All,
            Some(Drawn::From(shown, from)) if shown == agent => {
                match rows.iter().position(|id| *id >= from) {
                    Some(first) if first > 0 => Self::Latest(first),
                    Some(_) | None => Self::All,
                }
            }
            _ => Self::opening(rows.len(), scrolled_up),
        }
    }

    /// In the frame a chat of `rows` rows opens.
    fn opening(rows: usize, scrolled_up: bool) -> Self {
        if rows <= FIRST_ROWS {
            Self::All
        } else if scrolled_up {
            Self::Skeleton
        } else {
            Self::Latest(rows - FIRST_ROWS)
        }
    }

    /// The index of the first row drawn, `rows` when none is.
    fn first(self, rows: usize) -> usize {
        match self {
            Self::All => 0,
            Self::Latest(first) => first,
            Self::Skeleton => rows,
        }
    }
}

#[component]
pub fn TranscriptView() -> Element {
    use_hook(start_fold_script);
    let cockpit: Cockpit = use_context();
    let top_entry = chat_scroll::use_chat_scroll(cockpit);
    // The agent whose conversation the chat opened, once its first page came or failed. A
    // failure shows above all the rows.
    let opened = use_memo(move || {
        let state = cockpit.state.read();
        state
            .selected()
            .filter(|entry| !entry.transcript.history().awaits_first_page())
            .map(AgentEntry::id)
    });
    // Effects run once the frame's changes are on the page: the chat draws its latest rows in
    // one frame, then a chunk of older ones in each following frame. The rows' ids come from the
    // last render, which the effect follows.
    let mut drawn = use_signal(|| None::<Drawn>);
    let mut rows = use_hook(|| CopyValue::new((None::<AgentId>, Vec::<ItemId>::new())));
    use_effect(move || {
        let Some(agent) = opened() else {
            return;
        };
        let current = drawn();
        let scrolled_up = cockpit
            .state
            .peek()
            .ui
            .chat_scrolled_up
            .contains_key(&agent);
        let next = {
            let (rows_agent, ids) = &*rows.read();
            if *rows_agent != Some(agent) {
                return;
            }
            next_drawn(agent, current, ids, scrolled_up)
        };
        if let Some(next) = next {
            drawn.set(Some(next));
        }
    });
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {
            div { class: "chat-scroll scroll",
                div { class: "chat-column",
                    p { class: "chat-empty", "Pick an agent on the left, or start one with {state.prefs.keys.shown(Command::NewAgent)}." }
                }
            }
        };
    };
    let agent = entry.id();
    let folder = entry.summary.folder.clone();
    let status = entry.summary.status;
    let working = status == AgentStatus::Working;
    let turn_start = entry.turn_start();
    let items: Vec<&TranscriptItem> = entry.transcript.items().collect();
    let loaded: BTreeSet<ItemId> = items.iter().map(|item| item.id).collect();
    let children = children_by_parent(&items);
    let awaiting = state::awaiting_approval(&items);
    // The card of a turn that found no login says what its notice says.
    let sign_in = accounts::sign_in_turn(entry);
    let sign_in_notice = sign_in.as_ref().and_then(|turn| turn.notice);
    let sign_in_end = sign_in.as_ref().map(|turn| turn.turn_end);
    let waits_for_sign_in = entry.summary.sign_in_wait == SignInWait::Waiting;
    let top_items: Vec<&TranscriptItem> = items
        .iter()
        .copied()
        .filter(|item| shows_at_top(item, &loaded) && Some(item.id) != sign_in_notice)
        .collect();
    let divider = actions::new_divider(&state, agent);
    let top_rows = tool_rows::rows(&top_items, &awaiting, divider);
    let states: Vec<Option<StepState>> = top_rows
        .iter()
        .map(|row| {
            let item = row.first();
            if Some(item.id) == sign_in_end {
                return Some(if waits_for_sign_in {
                    StepState::Waiting
                } else {
                    StepState::Plain
                });
            }
            match row {
                Row::Explored { .. } => Some(StepState::of_run(&row.items())),
                Row::Item(_) | Row::Skill { .. } => {
                    StepState::new(item, awaiting.contains(&item.id))
                }
            }
        })
        .collect();
    let is_step: Vec<bool> = states.iter().map(Option::is_some).collect();
    let links = step_links(&is_step, working);
    let working_linked = is_step.last().copied().unwrap_or(false);
    let firsts: Vec<&TranscriptItem> = top_rows.iter().map(Row::first).collect();
    let notes = older_step_notes(&firsts, &loaded);
    let scrolled_up = state.ui.chat_scrolled_up.get(&agent).copied();
    let row_ids: Vec<ItemId> = firsts.iter().map(|item| item.id).collect();
    let shown = ShownRows::of(agent, drawn(), &row_ids, scrolled_up.is_some());
    rows.set((Some(agent), row_ids));
    let first = shown.first(top_rows.len());
    let top: Vec<TopRow> = top_rows
        .iter()
        .zip(states)
        .zip(links)
        .zip(notes)
        .skip(first)
        .map(|(((row, state), links), older_step)| TopRow {
            id: row.first().id,
            new_above: divider
                .is_some_and(|divider| row.items().iter().any(|item| item.id == divider)),
            view: RowView::of(row, &children, &awaiting, sign_in_end),
            step: state.map(|state| Step { state, links }),
            older_step,
        })
        .collect();
    let history = entry.transcript.history().clone();
    // A run of new reads counts once, as it shows.
    let waiting = scrolled_up.map_or(0, |last_seen| {
        tool_rows::new_rows(&top_rows, actions::new_entries(entry, last_seen).first)
    });
    rsx! {
        div {
            id: CHAT_SCROLL_ID,
            class: "chat-scroll scroll",
            // The chat's scroll script keeps each agent's place. It goes back to that place once
            // the chat draws all the agent's rows.
            "data-agent": (shown == ShownRows::All).then(|| agent.get().to_string()),
            div { class: "chat-column",
                if shown == ShownRows::Skeleton {
                    TranscriptSkeleton {}
                } else if shown == ShownRows::All {
                    {history_note(history, cockpit, agent)}
                    if status == AgentStatus::Exited {
                        div { class: "exited-banner",
                            span { "The claude process ended. Resume starts it again on the same conversation." }
                            button {
                                class: "button primary",
                                onclick: move |_| cockpit.change(|_| actions::resume_agent(agent)),
                                "Resume"
                            }
                        }
                    }
                }
                // Item ids repeat across agents: with the agent in the key, another agent's item
                // never takes over the state of this one's (a fold, a typed answer).
                for TopRow { id, new_above, view, step, older_step } in top {
                    TranscriptRow { key: "{agent.get()}-{id.get()}", agent, folder: folder.clone(), new_above, id, view, step, older_step }
                }
                if working && shown != ShownRows::Skeleton {
                    WorkingLine { since: turn_start, linked: working_linked }
                }
            }
        }
        LastInstruction { top_entry }
        JumpToLatest { shown: scrolled_up.is_some(), count: waiting }
    }
}

/// What stands above the conversation's first loaded item: the skeleton while the first page
/// loads, the way to older messages, or why they did not load.
fn history_note(history: HistoryState, cockpit: Cockpit, agent: AgentId) -> Element {
    match history {
        HistoryState::NotLoaded | HistoryState::Loading => rsx! {
            TranscriptSkeleton {}
        },
        HistoryState::LoadingOlder => rsx! {
            p { class: "history-note loading-note", role: "status",
                span { class: "spinner", aria_hidden: true }
                "Loading older messages"
            }
        },
        HistoryState::Loaded { has_older: true } => rsx! {
            button {
                class: "link-button history-note",
                onclick: move |_| cockpit.change(|state| actions::load_older(state, agent)),
                "Load older messages"
            }
        },
        HistoryState::Loaded { has_older: false } => rsx! {},
        HistoryState::Failed(error) => rsx! {
            p { class: "history-note",
                "The conversation did not load: {error}. "
                button {
                    class: "link-button",
                    onclick: move |_| cockpit.update(|state| actions::retry_history(state, agent)),
                    "Retry"
                }
            }
        },
    }
}

/// What a row of the main conversation needs, as `TranscriptRow` takes it.
struct TopRow {
    /// The row's first item, which names its entry.
    id: ItemId,
    /// The "New" divider goes above the row: it holds the first new item.
    new_above: bool,
    view: RowView,
    step: Option<Step>,
    older_step: bool,
}

/// What a row of the main conversation shows.
#[derive(Debug, Clone, PartialEq)]
enum RowView {
    Item {
        item: TranscriptItem,
        nested: Vec<TranscriptItem>,
        awaits_approval: bool,
        /// The item is the end of a turn that found no login: it shows as the sign-in card.
        sign_in_card: bool,
    },
    Skill {
        item: TranscriptItem,
        /// The Markdown of the notice that follows the call.
        instructions: Option<String>,
        awaits_approval: bool,
    },
    Explored(Vec<TranscriptItem>),
}

impl RowView {
    fn of(
        row: &Row<'_>,
        children: &Children<'_>,
        awaiting: &BTreeSet<ItemId>,
        sign_in_end: Option<ItemId>,
    ) -> Self {
        match row {
            Row::Item(item) => Self::Item {
                item: (*item).clone(),
                nested: subtree(children, item.id),
                awaits_approval: awaiting.contains(&item.id),
                sign_in_card: Some(item.id) == sign_in_end,
            },
            Row::Skill { call, instructions } => Self::Skill {
                item: (*call).clone(),
                awaits_approval: awaiting.contains(&call.id),
                instructions: instructions.and_then(|notice| match &notice.body {
                    ItemBody::Notice { detail, .. } => detail.clone(),
                    _ => None,
                }),
            },
            Row::Explored { .. } => Self::Explored(row.items().into_iter().cloned().collect()),
        }
    }
}

/// Where a step of a turn stands, as its dot's color says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepState {
    /// Text and thinking, which neither succeed nor fail.
    Plain,
    Running,
    Done,
    Failed,
    /// A tool call, an approval, a question or a plan waits for an answer.
    Waiting,
}

impl StepState {
    /// `None` for what is not a step of the agent's turn: a user message, a notice, a turn's end.
    fn new(item: &TranscriptItem, awaits_approval: bool) -> Option<Self> {
        let state = match &item.body {
            ItemBody::AssistantText { .. } | ItemBody::Thinking { .. } => Self::Plain,
            ItemBody::ToolCall(call) => Self::from_stage(CallStage::new(call, awaits_approval)),
            ItemBody::Subagent(call) => Self::from_stage(CallStage::new(call, false)),
            ItemBody::Approval(_) | ItemBody::Question(_) | ItemBody::Plan(_) => {
                if waits_for_answer(item) {
                    Self::Waiting
                } else {
                    Self::Plain
                }
            }
            ItemBody::UserMessage(_) | ItemBody::TurnEnd(_) | ItemBody::Notice { .. } => {
                return None;
            }
        };
        Some(state)
    }

    /// A run of reads and searches: running while one runs, failed when one failed.
    fn of_run(calls: &[&TranscriptItem]) -> Self {
        let stages: Vec<Self> = calls
            .iter()
            .filter_map(|item| Self::new(item, false))
            .collect();
        if stages.contains(&Self::Running) {
            Self::Running
        } else if stages.contains(&Self::Failed) {
            Self::Failed
        } else {
            Self::Done
        }
    }

    fn from_stage(stage: CallStage) -> Self {
        match stage {
            CallStage::AwaitsApproval => Self::Waiting,
            CallStage::Running => Self::Running,
            CallStage::Done => Self::Done,
            CallStage::Failed => Self::Failed,
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Plain => "step-plain",
            Self::Running => "step-running",
            Self::Done => "step-done",
            Self::Failed => "step-failed",
            Self::Waiting => "step-waiting",
        }
    }
}

/// Whether the line that links a turn's steps comes down to a step's dot, and goes on below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StepLinks {
    above: bool,
    below: bool,
}

/// A step's dot, and the line that links it to the turn's other steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Step {
    state: StepState,
    links: StepLinks,
}

impl Step {
    fn class(self) -> String {
        let mut class = format!("step {}", self.state.class());
        if self.links.above {
            class.push_str(" linked-above");
        }
        if self.links.below {
            class.push_str(" linked-below");
        }
        class
    }
}

/// The links of each item, given which items are steps: a user message, a notice or a turn's end
/// breaks the line. With `working`, the last step links down to the working line.
fn step_links(is_step: &[bool], working: bool) -> Vec<StepLinks> {
    let mut links = Vec::with_capacity(is_step.len());
    for (index, &step) in is_step.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .and_then(|before| is_step.get(before))
            .copied()
            .unwrap_or(false);
        let next = is_step.get(index + 1).copied().unwrap_or(working);
        links.push(StepLinks {
            above: step && previous,
            below: step && next,
        });
    }
    links
}

/// The last step of a running turn: a spinner on the steps' line, and the time since the turn
/// started.
#[component]
fn WorkingLine(since: Option<TurnStart>, linked: bool) -> Element {
    let Clock(now) = use_context::<Clock>();
    let text = match since {
        Some(start) => format!("Working, {}", format::duration(start.elapsed(now()))),
        None => "Working".to_owned(),
    };
    rsx! {
        div { class: if linked { "step working linked-above" } else { "step working" },
            span { class: "step-marker",
                span { class: "spinner", role: "status", aria_label: "Working" }
            }
            "{text}"
        }
    }
}

/// Top-level items, the cards still waiting for an answer, even a subagent's, which a folded
/// subagent must not hide, and the items whose parent sits in older messages the window has not
/// loaded, which would show nowhere else.
fn shows_at_top(item: &TranscriptItem, loaded: &BTreeSet<ItemId>) -> bool {
    item.parent.is_none_or(|parent| !loaded.contains(&parent)) || waits_for_answer(item)
}

/// For each top item, whether a note goes above it: the item starts a run of items whose parent,
/// the step they belong to, is not loaded.
fn older_step_notes(top_items: &[&TranscriptItem], loaded: &BTreeSet<ItemId>) -> Vec<bool> {
    let mut notes = Vec::with_capacity(top_items.len());
    let mut run: Option<ItemId> = None;
    for item in top_items {
        let unloaded_parent = item.parent.filter(|parent| !loaded.contains(parent));
        notes.push(unloaded_parent.is_some() && unloaded_parent != run);
        run = unloaded_parent;
    }
    notes
}

pub(crate) fn waits_for_answer(item: &TranscriptItem) -> bool {
    match &item.body {
        ItemBody::Approval(approval) => approval.answer == AnswerState::Waiting,
        ItemBody::Question(question) => question.answer == AnswerState::Waiting,
        ItemBody::Plan(plan) => plan.answer == AnswerState::Waiting,
        _ => false,
    }
}

/// Each item's direct children. Built once per render, so that the subtrees cost one pass over
/// the transcript instead of one per item.
pub(crate) type Children<'item> = BTreeMap<ItemId, Vec<&'item TranscriptItem>>;

pub(crate) fn children_by_parent<'item>(items: &[&'item TranscriptItem]) -> Children<'item> {
    let mut children = Children::new();
    for item in items {
        if let Some(parent) = item.parent {
            children.entry(parent).or_default().push(item);
        }
    }
    children
}

/// Every item nested under `root`, at any depth, in transcript order. Each item comes once, even
/// when parents form a cycle.
pub(crate) fn subtree(children: &Children<'_>, root: ItemId) -> Vec<TranscriptItem> {
    let mut found: Vec<TranscriptItem> = Vec::new();
    let mut seen = BTreeSet::from([root]);
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            if seen.insert(child.id) {
                found.push((*child).clone());
                pending.push(child.id);
            }
        }
    }
    found.sort_by_key(|item| item.id);
    found
}

/// A row of the main conversation, on the steps' line when it is a step of a turn.
#[component]
fn TranscriptRow(
    agent: AgentId,
    folder: PathBuf,
    /// The "New" divider goes above the entry.
    new_above: bool,
    /// The row's first item, which names its entry.
    id: ItemId,
    view: RowView,
    step: Option<Step>,
    /// The item starts a run whose step sits in older messages the window has not loaded.
    older_step: bool,
) -> Element {
    let item_id = id;
    let id = chat_scroll::entry_id(agent, item_id);
    let view = match view {
        RowView::Item {
            sign_in_card: true, ..
        } => rsx! {
            SignInCard { agent }
        },
        RowView::Item {
            item,
            nested,
            awaits_approval,
            ..
        } => rsx! {
            ItemView { agent, folder, item, nested, awaits_approval }
        },
        RowView::Skill {
            item:
                TranscriptItem {
                    body: ItemBody::ToolCall(call),
                    ..
                },
            instructions,
            awaits_approval,
        } => rsx! {
            SkillView { agent, folder, id: item_id, stage: CallStage::new(&call, awaits_approval), call, instructions }
        },
        RowView::Skill { item, .. } => rsx! {
            ItemView { agent, folder, item, nested: Vec::new(), awaits_approval: false }
        },
        RowView::Explored(calls) => rsx! {
            ExploredView { agent, folder, calls }
        },
    };
    let row = match step {
        Some(step) => rsx! {
            div { class: step.class(),
                span { class: "step-dot", aria_hidden: true }
                {view}
            }
        },
        None => view,
    };
    rsx! {
        div { id, class: ENTRY_CLASS,
            if new_above {
                NewDivider {}
            }
            if older_step {
                p { class: "history-note", "Part of a step in older messages: load them to see it." }
            }
            {row}
        }
    }
}

#[component]
pub fn ItemView(
    agent: AgentId,
    folder: PathBuf,
    item: TranscriptItem,
    nested: Vec<TranscriptItem>,
    /// Whether the item is a running tool call whose approval card waits for an answer.
    awaits_approval: bool,
) -> Element {
    let id = item.id;
    match item.body {
        ItemBody::UserMessage(message) => rsx! {
            UserBubble { agent, folder, message }
        },
        ItemBody::AssistantText { markdown } => rsx! {
            MarkdownView { agent, folder, markdown }
        },
        ItemBody::Thinking { text } => rsx! {
            ThinkingView { text }
        },
        ItemBody::ToolCall(call) if call.edit.is_some() => rsx! {
            EditView { agent, folder, stage: CallStage::new(&call, awaits_approval), call }
        },
        ItemBody::ToolCall(call) => rsx! {
            ToolView { agent, id, stage: CallStage::new(&call, awaits_approval), call }
        },
        ItemBody::Subagent(call) => rsx! {
            SubagentView { agent, folder, id, call, nested }
        },
        ItemBody::Approval(approval) => rsx! {
            ApprovalCard { agent, folder, approval }
        },
        ItemBody::Question(question) => rsx! {
            QuestionCard { agent, item: id, question }
        },
        ItemBody::Plan(plan) => rsx! {
            PlanCard { agent, folder, plan }
        },
        ItemBody::TurnEnd(end) => rsx! {
            TurnEndView { end }
        },
        ItemBody::Notice {
            text,
            detail: Some(detail),
        } => rsx! {
            NoticeWithDetail { agent, folder, title: text, markdown: detail }
        },
        ItemBody::Notice { text, detail: None } => rsx! {
            p { class: "notice", "{text}" }
        },
    }
}

/// The user's message. Its text renders as Markdown, as the agent's text does. A long one keeps
/// its first screen lines until "Show more", as a command's output does.
#[component]
fn UserBubble(agent: AgentId, folder: PathBuf, message: UserMessage) -> Element {
    let mut unfolded = use_signal(|| false);
    let open = unfolded();
    let state = if open { " unfolded" } else { "" };
    rsx! {
        div { class: "user-message",
            for (index, image) in message.images.iter().enumerate() {
                img { key: "{index}", class: "message-image", src: bridge::image_url(image), alt: "Attached image" }
            }
            if !message.text.is_empty() {
                div {
                    class: "{USER_BUBBLE_CLASS} {MESSAGE_ROW_CLASS}{state}",
                    style: "{FOLD_LINES_PROPERTY}: {MESSAGE_FOLD_LINES}",
                    div { class: MESSAGE_CLIP_CLASS,
                        MarkdownView { agent, folder, markdown: message.text }
                    }
                    button {
                        class: FOLD_BUTTON_CLASS,
                        aria_expanded: open,
                        onclick: move |_| unfolded.toggle(),
                        span { class: FOLD_LABEL_CLASS }
                        span { class: "fold-less", "Show less" }
                    }
                }
            }
        }
    }
}

/// A notice that unfolds to the Markdown it titles, such as a skill's instructions, folded as
/// thinking is.
#[component]
fn NoticeWithDetail(agent: AgentId, folder: PathBuf, title: String, markdown: String) -> Element {
    let mut expanded = use_signal(|| false);
    let open = expanded();
    rsx! {
        div { class: "thinking notice-with-detail",
            button {
                class: "fold-button",
                aria_expanded: open,
                onclick: move |_| expanded.toggle(),
                span { class: "dim", "{title}" }
                span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
            }
            if open {
                div { class: "notice-detail",
                    MarkdownView { agent, folder, markdown }
                }
            }
        }
    }
}

#[component]
fn ThinkingView(text: String) -> Element {
    let mut expanded = use_signal(|| false);
    let open = expanded();
    rsx! {
        div { class: "thinking",
            button {
                class: "fold-button",
                aria_expanded: open,
                onclick: move |_| expanded.toggle(),
                span { class: "muted", "Thinking" }
                span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
            }
            if open {
                p { class: "thinking-text", "{text}" }
            }
        }
    }
}

/// Where a tool call stands, as its line says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallStage {
    /// Claude Code asked whether the call may run, and nobody answered yet.
    AwaitsApproval,
    Running,
    Done,
    Failed,
}

impl CallStage {
    pub(crate) fn new(call: &ToolCall, awaits_approval: bool) -> Self {
        match call.status {
            ToolCallStatus::Running if awaits_approval => Self::AwaitsApproval,
            ToolCallStatus::Running => Self::Running,
            ToolCallStatus::Done => Self::Done,
            ToolCallStatus::Failed => Self::Failed,
        }
    }

    /// The stage's class alone. A tool line's class adds it to "tool".
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::AwaitsApproval => "waiting",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn class(self) -> String {
        format!("tool {}", self.name())
    }
}

/// What a tool call's title line and its IN block show, by the kind of tool.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolShape {
    /// A shell command: the description Claude Code sends with it on the title line, the command
    /// in the IN block.
    Command {
        description: Option<String>,
        command: String,
    },
    /// An MCP tool, as "Search [find_pages]": its server and its name on the title line, its
    /// input as JSON in the IN block.
    Mcp {
        server: String,
        tool: String,
        input: Option<String>,
    },
    /// A tool that names a file, such as `Read`: its summary opens the file view, at the line
    /// the call starts at.
    File { path: PathBuf, line: Option<u32> },
    /// Any other tool: its name and summary (a path, a pattern, a URL) on the title line.
    Other,
}

impl ToolShape {
    fn new(call: &ToolCall) -> Self {
        if let Some(name) = call.tool.strip_prefix(MCP_TOOL_PREFIX) {
            let (server, tool) = name.split_once(MCP_TOOL_SEPARATOR).unwrap_or((name, ""));
            let input = match &call.input {
                Value::Object(fields) if !fields.is_empty() => {
                    serde_json::to_string_pretty(&call.input).ok()
                }
                _ => None,
            };
            return Self::Mcp {
                server: capitalized(server),
                tool: tool.to_owned(),
                input,
            };
        }
        let field = |name: &str| {
            call.input
                .get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let command = field(COMMAND_FIELD).or_else(|| {
            (call.tool == file_edits::BASH_TOOL && !call.summary.is_empty())
                .then(|| call.summary.clone())
        });
        if let Some(command) = command {
            return Self::Command {
                description: field(DESCRIPTION_FIELD),
                command,
            };
        }
        match file_edits::FILE_FIELDS.iter().find_map(|name| field(name)) {
            Some(path) => Self::File {
                path: PathBuf::from(path),
                line: call
                    .input
                    .get(OFFSET_FIELD)
                    .and_then(Value::as_u64)
                    .and_then(|line| u32::try_from(line).ok())
                    .filter(|&line| line > 0),
            },
            None => Self::Other,
        }
    }

    /// The text of the IN block.
    fn input(&self) -> Option<&str> {
        match self {
            Self::Command { command, .. } => Some(command),
            Self::Mcp { input, .. } => input.as_deref(),
            Self::File { .. } | Self::Other => None,
        }
    }
}

/// "search" reads "Search".
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Where a tool call's title line shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleLayout {
    /// Over the IN and OUT block, which holds the command.
    Full,
    /// Alone, in a subagent: the command takes the place of a missing description.
    Compact,
}

/// The tool's name in bold, then the description or the summary.
fn tool_title(agent: AgentId, call: &ToolCall, shape: &ToolShape, layout: TitleLayout) -> Element {
    match shape {
        ToolShape::Command {
            description: Some(description),
            ..
        } => rsx! {
            span { class: "tool-name", "{call.tool}" }
            span { class: "call-description", "{description}" }
        },
        ToolShape::Command {
            description: None, ..
        } if layout == TitleLayout::Full => rsx! {
            span { class: "tool-name", "{call.tool}" }
        },
        ToolShape::Mcp { server, tool, .. } => rsx! {
            span { class: "tool-name", "{server}" }
            if !tool.is_empty() {
                code { class: "tool-summary", "[{tool}]" }
            }
        },
        ToolShape::File { path, line } if !call.summary.is_empty() => {
            let (path, line) = (path.clone(), *line);
            rsx! {
                span { class: "tool-name", "{call.tool}" }
                FileLink { agent, path, line, text: call.summary.clone() }
            }
        }
        ToolShape::Command { .. } | ToolShape::File { .. } | ToolShape::Other => rsx! {
            span { class: "tool-name", "{call.tool}" }
            if !call.summary.is_empty() {
                code { class: "tool-summary", "{call.summary}" }
            }
        },
    }
}

/// A tool call's summary that opens its file in the file view.
#[component]
fn FileLink(agent: AgentId, path: PathBuf, line: Option<u32>, text: String) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "tool-summary path-link",
            title: "Open the file",
            onclick: move |_| {
                let path = path.clone();
                cockpit.update(|state| actions::open_file(state, agent, path, line));
            },
            "{text}"
        }
    }
}

/// A row of a tool call's framed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IoKind {
    /// The command or the input the call got.
    In,
    /// What it answered.
    Out,
}

impl IoKind {
    fn label(self) -> &'static str {
        match self {
            Self::In => "IN",
            Self::Out => "OUT",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::In => "io-in",
            Self::Out => "io-out",
        }
    }

    fn fold_lines(self) -> u8 {
        match self {
            Self::In => IN_FOLD_LINES,
            Self::Out => OUT_FOLD_LINES,
        }
    }
}

#[derive(Serialize)]
struct FoldClasses {
    row: &'static str,
    clip: &'static str,
    label: &'static str,
    last: &'static str,
    message_row: &'static str,
    message_clip: &'static str,
    markdown: &'static str,
    lines_property: &'static str,
}

const FOLD_CLASSES: FoldClasses = FoldClasses {
    row: IO_ROW_CLASS,
    clip: IO_CLIP_CLASS,
    label: FOLD_LABEL_CLASS,
    last: FOLD_LAST_CLASS,
    message_row: MESSAGE_ROW_CLASS,
    message_clip: MESSAGE_CLIP_CLASS,
    markdown: MARKDOWN_CLASS,
    lines_property: FOLD_LINES_PROPERTY,
};

fn start_fold_script() {
    let script = document::eval(FOLD_SCRIPT);
    if let Err(error) = script.send(FOLD_CLASSES) {
        tracing::warn!("cannot start the script that folds tool output and messages: {error}");
    }
}

/// Where a folded OUT row keeps its lines. A command's last lines say how it ended, but JSON's are
/// closing brackets.
fn out_fold(shape: &ToolShape, output: &str) -> FoldEnd {
    let json = output.trim_start().starts_with(JSON_STARTS);
    match shape {
        ToolShape::Command { .. } if !json => FoldEnd::Last,
        ToolShape::Command { .. }
        | ToolShape::Mcp { .. }
        | ToolShape::File { .. }
        | ToolShape::Other => FoldEnd::First,
    }
}

/// How a JSON document starts.
const JSON_STARTS: [char; 2] = ['{', '['];

/// Which screen lines a folded IN or OUT row keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldEnd {
    First,
    /// The last ones, under the button that shows the earlier ones.
    Last,
}

/// Folded, a row taller than its fold keeps its first or its last screen lines; the page script
/// decides which rows are taller. The fold stays as the user set it when the call's output
/// arrives. An OUT row shows a run of lines that say the same thing once, with its count.
#[component]
fn IoRow(kind: IoKind, text: String, fold: FoldEnd) -> Element {
    let mut unfolded = use_signal(|| false);
    let open = unfolded();
    let state = if open { " unfolded" } else { "" };
    let end = match fold {
        FoldEnd::First => "",
        FoldEnd::Last => FOLD_LAST_CLASS,
    };
    let button = rsx! {
        button {
            class: FOLD_BUTTON_CLASS,
            aria_expanded: open,
            onclick: move |_| unfolded.toggle(),
            span { class: FOLD_LABEL_CLASS }
            span { class: "fold-less", "Show less" }
        }
    };
    let parts = match kind {
        IoKind::In => vec![OutputPart::Lines(text)],
        IoKind::Out => render::output_parts(&text),
    };
    rsx! {
        div {
            class: "{IO_ROW_CLASS} {kind.class()} {end}{state}",
            style: "{FOLD_LINES_PROPERTY}: {kind.fold_lines()}",
            span { class: "io-label", "{kind.label()}" }
            div { class: "io-text",
                if fold == FoldEnd::Last {
                    {button.clone()}
                }
                div { class: IO_CLIP_CLASS,
                    OutputText { parts }
                }
                if fold == FoldEnd::First {
                    {button}
                }
            }
        }
    }
}

/// The text of an IN or OUT row. A run of repeated lines shows once, with a button that shows
/// each of its lines.
#[component]
fn OutputText(parts: Vec<OutputPart>) -> Element {
    let mut shown_runs = use_signal(BTreeSet::<usize>::new);
    let open_runs = shown_runs();
    rsx! {
        pre {
            for (index, part) in parts.into_iter().enumerate() {
                if index > 0 {
                    "\n"
                }
                match part {
                    OutputPart::Lines(text) => rsx! { "{text}" },
                    OutputPart::Repeated { shown, count, lines } => {
                        let open = open_runs.contains(&index);
                        rsx! {
                            if open { "{lines}" } else { "{shown}" }
                            button {
                                class: if open { "repeat-count open" } else { "repeat-count" },
                                aria_pressed: open,
                                title: if open { "Show these lines once" } else { "Show each of these lines" },
                                onclick: move |_| {
                                    shown_runs.with_mut(|runs| {
                                        if !runs.remove(&index) {
                                            runs.insert(index);
                                        }
                                    });
                                },
                                "× {count}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// "running, 1 min 4 s" while a call runs, counted from when the app saw it start, or "running"
/// for a call already running when the app connected. It draws again each second, apart from its
/// card.
#[component]
pub(crate) fn RunningMeta(agent: AgentId, item: ItemId) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context();
    // Read for its tick alone: the time counts from an `Instant`.
    let _ = now();
    let since = cockpit
        .state
        .peek()
        .agents
        .get(&agent)
        .and_then(|entry| entry.calls_running_since.get(&item).copied());
    let text = match since {
        Some(since) => format!(
            "{RUNNING_WORD}, {}",
            format::duration(Duration::from_secs(since.elapsed().as_secs()))
        ),
        None => RUNNING_WORD.to_owned(),
    };
    rsx! {
        span { class: "tool-meta", "{text}" }
    }
}

pub(crate) fn tool_meta(call: &ToolCall, stage: CallStage) -> String {
    match (stage, call.exit_code) {
        (CallStage::AwaitsApproval, _) => "waiting for approval".to_owned(),
        (CallStage::Running, _) => RUNNING_WORD.to_owned(),
        (_, Some(code)) => format!("exit {code}"),
        (CallStage::Failed, None)
            if call.tool == file_edits::READ_TOOL || tool_rows::is_skill(call) =>
        {
            call.output
                .as_deref()
                .and_then(error_line)
                .unwrap_or_else(|| "failed".to_owned())
        }
        (CallStage::Failed, None) => "failed".to_owned(),
        (CallStage::Done, None) => done_meta(call).unwrap_or_default(),
    }
}

/// What a finished `Read`, `Grep` or `Glob` returned, in a few words.
fn done_meta(call: &ToolCall) -> Option<String> {
    let output = call.output.as_deref()?;
    if call.tool == file_edits::READ_TOOL {
        let lines = render::read_lines(output)?;
        return Some(if lines.first == lines.last {
            format!("line {}", lines.first)
        } else {
            format!("lines {}–{}", lines.first, lines.last)
        });
    }
    let search: SearchTool = call.tool.parse().ok()?;
    Some(match render::found(search, output) {
        Found::Files(files) => format::count(files, "file", "files"),
        Found::Lines(lines) => format::count(lines, "line", "lines"),
        Found::Matches(matches) => format::count(matches, "match", "matches"),
        Found::Nothing => "no matches".to_owned(),
    })
}

/// The first line of a tool's error, without the tags Claude Code puts around it.
fn error_line(output: &str) -> Option<String> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line = line.strip_prefix(TOOL_ERROR_OPEN).unwrap_or(line);
    let line = line.strip_suffix(TOOL_ERROR_CLOSE).unwrap_or(line);
    Some(line.to_owned())
}

/// A tool whose output the chat leaves out once it ran: the file view shows what a `Read` read.
fn hides_output(call: &ToolCall, stage: CallStage) -> bool {
    call.tool == file_edits::READ_TOOL && matches!(stage, CallStage::Done | CallStage::Failed)
}

/// The end of an edit's line: what the edit did once it ran, where it stands before that.
#[derive(Debug, PartialEq, Eq)]
enum EditMeta {
    Stage(String),
    Created { lines: usize },
    Changed { added: usize, removed: usize },
}

impl EditMeta {
    fn new(call: &ToolCall, stage: CallStage, added: usize, removed: usize) -> Self {
        // A `Write` has no old text until it ran, even over a file: the daemon reads that text
        // from the result.
        let created = call
            .edit
            .as_ref()
            .is_some_and(|edit| edit.old_text.is_empty());
        match stage {
            CallStage::Done if created => Self::Created { lines: added },
            CallStage::Done => Self::Changed { added, removed },
            CallStage::AwaitsApproval | CallStage::Running | CallStage::Failed => {
                Self::Stage(tool_meta(call, stage))
            }
        }
    }
}

#[component]
fn ToolView(agent: AgentId, id: ItemId, call: ToolCall, stage: CallStage) -> Element {
    let shape = ToolShape::new(&call);
    let meta = tool_meta(&call, stage);
    let input = shape.input().map(str::to_owned);
    let output = call
        .output
        .clone()
        .filter(|output| !output.is_empty() && !hides_output(&call, stage));
    // A command's last lines say how it ended.
    let out_fold = out_fold(&shape, output.as_deref().unwrap_or_default());
    let framed = input.is_some() || output.is_some();
    rsx! {
        div { class: stage.class(),
            div { class: "tool-head",
                {tool_title(agent, &call, &shape, TitleLayout::Full)}
                if stage == CallStage::Running {
                    RunningMeta { agent, item: id }
                } else if !meta.is_empty() {
                    span { class: "tool-meta", "{meta}" }
                }
            }
            if framed {
                div { class: "tool-io",
                    if let Some(input) = input {
                        IoRow { kind: IoKind::In, text: input, fold: FoldEnd::First }
                    }
                    if let Some(output) = output {
                        IoRow { kind: IoKind::Out, text: output, fold: out_fold }
                    }
                }
            }
        }
    }
}

#[component]
fn EditView(agent: AgentId, folder: PathBuf, call: ToolCall, stage: CallStage) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(edit) = call.edit.clone() else {
        return rsx! {};
    };
    let language = Language::from_path(&edit.path);
    let Diff {
        rows: preview,
        more,
        added,
        removed,
    } = diff_lines(
        &edit.old_text,
        &edit.new_text,
        language,
        EDIT_CONTEXT_LINES,
        1,
        EDIT_PREVIEW_ROWS,
    );
    let numbers = if edit.old_text.is_empty() {
        LineNumbers::Shown
    } else {
        LineNumbers::Hidden
    };
    let shown_path = file_edits::shown_path(&folder, &edit.path);
    let path = edit.path.clone();
    let open = move |_: MouseEvent| {
        let path = path.clone();
        cockpit.update(|state| actions::open_file(state, agent, path, None));
    };
    let open_more = open.clone();
    let more_text = format!(
        "{} more, open in the file view",
        format::count(more, "line", "lines")
    );
    let meta = EditMeta::new(&call, stage, added, removed);
    rsx! {
        div { class: stage.class(),
            div { class: "tool-head",
                span { class: "tool-name", "{call.tool}" }
                button { class: "path-link mono", onclick: open, "{shown_path}" }
                match meta {
                    EditMeta::Stage(words) => rsx! {
                        span { class: "tool-meta", "{words}" }
                    },
                    EditMeta::Created { lines } => {
                        let lines = format::count(lines, "line", "lines");
                        rsx! {
                            span { class: "tool-meta", "Created, {lines}" }
                        }
                    }
                    EditMeta::Changed { added, removed } => rsx! {
                        span { class: "tool-meta mono",
                            span { class: "added", "+{added}" }
                            " "
                            span { class: "removed", "-{removed}" }
                        }
                    },
                }
            }
            if !preview.is_empty() {
                div { class: "diff-box inline-diff scroll-x",
                    div { class: "diff-rows",
                        for row in preview.iter() {
                            {diff_row(row, numbers, LineMarks::marked(None))}
                        }
                    }
                    if more > 0 {
                        button { class: "more-lines", onclick: open_more, "{more_text}" }
                    }
                }
            }
            if call.status == ToolCallStatus::Failed {
                if let Some(output) = call.output.clone() {
                    div { class: "tool-io",
                        IoRow { kind: IoKind::Out, text: output, fold: FoldEnd::First }
                    }
                }
            }
        }
    }
}

#[component]
fn SubagentView(
    agent: AgentId,
    folder: PathBuf,
    id: ItemId,
    call: ToolCall,
    nested: Vec<TranscriptItem>,
) -> Element {
    let mut expanded = use_signal(|| false);
    let open = expanded();
    let calls = nested
        .iter()
        .filter(|item| matches!(item.body, ItemBody::ToolCall(_) | ItemBody::Subagent(_)))
        .count();
    let calls_text = format::count(calls, "tool call", "tool calls");
    let meta = match call.status {
        ToolCallStatus::Running => format!("{RUNNING_WORD}, {calls_text}"),
        ToolCallStatus::Done => calls_text,
        ToolCallStatus::Failed => format!("failed, {calls_text}"),
    };
    let nested_refs: Vec<&TranscriptItem> = nested.iter().collect();
    let by_parent = children_by_parent(&nested_refs);
    let awaiting = state::awaiting_approval(&nested_refs);
    let children: Vec<(TranscriptItem, Vec<TranscriptItem>, bool)> = nested
        .iter()
        .filter(|item| item.parent == Some(id) && !waits_for_answer(item))
        .map(|item| {
            let grandchildren = subtree(&by_parent, item.id);
            (item.clone(), grandchildren, awaiting.contains(&item.id))
        })
        .collect();
    rsx! {
        div { class: CallStage::new(&call, false).class(),
            button {
                class: "fold-button tool-head",
                aria_expanded: open,
                onclick: move |_| expanded.toggle(),
                span { class: "tool-name", "Agent" }
                span { class: "subagent-title", "{call.summary}" }
                span { class: "tool-meta", "{meta}" }
                span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
            }
            if open {
                div { class: "subagent-body",
                    for (child, grandchildren, awaits_approval) in children {
                        CompactItem { key: "{child.id.get()}", agent, folder: folder.clone(), item: child, nested: grandchildren, awaits_approval }
                    }
                    if let Some(result) = &call.output {
                        p { class: "subagent-result", "{result}" }
                    }
                }
            }
        }
    }
}

/// A subagent's item: tool calls on one line, the rest as in the main conversation. A search's
/// line opens the list of what it found.
#[component]
pub(crate) fn CompactItem(
    agent: AgentId,
    folder: PathBuf,
    item: TranscriptItem,
    nested: Vec<TranscriptItem>,
    awaits_approval: bool,
) -> Element {
    let mut expanded = use_signal(|| false);
    match &item.body {
        ItemBody::ToolCall(call) => {
            let stage = CallStage::new(call, awaits_approval);
            let meta = tool_meta(call, stage);
            let found = call
                .output
                .clone()
                .filter(|output| !output.is_empty() && call.tool.parse::<SearchTool>().is_ok());
            if let Some(output) = found {
                let open = expanded();
                let title = tool_title(agent, call, &ToolShape::new(call), TitleLayout::Compact);
                return rsx! {
                    div { class: "compact-search",
                        button {
                            class: "fold-button compact-tool {stage.class()}",
                            aria_expanded: open,
                            onclick: move |_| expanded.toggle(),
                            span { class: "tool-dot", aria_hidden: true }
                            {title}
                            if !meta.is_empty() {
                                span { class: "tool-meta", "{meta}" }
                            }
                            span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
                        }
                        if open {
                            div { class: "tool-io",
                                IoRow { kind: IoKind::Out, text: output, fold: FoldEnd::First }
                            }
                        }
                    }
                };
            }
            let title = match &call.edit {
                Some(edit) => {
                    let path = file_edits::shown_path(&folder, &edit.path);
                    rsx! {
                        span { class: "tool-name", "{call.tool}" }
                        code { class: "tool-summary", "{path}" }
                    }
                }
                None => tool_title(agent, call, &ToolShape::new(call), TitleLayout::Compact),
            };
            rsx! {
                div { class: "compact-tool {stage.class()}",
                    span { class: "tool-dot", aria_hidden: true }
                    {title}
                    if stage == CallStage::Running {
                        RunningMeta { agent, item: item.id }
                    } else if !meta.is_empty() {
                        span { class: "tool-meta", "{meta}" }
                    }
                }
            }
        }
        _ => rsx! {
            ItemView { agent, folder, item: item.clone(), nested, awaits_approval }
        },
    }
}

#[component]
fn TurnEndView(end: TurnEnd) -> Element {
    let duration = format::duration(end.duration);
    let cost = format::cost(end.cost);
    let error = end.error.unwrap_or_default();
    rsx! {
        match end.outcome {
            TurnOutcome::Failed | TurnOutcome::NotSignedIn => rsx! {
                p { class: "turn-end failed", "The turn failed after {duration}: {error}" }
            },
            TurnOutcome::Stopped => rsx! {
                p { class: "turn-end", "Stopped after {duration}, {cost} at API prices" }
            },
            TurnOutcome::Completed => rsx! {
                p { class: "turn-end", "Turn done in {duration}, {cost} at API prices" }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Approval, ApprovalAnswer, ApprovalDecision, FileEdit, PendingId};

    use super::*;

    #[test]
    fn a_long_chat_draws_older_rows_in_chunks_up_to_all() {
        let agent = AgentId::new(7);
        let other = AgentId::new(8);
        // 100 rows, whose ids skip some numbers, as nested items leave gaps.
        let rows: Vec<ItemId> = (0..100).map(|row| ItemId::new(row * 2 + 1)).collect();
        let row = |index: usize| rows[index];

        let first = next_drawn(agent, None, &rows, false);
        let second = next_drawn(agent, first, &rows, false);
        let third = next_drawn(agent, second, &rows, false);

        // The opening frame drew the latest 25: rows 75 on.
        assert_eq!(first, Some(Drawn::From(agent, row(35))));
        assert_eq!(second, Some(Drawn::All(agent)));
        assert_eq!(third, None, "all drawn: nothing more to do");
        assert_eq!(
            next_drawn(agent, Some(Drawn::All(other)), &rows, false),
            first,
            "another agent's progress starts again"
        );
        assert_eq!(
            next_drawn(agent, None, &rows, true),
            Some(Drawn::All(agent)),
            "a chat scrolled up draws all at once"
        );
        assert_eq!(
            next_drawn(agent, None, &rows[..20], false),
            Some(Drawn::All(agent))
        );
    }

    #[test]
    fn a_scroll_up_while_the_chunks_come_keeps_the_rows_drawn() {
        let agent = AgentId::new(7);
        let rows: Vec<ItemId> = (0..100).map(ItemId::new).collect();
        let drawn = Some(Drawn::From(agent, ItemId::new(35)));

        assert_eq!(
            ShownRows::of(agent, drawn, &rows, false),
            ShownRows::Latest(35)
        );
        assert_eq!(
            ShownRows::of(agent, drawn, &rows, true),
            ShownRows::Latest(35),
            "scrolled up meanwhile: no skeleton over the rows"
        );
        assert_eq!(
            ShownRows::of(agent, None, &rows, true),
            ShownRows::Skeleton,
            "opening on a place scrolled up"
        );
    }

    fn notice(id: u32, parent: Option<u32>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body: ItemBody::Notice {
                text: String::new(),
                detail: None,
            },
        }
    }

    #[test]
    fn subtree_collects_every_depth_in_order() {
        let items = [
            notice(1, None),
            notice(2, Some(1)),
            notice(3, Some(2)),
            notice(4, None),
            notice(5, Some(1)),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        let ids: Vec<u32> = subtree(&children_by_parent(&refs), ItemId::new(1))
            .iter()
            .map(|item| item.id.get())
            .collect();

        assert_eq!(ids, [2, 3, 5]);
    }

    #[test]
    fn a_cycle_of_parents_ends() {
        let items = [notice(1, Some(2)), notice(2, Some(1)), notice(3, Some(3))];
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let children = children_by_parent(&refs);

        let ids = |root: u32| -> Vec<u32> {
            subtree(&children, ItemId::new(root))
                .iter()
                .map(|item| item.id.get())
                .collect()
        };

        assert_eq!(ids(1), [2]);
        assert!(ids(3).is_empty());
    }

    #[test]
    fn the_line_links_consecutive_steps_only() {
        let none = StepLinks::default();
        let links = |above, below| StepLinks { above, below };

        assert_eq!(
            step_links(&[false, true, true, true, false, true], false),
            [
                none,
                links(false, true),
                links(true, true),
                links(true, false),
                none,
                none
            ]
        );
        assert_eq!(step_links(&[false, true], true), [none, links(false, true)]);
        assert_eq!(step_links(&[true, false], true), [none, none]);
    }

    #[test]
    fn a_command_shows_its_description_and_its_command() {
        let with_input = |input: serde_json::Value| ToolCall {
            input,
            ..call("Bash", "cargo test", ToolCallStatus::Done)
        };

        assert_eq!(
            ToolShape::new(&with_input(serde_json::json!({
                "command": "cargo test\n--quiet",
                "description": "Run the tests",
            }))),
            ToolShape::Command {
                description: Some("Run the tests".to_owned()),
                command: "cargo test\n--quiet".to_owned(),
            }
        );
        assert_eq!(
            ToolShape::new(&with_input(serde_json::Value::Null)),
            ToolShape::Command {
                description: None,
                command: "cargo test".to_owned(),
            }
        );
        assert_eq!(
            ToolShape::new(&call("Read", "src/main.rs", ToolCallStatus::Done)),
            ToolShape::Other
        );
    }

    #[test]
    fn a_tool_that_names_a_file_links_to_it_at_its_first_line() {
        let read = |input: Value| {
            ToolShape::new(&ToolCall {
                input,
                ..call("Read", "src/main.rs", ToolCallStatus::Done)
            })
        };

        assert_eq!(
            read(serde_json::json!({"file_path": "/work/src/main.rs", "offset": 40})),
            ToolShape::File {
                path: PathBuf::from("/work/src/main.rs"),
                line: Some(40),
            }
        );
        assert_eq!(
            read(serde_json::json!({"file_path": "/work/src/main.rs", "offset": 0})),
            ToolShape::File {
                path: PathBuf::from("/work/src/main.rs"),
                line: None,
            }
        );
        assert_eq!(
            read(serde_json::json!({"notebook_path": "/work/plot.ipynb"})),
            ToolShape::File {
                path: PathBuf::from("/work/plot.ipynb"),
                line: None,
            }
        );
        assert_eq!(
            read(serde_json::json!({"file_path": "/work/shots/sheet05.JPG"})),
            ToolShape::File {
                path: PathBuf::from("/work/shots/sheet05.JPG"),
                line: None,
            }
        );
    }

    #[test]
    fn an_mcp_tool_shows_its_server_its_name_and_its_input() {
        let search = ToolCall {
            input: serde_json::json!({"query": "columns"}),
            ..call("mcp__search__find_pages", "columns", ToolCallStatus::Done)
        };
        let bare = call("mcp__search__list", "", ToolCallStatus::Done);

        assert_eq!(
            ToolShape::new(&search),
            ToolShape::Mcp {
                server: "Search".to_owned(),
                tool: "find_pages".to_owned(),
                input: Some("{\n  \"query\": \"columns\"\n}".to_owned()),
            }
        );
        assert_eq!(
            ToolShape::new(&bare),
            ToolShape::Mcp {
                server: "Search".to_owned(),
                tool: "list".to_owned(),
                input: None,
            }
        );
    }

    fn call(tool: &str, summary: &str, status: ToolCallStatus) -> ToolCall {
        ToolCall {
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input: serde_json::Value::Null,
            status,
            output: None,
            exit_code: None,
            edit: None,
        }
    }

    fn write_call(status: ToolCallStatus) -> ToolCall {
        ToolCall {
            edit: Some(FileEdit {
                path: PathBuf::from("/work/notes.txt"),
                old_text: String::new(),
                new_text: "one line\n".to_owned(),
            }),
            ..call("Write", "notes.txt", status)
        }
    }

    fn approval(tool: &str, summary: &str, answer: AnswerState<ApprovalAnswer>) -> ItemBody {
        ItemBody::Approval(Approval {
            pending: PendingId::new("request-1".to_owned()),
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input: serde_json::Value::Null,
            edit: None,
            reason: None,
            answer,
            tool_call: None,
        })
    }

    fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body,
        }
    }

    #[test]
    fn tool_meta_shows_the_stage_and_exit_code() {
        let failed = ToolCall {
            exit_code: Some(101),
            ..call("Bash", "cargo test", ToolCallStatus::Failed)
        };
        let meta = |status, awaits_approval| {
            let call = call("Bash", "cargo test", status);
            tool_meta(&call, CallStage::new(&call, awaits_approval))
        };

        assert_eq!(meta(ToolCallStatus::Running, true), "waiting for approval");
        assert_eq!(meta(ToolCallStatus::Running, false), "running");
        assert_eq!(tool_meta(&failed, CallStage::Failed), "exit 101");
        assert_eq!(meta(ToolCallStatus::Failed, false), "failed");
        assert_eq!(meta(ToolCallStatus::Done, false), "");
    }

    #[test]
    fn a_read_or_a_search_says_what_it_returned_on_its_line() {
        let meta = |tool: &str, status, output: &str| {
            let call = ToolCall {
                output: Some(output.to_owned()),
                ..call(tool, "src/lexer.rs", status)
            };
            tool_meta(&call, CallStage::new(&call, false))
        };
        let lines = "    40\tpub fn next_token() {\n   121\t}\n";
        let missing = "<tool_use_error>File does not exist.</tool_use_error>";

        assert_eq!(meta("Read", ToolCallStatus::Done, lines), "lines 40–121");
        assert_eq!(meta("Read", ToolCallStatus::Done, "     7→}"), "line 7");
        assert_eq!(
            meta("Read", ToolCallStatus::Failed, missing),
            "File does not exist."
        );
        assert_eq!(
            meta("Grep", ToolCallStatus::Done, "Found 4 files\na\nb\nc\nd"),
            "4 files"
        );
        assert_eq!(
            meta("Glob", ToolCallStatus::Done, "No files found"),
            "no matches"
        );
        assert_eq!(meta("Bash", ToolCallStatus::Done, "ok"), "");
    }

    #[test]
    fn a_failed_skill_says_why_on_its_line() {
        let failed = ToolCall {
            output: Some("<tool_use_error>Unknown skill: frobnicate</tool_use_error>".to_owned()),
            ..call("Skill", "frobnicate", ToolCallStatus::Failed)
        };

        assert_eq!(
            tool_meta(&failed, CallStage::Failed),
            "Unknown skill: frobnicate"
        );
    }

    #[test]
    fn a_skill_row_keeps_its_wait_for_approval() {
        let skill = item(
            1,
            None,
            ItemBody::ToolCall(call("Skill", "review", ToolCallStatus::Running)),
        );
        let awaiting = BTreeSet::from([ItemId::new(1)]);
        let row = Row::Skill {
            call: &skill,
            instructions: None,
        };

        let view = RowView::of(&row, &Children::new(), &awaiting, None);

        assert!(matches!(
            view,
            RowView::Skill {
                awaits_approval: true,
                ..
            }
        ));
    }

    #[test]
    fn a_command_keeps_its_last_lines_unless_it_answers_json() {
        let command = ToolShape::Command {
            description: None,
            command: "cargo test".to_owned(),
        };

        assert_eq!(out_fold(&command, "running 4 tests\nok"), FoldEnd::Last);
        assert_eq!(
            out_fold(&command, "  [\n  {\"case\": 1}\n]"),
            FoldEnd::First
        );
        assert_eq!(out_fold(&command, "{\"ok\": true}"), FoldEnd::First);
        assert_eq!(out_fold(&ToolShape::Other, "a\nb"), FoldEnd::First);
    }

    #[test]
    fn only_a_read_that_ran_leaves_its_output_out() {
        let read = call("Read", "src/lexer.rs", ToolCallStatus::Done);
        let bash = call("Bash", "cargo test", ToolCallStatus::Done);

        assert!(hides_output(&read, CallStage::Done));
        assert!(hides_output(&read, CallStage::Failed));
        assert!(!hides_output(&read, CallStage::Running));
        assert!(!hides_output(&bash, CallStage::Done));
    }

    #[test]
    fn an_edit_shows_its_result_only_once_it_ran() {
        let meta = |status, awaits_approval| {
            let call = write_call(status);
            EditMeta::new(&call, CallStage::new(&call, awaits_approval), 1, 0)
        };

        assert_eq!(
            meta(ToolCallStatus::Running, true),
            EditMeta::Stage("waiting for approval".to_owned())
        );
        assert_eq!(
            meta(ToolCallStatus::Running, false),
            EditMeta::Stage("running".to_owned())
        );
        assert_eq!(
            meta(ToolCallStatus::Failed, false),
            EditMeta::Stage("failed".to_owned())
        );
        assert_eq!(
            meta(ToolCallStatus::Done, false),
            EditMeta::Created { lines: 1 }
        );

        let mut edit = write_call(ToolCallStatus::Done);
        if let Some(file) = &mut edit.edit {
            file.old_text = "old line\n".to_owned();
        }
        assert_eq!(
            EditMeta::new(&edit, CallStage::Done, 1, 1),
            EditMeta::Changed {
                added: 1,
                removed: 1
            }
        );
    }

    /// A subagent's items whose call sits in an older page show at the top level, one note above
    /// each run of them.
    #[test]
    fn items_whose_parent_is_not_loaded_show_at_the_top_under_one_note_per_run() {
        let bash = || ItemBody::ToolCall(call("Bash", "ls", ToolCallStatus::Done));
        let items = [
            item(10, Some(5), bash()),
            item(11, Some(5), bash()),
            item(12, Some(10), bash()),
            item(13, None, bash()),
            item(14, Some(6), bash()),
            item(15, Some(5), bash()),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let loaded: BTreeSet<ItemId> = items.iter().map(|item| item.id).collect();

        let top: Vec<&TranscriptItem> = refs
            .iter()
            .copied()
            .filter(|item| shows_at_top(item, &loaded))
            .collect();
        let ids: Vec<u32> = top.iter().map(|item| item.id.get()).collect();

        assert_eq!(ids, [10, 11, 13, 14, 15]);
        assert_eq!(
            older_step_notes(&top, &loaded),
            [true, false, false, true, true]
        );
    }

    #[test]
    fn a_step_takes_its_dot_from_its_call_or_its_answer() {
        let step =
            |body: ItemBody, awaits_approval| StepState::new(&item(1, None, body), awaits_approval);
        let bash = |status| ItemBody::ToolCall(call("Bash", "cargo test", status));
        let allowed = AnswerState::Answered(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        });
        let message = ItemBody::UserMessage(UserMessage {
            text: "Run the tests".to_owned(),
            images: Vec::new(),
        });

        assert_eq!(
            step(bash(ToolCallStatus::Running), true),
            Some(StepState::Waiting)
        );
        assert_eq!(
            step(bash(ToolCallStatus::Running), false),
            Some(StepState::Running)
        );
        assert_eq!(
            step(bash(ToolCallStatus::Failed), false),
            Some(StepState::Failed)
        );
        assert_eq!(
            step(approval("Bash", "ls", AnswerState::Waiting), false),
            Some(StepState::Waiting)
        );
        assert_eq!(
            step(approval("Bash", "ls", allowed), false),
            Some(StepState::Plain)
        );
        assert_eq!(
            step(
                ItemBody::AssistantText {
                    markdown: "Done.".to_owned()
                },
                false
            ),
            Some(StepState::Plain)
        );
        assert_eq!(step(message, false), None);
        assert_eq!(
            step(
                ItemBody::Notice {
                    text: String::new(),
                    detail: None
                },
                false
            ),
            None
        );
    }

    #[test]
    fn an_opening_chat_draws_its_latest_rows_or_the_skeleton_over_a_place_scrolled_up() {
        let short = ShownRows::opening(FIRST_ROWS, true);
        let long = ShownRows::opening(140, false);
        let scrolled_up = ShownRows::opening(140, true);

        assert_eq!(short, ShownRows::All);
        assert_eq!(long, ShownRows::Latest(140 - FIRST_ROWS));
        assert_eq!(scrolled_up, ShownRows::Skeleton);
        assert_eq!(ShownRows::All.first(140), 0);
        assert_eq!(long.first(140), 140 - FIRST_ROWS);
        assert_eq!(scrolled_up.first(140), 140);
    }

    /// The fold script reads these fields by name: a rename in Rust would make it read
    /// `undefined`, with no error.
    #[test]
    fn the_fields_the_fold_script_reads_are_pinned() {
        assert_eq!(
            serde_json::to_value(FOLD_CLASSES).unwrap(),
            serde_json::json!({
                "row": "io-row",
                "clip": "io-clip",
                "label": "fold-more",
                "last": "fold-last",
                "message_row": "message-fold",
                "message_clip": "message-clip",
                "markdown": "markdown",
                "lines_property": "--fold-lines",
            })
        );
    }
}

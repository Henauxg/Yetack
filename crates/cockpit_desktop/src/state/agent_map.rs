//! The agent map: an outline of the selected agent and every subagent it started, folded so that
//! many subagents stay readable, and the keys that move in it.
//!
//! - The outline shows the current turn's subagents, and those of earlier turns that still run
//!   or hold one that does. The other earlier ones fold into one row.
//! - A subagent whose subtree holds no running or failed subagent starts folded.
//! - Past `SIBLING_CAP` siblings, the ended ones that did not fail fold into one row.
//! - The filter shows only the running and failed subagents, with the ones above them.

use std::collections::{BTreeMap, BTreeSet};

use cockpit_protocol::{
    AgentDetails, AgentId, AgentStatus, AgentSummary, ApprovalDecision, ItemId, PlanDecision,
    Subagent, SubagentRun, SubagentStatus,
};

use super::actions;
use super::app_state::AppState;
use super::requests::Outgoing;
use super::shortcuts::{KEY_DOWN, KEY_ENTER, KEY_LEFT, KEY_NO, KEY_RIGHT, KEY_UP, KEY_YES};
use super::transcript::{Transcript, Waiting, WaitingKind};
use super::ui_state::{Dialog, RevealPlace};

/// Siblings a level shows before its ended ones fold into one row.
pub const SIBLING_CAP: usize = 8;

/// The keys the page's script passes on while the map is open.
pub const MAP_KEYS: [&str; 5] = [KEY_UP, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_ENTER];

/// A list of siblings in the outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// The subagents the agent started in the current turn, and those of earlier turns that
    /// still run.
    Turn,
    /// The subagents the agent started in earlier turns, which have ended.
    Earlier,
    /// The subagents a subagent started.
    Under(ItemId),
}

/// A row of the outline, which the selection names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MapRow {
    Agent,
    Subagent(ItemId),
    /// The earlier turns' subagents, folded into one row.
    Earlier,
    /// The ended subagents of a level past the cap, folded into one row.
    More(Level),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MapTab {
    #[default]
    Transcript,
    Prompt,
    ToolCalls,
}

/// The map dialog's view of one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapView {
    pub agent: AgentId,
    pub selected: MapRow,
    /// The subagents the user folded (`true`) or unfolded (`false`), over the default.
    pub folds: BTreeMap<ItemId, bool>,
    pub earlier_open: bool,
    /// The levels whose ended subagents all show, past the cap.
    pub levels_open: BTreeSet<Level>,
    /// Shows only the running and failed subagents, with the ones above them.
    pub only_live: bool,
    pub tab: MapTab,
}

impl MapView {
    pub fn new(agent: AgentId) -> Self {
        Self {
            agent,
            selected: MapRow::Agent,
            folds: BTreeMap::new(),
            earlier_open: false,
            levels_open: BTreeSet::new(),
            only_live: false,
            tab: MapTab::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind<'list> {
    Agent,
    Subagent(&'list Subagent),
    Earlier {
        turns: usize,
        subagents: usize,
    },
    More {
        count: usize,
        /// Every folded subagent is done: none was stopped.
        all_done: bool,
    },
}

/// A row as the outline shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlineRow<'list> {
    pub row: MapRow,
    pub kind: RowKind<'list>,
    /// 0 for the agent.
    pub depth: u16,
    /// `None` for a row with nothing under it.
    pub folded: Option<bool>,
    /// The row Left goes to.
    pub parent: MapRow,
}

/// The turn the map calls current: the one running, or the last one.
pub fn current_turn(summary: &AgentSummary) -> u32 {
    match summary.status {
        AgentStatus::Working | AgentStatus::NeedsYou => summary.turns.saturating_add(1),
        AgentStatus::Idle | AgentStatus::Exited => summary.turns,
    }
}

/// Whether the subagent shows among the current turn's.
pub fn in_current_turn(subagent: &Subagent, turn: u32) -> bool {
    subagent.turn >= turn || subagent.status.is_running()
}

/// The subagents by the subagent that started them. A subagent whose parent the list lacks shows
/// at the top.
struct Tree<'list> {
    children: BTreeMap<Option<ItemId>, Vec<&'list Subagent>>,
    /// The subagents whose subtree holds a running or failed subagent, themselves included.
    lively: BTreeSet<ItemId>,
    /// The subagents whose subtree holds a running subagent, themselves included.
    running: BTreeSet<ItemId>,
}

impl<'list> Tree<'list> {
    fn new(subagents: &'list [Subagent]) -> Self {
        let by_item: BTreeMap<ItemId, &Subagent> = subagents
            .iter()
            .map(|subagent| (subagent.item, subagent))
            .collect();
        let mut children: BTreeMap<Option<ItemId>, Vec<&Subagent>> = BTreeMap::new();
        for subagent in subagents {
            let parent = subagent
                .parent
                .filter(|parent| *parent != subagent.item && by_item.contains_key(parent));
            children.entry(parent).or_default().push(subagent);
        }
        Self {
            children,
            lively: with_ancestors(subagents, &by_item, is_lively),
            running: with_ancestors(subagents, &by_item, |subagent| subagent.status.is_running()),
        }
    }

    fn children(&self, parent: Option<ItemId>) -> &[&'list Subagent] {
        self.children.get(&parent).map_or(&[], Vec::as_slice)
    }

    /// Whether a subagent the agent started shows among the current turn's: one of an earlier
    /// turn does while a subagent under it runs.
    fn in_turn(&self, subagent: &Subagent, turn: u32) -> bool {
        in_current_turn(subagent, turn) || self.running.contains(&subagent.item)
    }
}

/// The subagents that `pick` takes, and every subagent above them.
fn with_ancestors(
    subagents: &[Subagent],
    by_item: &BTreeMap<ItemId, &Subagent>,
    pick: impl Fn(&Subagent) -> bool,
) -> BTreeSet<ItemId> {
    let mut found = BTreeSet::new();
    let picked = subagents.iter().filter(|subagent| pick(subagent));
    for subagent in picked {
        // Bounded by the list's length, so that parents in a cycle end the walk.
        let mut next = Some(subagent);
        for _ in 0..subagents.len() {
            let Some(current) = next else {
                break;
            };
            if !found.insert(current.item) {
                break;
            }
            next = current
                .parent
                .and_then(|parent| by_item.get(&parent).copied());
        }
    }
    found
}

fn is_lively(subagent: &Subagent) -> bool {
    subagent.status.is_running() || subagent.status == SubagentStatus::Failed
}

/// The outline's rows, top to bottom.
pub fn outline<'list>(
    subagents: &'list [Subagent],
    turn: u32,
    view: &MapView,
) -> Vec<OutlineRow<'list>> {
    let tree = Tree::new(subagents);
    let builder = Builder { tree: &tree, view };
    let mut rows = vec![OutlineRow {
        row: MapRow::Agent,
        kind: RowKind::Agent,
        depth: 0,
        folded: None,
        parent: MapRow::Agent,
    }];
    let (current, earlier): (Vec<&Subagent>, Vec<&Subagent>) = builder
        .shown(tree.children(None))
        .into_iter()
        .partition(|subagent| tree.in_turn(subagent, turn));
    builder.push_level(&mut rows, Level::Turn, &current, 1, MapRow::Agent);
    if !earlier.is_empty() {
        let turns: BTreeSet<u32> = earlier.iter().map(|subagent| subagent.turn).collect();
        rows.push(OutlineRow {
            row: MapRow::Earlier,
            kind: RowKind::Earlier {
                turns: turns.len(),
                subagents: earlier.len(),
            },
            depth: 1,
            folded: Some(!view.earlier_open),
            parent: MapRow::Agent,
        });
        if view.earlier_open {
            builder.push_level(&mut rows, Level::Earlier, &earlier, 2, MapRow::Earlier);
        }
    }
    rows
}

struct Builder<'tree, 'list> {
    tree: &'tree Tree<'list>,
    view: &'tree MapView,
}

impl<'list> Builder<'_, 'list> {
    /// The subagents the filter lets through.
    fn shown(&self, subagents: &[&'list Subagent]) -> Vec<&'list Subagent> {
        subagents
            .iter()
            .copied()
            .filter(|subagent| !self.view.only_live || self.tree.lively.contains(&subagent.item))
            .collect()
    }

    fn push_level(
        &self,
        rows: &mut Vec<OutlineRow<'list>>,
        level: Level,
        siblings: &[&'list Subagent],
        depth: u16,
        parent: MapRow,
    ) {
        let folded = folded_siblings(siblings, self.view.levels_open.contains(&level));
        for subagent in siblings {
            if !folded.contains(&subagent.item) {
                self.push_subagent(rows, subagent, depth, parent);
            }
        }
        if !folded.is_empty() {
            let all_done = siblings
                .iter()
                .filter(|subagent| folded.contains(&subagent.item))
                .all(|subagent| subagent.status == SubagentStatus::Done);
            rows.push(OutlineRow {
                row: MapRow::More(level),
                kind: RowKind::More {
                    count: folded.len(),
                    all_done,
                },
                depth,
                folded: Some(true),
                parent,
            });
        }
    }

    fn push_subagent(
        &self,
        rows: &mut Vec<OutlineRow<'list>>,
        subagent: &'list Subagent,
        depth: u16,
        parent: MapRow,
    ) {
        let children = self.shown(self.tree.children(Some(subagent.item)));
        let folded = (!children.is_empty()).then(|| {
            self.view
                .folds
                .get(&subagent.item)
                .copied()
                .unwrap_or(!self.tree.lively.contains(&subagent.item))
        });
        let row = MapRow::Subagent(subagent.item);
        rows.push(OutlineRow {
            row,
            kind: RowKind::Subagent(subagent),
            depth,
            folded,
            parent,
        });
        if folded == Some(false) {
            let level = Level::Under(subagent.item);
            self.push_level(rows, level, &children, depth.saturating_add(1), row);
        }
    }
}

/// The siblings that fold into a "more done" row: past the cap, the oldest ended ones that did
/// not fail, so that the running, the failed and the latest ended ones fill the cap.
fn folded_siblings(siblings: &[&Subagent], open: bool) -> BTreeSet<ItemId> {
    if open || siblings.len() <= SIBLING_CAP {
        return BTreeSet::new();
    }
    let foldable: Vec<ItemId> = siblings
        .iter()
        .filter(|subagent| !is_lively(subagent))
        .map(|subagent| subagent.item)
        .collect();
    let kept = siblings.len() - foldable.len();
    let room = SIBLING_CAP.saturating_sub(kept);
    let folded = foldable.len().saturating_sub(room);
    // A single folded subagent shows as itself: its row takes no more room than "1 more".
    if folded <= 1 {
        return BTreeSet::new();
    }
    foldable.into_iter().take(folded).collect()
}

/// The subagents of the current turn, at any depth, and those still running.
pub fn turn_subagents(subagents: &[Subagent], turn: u32) -> Vec<&Subagent> {
    let by_item: BTreeMap<ItemId, &Subagent> = subagents
        .iter()
        .map(|subagent| (subagent.item, subagent))
        .collect();
    subagents
        .iter()
        .filter(|subagent| {
            let top = top_ancestor(subagent, &by_item);
            in_current_turn(top, turn) || subagent.status.is_running()
        })
        .collect()
}

fn top_ancestor<'list>(
    subagent: &'list Subagent,
    by_item: &BTreeMap<ItemId, &'list Subagent>,
) -> &'list Subagent {
    let mut top = subagent;
    for _ in 0..by_item.len() {
        match top.parent.and_then(|parent| by_item.get(&parent)) {
            Some(parent) if parent.item != top.item => top = parent,
            Some(_) | None => break,
        }
    }
    top
}

/// The tokens of the subagents that report theirs; `None` when none does.
pub fn total_tokens<'list>(subagents: impl IntoIterator<Item = &'list Subagent>) -> Option<u64> {
    subagents
        .into_iter()
        .filter_map(|subagent| subagent.tokens)
        .reduce(u64::saturating_add)
}

/// The status the composer's map button shows: waiting for you first, then running.
pub fn live_status(details: &AgentDetails) -> Option<SubagentStatus> {
    let statuses = || details.subagents.iter().map(|subagent| subagent.status);
    if statuses().any(|status| status == SubagentStatus::NeedsYou) {
        Some(SubagentStatus::NeedsYou)
    } else if statuses().any(|status| status == SubagentStatus::Running) {
        Some(SubagentStatus::Running)
    } else {
        None
    }
}

/// What Stop can do for a subagent that runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    /// Its task stops, and so do the tasks of the subagents under it.
    Task,
    /// Claude Code stops a foreground subagent without a task only with the agent's turn.
    Turn,
}

pub fn stop_kind(subagent: &Subagent) -> Option<StopKind> {
    if !subagent.status.is_running() {
        return None;
    }
    match (&subagent.task, subagent.run) {
        (Some(_), _) => Some(StopKind::Task),
        (None, SubagentRun::Foreground) => Some(StopKind::Turn),
        (None, SubagentRun::Background) => None,
    }
}

/// Stops the subagent of `item` and the running subagents under it.
pub fn stop(state: &AppState, agent: AgentId, item: ItemId) -> Vec<Outgoing> {
    let Some(subagents) = state
        .agents
        .get(&agent)
        .and_then(|entry| entry.details.loaded())
        .map(|details| details.subagents.as_slice())
    else {
        return Vec::new();
    };
    let Some(subagent) = subagents.iter().find(|subagent| subagent.item == item) else {
        return Vec::new();
    };
    match stop_kind(subagent) {
        Some(StopKind::Task) => {
            let mut requests = Vec::new();
            for running in with_running_descendants(subagents, item) {
                if let Some(task) = &running.task {
                    requests.extend(actions::stop_task(agent, task.clone()));
                }
            }
            requests
        }
        Some(StopKind::Turn) => actions::interrupt(state, agent),
        None => Vec::new(),
    }
}

/// The subagent of `item`, then the running subagents under it, at any depth.
fn with_running_descendants(subagents: &[Subagent], item: ItemId) -> Vec<&Subagent> {
    let mut found: Vec<&Subagent> = subagents
        .iter()
        .filter(|subagent| subagent.item == item)
        .collect();
    let mut seen = BTreeSet::from([item]);
    let mut pending = vec![item];
    while let Some(parent) = pending.pop() {
        for child in subagents {
            if child.parent == Some(parent) && seen.insert(child.item) {
                pending.push(child.item);
                if child.status.is_running() {
                    found.push(child);
                }
            }
        }
    }
    found
}

/// Ctrl+M: opens the map of the selected agent, or closes it. The new agent and "Add an account"
/// dialogs stay open, with what their fields hold, and so does an "Accounts" dialog that goes
/// back to the new agent dialog.
pub fn toggle(state: &mut AppState) {
    if state.ui.holds_new_agent_form() {
        return;
    }
    match state.ui.dialog {
        Some(Dialog::AgentMap(_)) => {
            state.ui.dialog = None;
            return;
        }
        Some(Dialog::NewAgent(_) | Dialog::AddAccount(_) | Dialog::Machines(_)) => return,
        Some(
            Dialog::Switcher
            | Dialog::OpenFile(_)
            | Dialog::CloseAgent(_)
            | Dialog::Accounts(_)
            | Dialog::NoDaemon
            | Dialog::DaemonUpdate(_),
        )
        | None => {}
    }
    if let Some(agent) = state.ui.selected {
        open_on_agent(state, agent);
    }
}

/// Opens the map of `agent` on the agent's own row.
pub fn open_on_agent(state: &mut AppState, agent: AgentId) {
    state.ui.popover = None;
    state.ui.badge = None;
    state.ui.dialog = Some(Dialog::AgentMap(MapView::new(agent)));
}

/// Opens the map on one subagent, unfolding what hides it.
pub fn open_on(state: &mut AppState, agent: AgentId, item: ItemId) {
    let mut view = MapView::new(agent);
    if let Some(entry) = state.agents.get(&agent)
        && let Some(details) = entry.details.loaded()
    {
        reveal(
            &mut view,
            &details.subagents,
            current_turn(&entry.summary),
            item,
        );
    }
    state.ui.popover = None;
    state.ui.badge = None;
    state.ui.dialog = Some(Dialog::AgentMap(view));
}

/// Selects the subagent of `item`, and unfolds the rows above it and the filter that hide it.
pub fn reveal(view: &mut MapView, subagents: &[Subagent], turn: u32, item: ItemId) {
    let by_item: BTreeMap<ItemId, &Subagent> = subagents
        .iter()
        .map(|subagent| (subagent.item, subagent))
        .collect();
    let Some(&target) = by_item.get(&item) else {
        return;
    };
    let tree = Tree::new(subagents);
    if !tree.lively.contains(&item) {
        view.only_live = false;
    }
    // The cap folds only subagents that neither run nor failed, nor hold one that does.
    let foldable = |subagent: &Subagent| !tree.lively.contains(&subagent.item);
    let mut current = target;
    for _ in 0..subagents.len() {
        match current.parent.and_then(|parent| by_item.get(&parent)) {
            Some(&parent) if parent.item != current.item => {
                view.folds.insert(parent.item, false);
                if foldable(current) {
                    view.levels_open.insert(Level::Under(parent.item));
                }
                current = parent;
            }
            Some(_) | None => break,
        }
    }
    let level = if tree.in_turn(current, turn) {
        Level::Turn
    } else {
        view.earlier_open = true;
        Level::Earlier
    };
    if foldable(current) {
        view.levels_open.insert(level);
    }
    view.selected = MapRow::Subagent(item);
}

/// What a change to the open map's view sees: the agent's subagents, the current turn, and the
/// outline before the change.
pub struct MapContext<'list> {
    pub subagents: &'list [Subagent],
    pub turn: u32,
    pub rows: Vec<OutlineRow<'list>>,
}

/// Changes the open map's view, then moves a selection the change hid.
fn change_view(state: &mut AppState, change: impl FnOnce(&mut MapView, &MapContext<'_>)) {
    let AppState { ui, agents, .. } = state;
    let Some(Dialog::AgentMap(view)) = &mut ui.dialog else {
        return;
    };
    let Some(entry) = agents.get(&view.agent) else {
        return;
    };
    let subagents: &[Subagent] = entry
        .details
        .loaded()
        .map_or(&[], |details| details.subagents.as_slice());
    let turn = current_turn(&entry.summary);
    let context = MapContext {
        subagents,
        turn,
        rows: outline(subagents, turn, view),
    };
    let index = context.rows.iter().position(|row| row.row == view.selected);
    change(view, &context);
    let rows = outline(subagents, turn, view);
    keep_selection_visible(view, &rows, subagents, index);
}

/// A key the page's script passes on while the map is open: a key that moves or folds, or Y
/// and N, which answer the selected subagent's approval or plan.
pub fn press(state: &mut AppState, key: &str) -> Vec<Outgoing> {
    if MAP_KEYS.contains(&key) {
        change_view(state, |view, context| apply_key(view, &context.rows, key));
        return Vec::new();
    }
    let (Some(Dialog::AgentMap(view)), Some(waiting)) = (&state.ui.dialog, selected_waiting(state))
    else {
        return Vec::new();
    };
    let agent = view.agent;
    match (waiting.kind, key.to_lowercase().as_str()) {
        (WaitingKind::Approval, KEY_YES) => {
            actions::answer_approval(agent, waiting.pending, ApprovalDecision::Allow)
        }
        (WaitingKind::Approval, KEY_NO) => {
            actions::answer_approval(agent, waiting.pending, ApprovalDecision::Deny)
        }
        (WaitingKind::Plan, KEY_YES) => {
            actions::answer_plan(agent, waiting.pending, PlanDecision::AcceptEdits)
        }
        (WaitingKind::Plan, KEY_NO) => {
            actions::answer_plan(agent, waiting.pending, PlanDecision::KeepPlanning)
        }
        _ => Vec::new(),
    }
}

/// The approval, question or plan of the selected subagent that waits for you.
pub fn selected_waiting(state: &AppState) -> Option<Waiting> {
    let Some(Dialog::AgentMap(view)) = &state.ui.dialog else {
        return None;
    };
    let MapRow::Subagent(item) = view.selected else {
        return None;
    };
    state
        .agents
        .get(&view.agent)?
        .transcript
        .waiting_under(item)
}

/// Moves the selection, or folds and unfolds, as Up, Down, Left, Right or Enter asks.
pub fn apply_key(view: &mut MapView, rows: &[OutlineRow<'_>], key: &str) {
    let Some(index) = rows.iter().position(|row| row.row == view.selected) else {
        view.selected = MapRow::Agent;
        return;
    };
    let Some(row) = rows.get(index) else {
        return;
    };
    let next = rows.get(index.saturating_add(1));
    match key {
        KEY_UP => {
            if let Some(previous) = index.checked_sub(1).and_then(|before| rows.get(before)) {
                view.selected = previous.row;
            }
        }
        KEY_DOWN => {
            if let Some(next) = next {
                view.selected = next.row;
            }
        }
        KEY_LEFT => {
            if row.folded == Some(false) {
                set_folded(view, row.row, true);
            } else {
                view.selected = row.parent;
            }
        }
        KEY_RIGHT => match row.folded {
            Some(true) => set_folded(view, row.row, false),
            Some(false) | None => {
                if let Some(child) = next.filter(|next| next.depth > row.depth) {
                    view.selected = child.row;
                }
            }
        },
        KEY_ENTER => {
            if let Some(folded) = row.folded {
                set_folded(view, row.row, !folded);
            }
        }
        _ => {}
    }
}

/// Folds or unfolds a row: a subagent, the earlier turns, or the ended subagents of a level.
pub fn set_folded(view: &mut MapView, row: MapRow, folded: bool) {
    match row {
        MapRow::Subagent(item) => {
            view.folds.insert(item, folded);
        }
        MapRow::Earlier => view.earlier_open = !folded,
        MapRow::More(level) => {
            if folded {
                view.levels_open.remove(&level);
            } else {
                view.levels_open.insert(level);
            }
        }
        MapRow::Agent => {}
    }
}

/// After a fold or a filter, a selection the outline no longer shows moves up to the nearest row
/// that shows it: the subagent above it, the row its level folded into, or the earlier turns'
/// row. A group row that unfolded leaves its place, `index`, to the row that takes it.
pub fn keep_selection_visible(
    view: &mut MapView,
    rows: &[OutlineRow<'_>],
    subagents: &[Subagent],
    index: Option<usize>,
) {
    let shows = |row: MapRow| rows.iter().any(|shown| shown.row == row);
    if shows(view.selected) {
        return;
    }
    let MapRow::Subagent(item) = view.selected else {
        let in_place = index.and_then(|index| rows.get(index)).map(|row| row.row);
        view.selected = in_place.unwrap_or(MapRow::Agent);
        return;
    };
    let by_item: BTreeMap<ItemId, &Subagent> = subagents
        .iter()
        .map(|subagent| (subagent.item, subagent))
        .collect();
    let mut current = by_item.get(&item).copied();
    // Bounded by the list's length, so that parents in a cycle end the walk.
    for _ in 0..=subagents.len() {
        let Some(subagent) = current else {
            break;
        };
        let parent = subagent
            .parent
            .filter(|parent| by_item.contains_key(parent));
        let candidates = match parent {
            Some(parent) => vec![
                MapRow::Subagent(subagent.item),
                MapRow::More(Level::Under(parent)),
            ],
            None => vec![
                MapRow::Subagent(subagent.item),
                MapRow::More(Level::Turn),
                MapRow::More(Level::Earlier),
                MapRow::Earlier,
            ],
        };
        if let Some(found) = candidates.into_iter().find(|row| shows(*row)) {
            view.selected = found;
            return;
        }
        current = parent.and_then(|parent| by_item.get(&parent).copied());
    }
    view.selected = MapRow::Agent;
}

/// Selects a row clicked in the outline. A click on a folded group row unfolds it.
pub fn click_row(state: &mut AppState, row: MapRow) {
    change_view(state, |view, _| {
        view.selected = row;
        if matches!(row, MapRow::More(_)) || (row == MapRow::Earlier && !view.earlier_open) {
            set_folded(view, row, false);
        }
    });
}

/// Folds or unfolds a row from its chevron.
pub fn toggle_fold(state: &mut AppState, row: MapRow, folded: bool) {
    change_view(state, |view, _| set_folded(view, row, !folded));
}

/// Shows only the running and failed subagents, or all of them again.
pub fn toggle_only_live(state: &mut AppState) {
    change_view(state, |view, _| view.only_live = !view.only_live);
}

pub fn set_tab(state: &mut AppState, tab: MapTab) {
    change_view(state, |view, _| view.tab = tab);
}

/// Selects a subagent from a link in the details, such as the one that started it.
pub fn select_subagent(state: &mut AppState, item: ItemId) {
    change_view(state, |view, context| {
        reveal(view, context.subagents, context.turn, item);
    });
}

/// "Show its call in the chat": closes the map, and scrolls the agent's chat to the entry that
/// holds the call of `item`'s subagent, which it highlights for a moment. A call the app does not
/// hold changes nothing.
pub fn show_call(state: &mut AppState, item: ItemId) {
    let Some(Dialog::AgentMap(view)) = &state.ui.dialog else {
        return;
    };
    let agent = view.agent;
    let Some(entry) = state
        .agents
        .get(&agent)
        .and_then(|entry| chat_entry(&entry.transcript, item))
    else {
        return;
    };
    state.ui.dialog = None;
    if state.ui.selected != Some(agent) {
        actions::select_agent(state, agent);
    }
    state.ui.reveal_in_chat(agent, entry, RevealPlace::Middle);
}

/// The top-level item the chat shows `item` in: `item` itself, or the loaded item it nests under,
/// at any depth.
fn chat_entry(transcript: &Transcript, item: ItemId) -> Option<ItemId> {
    let mut current = transcript.get(item)?;
    // Bounded by the transcript's length, so that parents in a cycle end the walk.
    for _ in 0..transcript.items().count() {
        match current.parent.and_then(|parent| transcript.get(parent)) {
            Some(parent) if parent.id != current.id => current = parent,
            Some(_) | None => break,
        }
    }
    Some(current.id)
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        BackgroundTaskId, ItemBody, Request, Timestamp, ToolCall, ToolCallStatus, TranscriptItem,
    };
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::app_state::{AgentEntry, Loadable};
    use crate::state::drafts::Drafts;

    const TURN: u32 = 3;

    fn subagent(item: u32, parent: Option<u32>, status: SubagentStatus) -> Subagent {
        Subagent {
            item: ItemId::new(item),
            parent: parent.map(ItemId::new),
            description: format!("Subagent {item}"),
            agent_type: None,
            run: SubagentRun::Foreground,
            status,
            started_at: Timestamp::from_unix_millis(u64::from(item) * 1000),
            ended_at: None,
            tokens: None,
            tool_calls: 0,
            last_tool: None,
            error: None,
            turn: TURN,
            task: None,
        }
    }

    fn in_turn(turn: u32, subagent: Subagent) -> Subagent {
        Subagent { turn, ..subagent }
    }

    fn view() -> MapView {
        MapView::new(AgentId::new(1))
    }

    /// Each row as text: its indent, then the subagent's item, `agent`, `earlier` or `more`.
    fn shape(subagents: &[Subagent], view: &MapView) -> Vec<String> {
        outline(subagents, TURN, view)
            .iter()
            .map(|row| {
                let name = match row.kind {
                    RowKind::Agent => "agent".to_owned(),
                    RowKind::Subagent(subagent) => subagent.item.get().to_string(),
                    RowKind::Earlier { turns, subagents } => format!("earlier {turns}/{subagents}"),
                    RowKind::More { count, all_done } => {
                        format!("more {count}{}", if all_done { " done" } else { "" })
                    }
                };
                let fold = match row.folded {
                    Some(true) => "+",
                    Some(false) => "-",
                    None => "",
                };
                format!("{}{fold}{name}", " ".repeat(usize::from(row.depth)))
            })
            .collect()
    }

    use SubagentStatus::{Done, Failed, NeedsYou, Running, Stopped};

    #[test]
    fn subagents_nest_under_the_one_that_started_them() {
        let subagents = [
            subagent(1, None, Running),
            subagent(2, Some(1), Running),
            subagent(3, None, Done),
            // Its parent left the list: it shows at the top.
            subagent(4, Some(99), Running),
        ];

        assert_eq!(
            shape(&subagents, &view()),
            ["agent", " -1", "  2", " 3", " 4"]
        );
    }

    #[test]
    fn an_ended_subtree_folds_by_itself_and_a_failed_or_running_one_stays_open() {
        let subagents = [
            subagent(1, None, Done),
            subagent(2, Some(1), Done),
            subagent(3, None, Done),
            subagent(4, Some(3), Failed),
            subagent(5, None, Stopped),
            subagent(6, Some(5), NeedsYou),
        ];

        assert_eq!(
            shape(&subagents, &view()),
            ["agent", " +1", " -3", "  4", " -5", "  6"]
        );

        let mut unfolded = view();
        unfolded.folds.insert(ItemId::new(1), false);
        unfolded.folds.insert(ItemId::new(3), true);
        assert_eq!(
            shape(&subagents, &unfolded),
            ["agent", " -1", "  2", " +3", " -5", "  6"]
        );
    }

    #[test]
    fn an_earlier_subagent_whose_subagent_still_runs_shows_in_the_turn() {
        let subagents = [
            in_turn(1, subagent(1, None, Done)),
            in_turn(1, subagent(2, Some(1), Running)),
            in_turn(1, subagent(3, None, Done)),
        ];
        let mut view = view();

        assert_eq!(
            shape(&subagents, &view),
            ["agent", " -1", "  2", " +earlier 1/1"]
        );

        reveal(&mut view, &subagents, TURN, ItemId::new(2));
        assert!(!view.earlier_open, "it is not among the earlier ones");
        assert_eq!(view.selected, row(2));
    }

    #[test]
    fn earlier_turns_fold_into_one_row_that_opens() {
        let subagents = [
            in_turn(1, subagent(1, None, Done)),
            in_turn(2, subagent(2, None, Failed)),
            in_turn(1, subagent(3, None, Running)),
            subagent(4, None, Done),
        ];
        let mut view = view();

        assert_eq!(
            shape(&subagents, &view),
            ["agent", " 3", " 4", " +earlier 2/2"]
        );

        view.earlier_open = true;
        assert_eq!(
            shape(&subagents, &view),
            ["agent", " 3", " 4", " -earlier 2/2", "  1", "  2"]
        );
    }

    #[test]
    fn past_the_cap_the_oldest_ended_siblings_fold_into_one_row() {
        let mut subagents: Vec<Subagent> =
            (1..=12).map(|item| subagent(item, None, Done)).collect();
        subagents[0].status = Failed;
        subagents[5].status = Running;
        subagents[6].status = Stopped;
        let mut view = view();

        // The failed and the running ones stay, and the six latest ended ones fill the cap.
        assert_eq!(
            shape(&subagents, &view),
            [
                "agent",
                " 1",
                " 6",
                " 7",
                " 8",
                " 9",
                " 10",
                " 11",
                " 12",
                " +more 4 done"
            ]
        );

        subagents[1].status = Stopped;
        assert_eq!(shape(&subagents, &view).last().unwrap(), " +more 4");

        view.levels_open.insert(Level::Turn);
        assert_eq!(shape(&subagents, &view).len(), 13);
    }

    #[test]
    fn one_ended_sibling_over_the_cap_shows_as_itself() {
        let subagents: Vec<Subagent> = (1..=9).map(|item| subagent(item, None, Done)).collect();

        assert_eq!(shape(&subagents, &view()).len(), 10);
    }

    #[test]
    fn the_filter_keeps_running_and_failed_ones_and_the_ones_above_them() {
        let subagents = [
            subagent(1, None, Done),
            subagent(2, Some(1), Running),
            subagent(3, Some(1), Done),
            subagent(4, None, Done),
            subagent(5, None, Failed),
        ];
        let mut view = view();
        view.only_live = true;

        assert_eq!(shape(&subagents, &view), ["agent", " -1", "  2", " 5"]);
    }

    #[test]
    fn the_turn_counts_its_subagents_at_any_depth_and_their_tokens() {
        let mut subagents = vec![
            in_turn(1, subagent(1, None, Done)),
            subagent(2, None, Done),
            in_turn(1, subagent(3, Some(2), Done)),
        ];
        subagents[0].tokens = Some(5_000);
        subagents[1].tokens = Some(1_200);

        let in_turn: Vec<u32> = turn_subagents(&subagents, TURN)
            .iter()
            .map(|subagent| subagent.item.get())
            .collect();

        assert_eq!(in_turn, [2, 3]);
        assert_eq!(total_tokens(turn_subagents(&subagents, TURN)), Some(1_200));
        assert_eq!(total_tokens(&subagents[2..]), None, "no usage: no number");
    }

    fn map_state(subagents: Vec<Subagent>) -> AppState {
        let mut summary = summary(1, AgentStatus::Working);
        summary.turns = TURN - 1;
        let mut state = state_with(vec![summary]);
        state.ui.selected = Some(AgentId::new(1));
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.details = Loadable::Loaded(AgentDetails {
                subagents,
                ..AgentDetails::default()
            });
        }
        toggle(&mut state);
        state
    }

    fn selected(state: &AppState) -> MapRow {
        match &state.ui.dialog {
            Some(Dialog::AgentMap(view)) => view.selected,
            other => panic!("the map is not open: {other:?}"),
        }
    }

    fn row(item: u32) -> MapRow {
        MapRow::Subagent(ItemId::new(item))
    }

    #[test]
    fn keys_move_fold_and_unfold() {
        let mut state = map_state(vec![
            subagent(1, None, Running),
            subagent(2, Some(1), Running),
            subagent(3, None, Done),
        ]);
        assert_eq!(selected(&state), MapRow::Agent);

        press(&mut state, KEY_DOWN);
        assert_eq!(selected(&state), row(1));
        press(&mut state, KEY_RIGHT);
        assert_eq!(
            selected(&state),
            row(2),
            "an open row goes to its first child"
        );
        press(&mut state, KEY_LEFT);
        assert_eq!(
            selected(&state),
            row(1),
            "a row without children goes to its parent"
        );
        press(&mut state, KEY_LEFT);
        assert_eq!(selected(&state), row(1), "an open row folds first");
        press(&mut state, KEY_DOWN);
        assert_eq!(selected(&state), row(3), "the folded child is skipped");
        press(&mut state, KEY_UP);
        press(&mut state, KEY_RIGHT);
        press(&mut state, KEY_DOWN);
        assert_eq!(selected(&state), row(2), "Right unfolded it");
        press(&mut state, KEY_LEFT);
        press(&mut state, KEY_LEFT);
        press(&mut state, KEY_LEFT);
        assert_eq!(selected(&state), MapRow::Agent);
        press(&mut state, KEY_UP);
        assert_eq!(selected(&state), MapRow::Agent, "the top stays");
    }

    #[test]
    fn enter_opens_a_group_row_and_the_selection_takes_its_place() {
        let mut state = map_state(vec![
            in_turn(1, subagent(1, None, Done)),
            subagent(2, None, Running),
        ]);
        press(&mut state, KEY_DOWN);
        press(&mut state, KEY_DOWN);
        assert_eq!(selected(&state), MapRow::Earlier);

        press(&mut state, KEY_ENTER);
        press(&mut state, KEY_DOWN);

        assert_eq!(selected(&state), row(1));
    }

    #[test]
    fn a_fold_moves_a_selection_it_hides_to_the_row_above() {
        let mut state = map_state(vec![
            subagent(1, None, Running),
            subagent(2, Some(1), Running),
            subagent(3, Some(2), Running),
        ]);
        select_subagent(&mut state, ItemId::new(3));

        toggle_fold(&mut state, row(1), false);

        assert_eq!(selected(&state), row(1));
    }

    #[test]
    fn the_filter_moves_a_selection_it_hides_to_the_agent() {
        let mut state = map_state(vec![subagent(1, None, Done), subagent(2, None, Running)]);
        select_subagent(&mut state, ItemId::new(1));

        toggle_only_live(&mut state);

        assert_eq!(selected(&state), MapRow::Agent);
    }

    #[test]
    fn opening_on_a_hidden_subagent_unfolds_what_hides_it() {
        let mut subagents: Vec<Subagent> = (1..=12)
            .map(|item| in_turn(1, subagent(item, None, Done)))
            .collect();
        subagents.push(in_turn(1, subagent(20, Some(1), Done)));
        let mut state = map_state(subagents);
        state.ui.dialog = None;

        open_on(&mut state, AgentId::new(1), ItemId::new(20));

        let Some(Dialog::AgentMap(view)) = &state.ui.dialog else {
            panic!("the map is not open");
        };
        let entry = &state.agents[&AgentId::new(1)];
        let subagents = &entry.details.loaded().unwrap().subagents;
        let rows = outline(subagents, TURN, view);
        assert!(rows.iter().any(|shown| shown.row == row(20)));
        assert_eq!(view.selected, row(20));
    }

    #[test]
    fn opening_on_a_running_subagent_keeps_the_ended_ones_folded() {
        let mut subagents: Vec<Subagent> =
            (1..=12).map(|item| subagent(item, None, Done)).collect();
        subagents[11].status = Running;
        let mut view = view();

        reveal(&mut view, &subagents, TURN, ItemId::new(12));

        assert_eq!(shape(&subagents, &view).last().unwrap(), " +more 4 done");
        assert_eq!(view.selected, row(12));
    }

    fn agent_call(item: u32, parent: Option<u32>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(item),
            parent: parent.map(ItemId::new),
            body: ItemBody::Subagent(ToolCall {
                tool: "Agent".to_owned(),
                summary: format!("Subagent {item}"),
                input: Value::Null,
                status: ToolCallStatus::Running,
                output: None,
                exit_code: None,
                edit: None,
            }),
        }
    }

    #[test]
    fn showing_a_call_closes_the_map_and_reveals_the_entry_that_holds_it() {
        let mut state = map_state(vec![
            subagent(1, None, Running),
            subagent(2, Some(1), Running),
            subagent(3, None, Done),
        ]);
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.transcript.append(agent_call(1, None));
            entry.transcript.append(agent_call(2, Some(1)));
        }

        show_call(&mut state, ItemId::new(1));

        assert_eq!(state.ui.dialog, None);
        let first = state.ui.chat_reveal.unwrap();
        assert_eq!((first.agent, first.item), (AgentId::new(1), ItemId::new(1)));

        // A call nested in another subagent's shows in that subagent's entry.
        toggle(&mut state);
        show_call(&mut state, ItemId::new(2));

        let second = state.ui.chat_reveal.unwrap();
        assert_eq!(second.item, ItemId::new(1));
        assert_ne!(second.serial, first.serial, "the same entry scrolls again");

        // A call older than the loaded messages: the map stays.
        toggle(&mut state);
        show_call(&mut state, ItemId::new(3));

        assert!(matches!(state.ui.dialog, Some(Dialog::AgentMap(_))));
        assert_eq!(state.ui.chat_reveal, Some(second));
    }

    #[test]
    fn ctrl_m_leaves_the_new_agent_dialog_open() {
        let mut state = map_state(Vec::new());
        let mut drafts = Drafts::default();
        state.ui.dialog = None;
        actions::open_new_agent(&mut state, &mut drafts);
        let form = state.ui.dialog.clone();

        toggle(&mut state);

        assert!(matches!(form, Some(Dialog::NewAgent(_))));
        assert_eq!(state.ui.dialog, form);
    }

    #[test]
    fn opening_another_agent_closes_the_map_of_the_last() {
        let mut state = map_state(Vec::new());
        state.agents.insert(
            AgentId::new(2),
            AgentEntry::new(summary(2, AgentStatus::Idle)),
        );

        actions::select_agent(&mut state, AgentId::new(1));
        assert!(matches!(state.ui.dialog, Some(Dialog::AgentMap(_))));
        actions::select_agent(&mut state, AgentId::new(2));

        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn ctrl_m_closes_the_map_it_opened() {
        let mut state = map_state(Vec::new());
        assert!(matches!(state.ui.dialog, Some(Dialog::AgentMap(_))));

        toggle(&mut state);

        assert_eq!(state.ui.dialog, None);
    }

    fn stopped_tasks(requests: &[Outgoing]) -> Vec<&str> {
        requests
            .iter()
            .filter_map(|outgoing| match &outgoing.request {
                Request::AgentStopTask(params) => Some(params.task.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn stop_stops_the_task_and_the_running_tasks_under_it() {
        let with_task = |mut subagent: Subagent, task: &str| {
            subagent.task = Some(BackgroundTaskId::new(task.to_owned()));
            subagent.run = SubagentRun::Background;
            subagent
        };
        let state = map_state(vec![
            with_task(subagent(1, None, Running), "task-1"),
            with_task(subagent(2, Some(1), NeedsYou), "task-2"),
            with_task(subagent(3, Some(1), Done), "task-3"),
            with_task(subagent(4, Some(2), Running), "task-4"),
            subagent(5, Some(1), Running),
        ]);

        let requests = stop(&state, AgentId::new(1), ItemId::new(1));

        assert_eq!(stopped_tasks(&requests), ["task-1", "task-2", "task-4"]);
    }

    #[test]
    fn stop_without_a_task_stops_the_turn_for_a_foreground_subagent_only() {
        let mut background = subagent(2, None, Running);
        background.run = SubagentRun::Background;
        let state = map_state(vec![subagent(1, None, Running), background]);

        let foreground = stop(&state, AgentId::new(1), ItemId::new(1));
        let unstoppable = stop(&state, AgentId::new(1), ItemId::new(2));

        assert!(matches!(
            foreground.as_slice(),
            [Outgoing {
                request: Request::AgentInterrupt(_),
                ..
            }]
        ));
        assert!(unstoppable.is_empty());
        assert_eq!(
            stop_kind(
                &state.agents[&AgentId::new(1)]
                    .details
                    .loaded()
                    .unwrap()
                    .subagents[1]
            ),
            None
        );
    }

    #[test]
    fn the_live_dot_shows_waiting_first_then_running() {
        let details = |statuses: &[SubagentStatus]| AgentDetails {
            subagents: (1_u32..)
                .zip(statuses)
                .map(|(item, &status)| subagent(item, None, status))
                .collect(),
            ..AgentDetails::default()
        };

        assert_eq!(
            live_status(&details(&[Done, Running, NeedsYou])),
            Some(NeedsYou)
        );
        assert_eq!(live_status(&details(&[Done, Running])), Some(Running));
        assert_eq!(live_status(&details(&[Done, Failed])), None);
    }

    #[test]
    fn the_current_turn_is_the_running_one_or_the_last() {
        let mut working = summary(1, AgentStatus::Working);
        working.turns = 4;
        let idle = AgentSummary {
            status: AgentStatus::Idle,
            ..working.clone()
        };

        assert_eq!(current_turn(&working), 5);
        assert_eq!(current_turn(&idle), 4);
    }
}

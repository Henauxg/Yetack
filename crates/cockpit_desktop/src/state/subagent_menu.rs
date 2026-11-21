//! The right-click menu of a subagent, on its row in the agent map's outline or in the chat's
//! Running block: show its call in the chat, open the map on it, copy its prompt or its result,
//! or stop it.

use cockpit_protocol::{AgentId, ItemBody, ItemId, SubagentStatus, ToolCall};
use serde_json::Value;

use super::agent_map::{self, MapMenu, StopKind};
use super::app_state::AppState;
use super::ui_state::{Dialog, Popover};

/// The input field of an `Agent` call that holds its prompt.
const PROMPT_FIELD: &str = "prompt";

/// The prompt an `Agent` call sent its subagent.
pub fn prompt(call: &ToolCall) -> Option<&str> {
    call.input.get(PROMPT_FIELD).and_then(Value::as_str)
}

/// Where the menu opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOrigin {
    /// A row of the agent map's outline.
    Map,
    /// A row of the chat's Running block, outside the map.
    Block,
}

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentItem {
    ShowInChat,
    OpenInMap,
    CopyPrompt,
    CopyResult,
    Stop(StopKind),
}

impl SubagentItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::ShowInChat => "Show its call in the chat",
            Self::OpenInMap => "Show it in the agent map",
            Self::CopyPrompt => "Copy its prompt",
            Self::CopyResult => "Copy its result",
            Self::Stop(StopKind::Task) => "Stop",
            Self::Stop(StopKind::Turn) => "Stop the turn",
        }
    }

    /// The letter that picks it while the menu is open.
    pub fn key(self) -> char {
        match self {
            Self::ShowInChat => 'c',
            Self::OpenInMap => 'm',
            Self::CopyPrompt => 'p',
            Self::CopyResult => 'r',
            Self::Stop(_) => 's',
        }
    }

    /// What the row does, or why it cannot run.
    pub fn title(self, enabled: bool) -> &'static str {
        match self {
            Self::ShowInChat | Self::CopyPrompt if !enabled => {
                "Its call is older than the part of the chat the app holds"
            }
            Self::CopyResult if !enabled => {
                "Its result is not in the part of the chat the app holds"
            }
            Self::Stop(StopKind::Task) => "Stop this subagent and the subagents it started",
            Self::Stop(StopKind::Turn) => {
                "Claude Code stops a foreground subagent only with the agent's turn"
            }
            Self::ShowInChat | Self::OpenInMap | Self::CopyPrompt | Self::CopyResult => "",
        }
    }

    /// A line divides it from the row above.
    pub fn divided(self) -> bool {
        matches!(self, Self::CopyPrompt | Self::Stop(_))
    }
}

/// A row of the menu, and whether it can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentRow {
    pub item: SubagentItem,
    pub enabled: bool,
}

/// What the open menu is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentTarget {
    pub agent: AgentId,
    /// The subagent's call.
    pub item: ItemId,
    pub x: i32,
    pub y: i32,
    pub origin: MenuOrigin,
    pub prompt: Option<String>,
    pub result: Option<String>,
    pub rows: Vec<SubagentRow>,
}

/// Opens the menu of the subagent of `item` at `x`, `y`, from its row in the map's outline.
pub fn open_in_map(state: &mut AppState, item: ItemId, x: i32, y: i32) {
    if let Some(Dialog::AgentMap(view)) = &mut state.ui.dialog {
        view.menu = Some(MapMenu { item, x, y });
    }
}

/// Opens the menu of `agent`'s subagent of `item` at `x`, `y`, from its row in the Running block.
pub fn open_in_block(state: &mut AppState, agent: AgentId, item: ItemId, x: i32, y: i32) {
    state.ui.popover = Some(Popover::SubagentMenu { agent, item, x, y });
}

/// Closes the menu, wherever it opened.
pub fn close(state: &mut AppState) {
    if let Some(Dialog::AgentMap(view)) = &mut state.ui.dialog {
        view.menu = None;
    }
    if matches!(state.ui.popover, Some(Popover::SubagentMenu { .. })) {
        state.ui.popover = None;
    }
}

/// The menu open in the map, with its rows.
pub fn map_target(state: &AppState) -> Option<SubagentTarget> {
    let Some(Dialog::AgentMap(view)) = &state.ui.dialog else {
        return None;
    };
    let MapMenu { item, x, y } = view.menu?;
    target(state, view.agent, item, x, y, MenuOrigin::Map)
}

/// The menu open in the Running block, with its rows.
pub fn block_target(state: &AppState) -> Option<SubagentTarget> {
    let Some(Popover::SubagentMenu { agent, item, x, y }) = state.ui.popover else {
        return None;
    };
    target(state, agent, item, x, y, MenuOrigin::Block)
}

fn target(
    state: &AppState,
    agent: AgentId,
    item: ItemId,
    x: i32,
    y: i32,
    origin: MenuOrigin,
) -> Option<SubagentTarget> {
    let entry = state.agents.get(&agent)?;
    let subagent = entry
        .details
        .loaded()?
        .subagents
        .iter()
        .find(|subagent| subagent.item == item)?;
    let call: Option<&ToolCall> = entry
        .transcript
        .get(item)
        .and_then(|held| match &held.body {
            ItemBody::Subagent(call) => Some(call),
            _ => None,
        });
    let prompt: Option<String> = call.and_then(prompt).map(str::to_owned);
    let result: Option<String> = call
        .and_then(|call| call.output.clone())
        .filter(|_| subagent.status == SubagentStatus::Done);
    let row = |item: SubagentItem, enabled: bool| SubagentRow { item, enabled };
    let mut rows = Vec::new();
    if origin == MenuOrigin::Block {
        rows.push(row(SubagentItem::OpenInMap, true));
    }
    rows.push(row(SubagentItem::ShowInChat, call.is_some()));
    rows.push(row(SubagentItem::CopyPrompt, prompt.is_some()));
    if subagent.status == SubagentStatus::Done {
        rows.push(row(SubagentItem::CopyResult, result.is_some()));
    }
    if let Some(stop) = agent_map::stop_kind(subagent) {
        rows.push(row(SubagentItem::Stop(stop), true));
    }
    Some(SubagentTarget {
        agent,
        item,
        x,
        y,
        origin,
        prompt,
        result,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentDetails, AgentStatus, Subagent, SubagentRun, Timestamp, ToolCallStatus, TranscriptItem,
    };
    use serde_json::json;

    use super::*;
    use crate::state::app_state::Loadable;
    use crate::state::app_state::tests::{state_with, summary};

    fn subagent(item: u32, status: SubagentStatus, run: SubagentRun) -> Subagent {
        Subagent {
            item: ItemId::new(item),
            parent: None,
            description: format!("Subagent {item}"),
            agent_type: None,
            run,
            status,
            started_at: Timestamp::from_unix_millis(0),
            ended_at: None,
            tokens: None,
            tool_calls: 0,
            task: None,
            turn: 1,
            error: None,
            last_tool: None,
        }
    }

    fn call(item: u32, output: Option<&str>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(item),
            parent: None,
            body: ItemBody::Subagent(ToolCall {
                tool: "Agent".to_owned(),
                summary: format!("Subagent {item}"),
                input: json!({ "prompt": format!("Do task {item}") }),
                status: ToolCallStatus::Done,
                output: output.map(str::to_owned),
                exit_code: None,
                edit: None,
            }),
        }
    }

    fn state_of(subagents: Vec<Subagent>, held: Vec<TranscriptItem>) -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.details = Loadable::Loaded(AgentDetails {
                subagents,
                ..AgentDetails::default()
            });
            for item in held {
                entry.transcript.append(item);
            }
        }
        state
    }

    fn rows(target: &SubagentTarget) -> Vec<(SubagentItem, bool)> {
        target
            .rows
            .iter()
            .map(|row| (row.item, row.enabled))
            .collect()
    }

    #[test]
    fn the_rows_follow_what_the_app_holds_and_whether_the_subagent_can_stop() {
        let state = state_of(
            vec![
                subagent(1, SubagentStatus::Done, SubagentRun::Foreground),
                subagent(2, SubagentStatus::Running, SubagentRun::Foreground),
                subagent(3, SubagentStatus::Done, SubagentRun::Background),
            ],
            vec![call(1, Some("All done")), call(2, None)],
        );
        let menu = |item: u32, origin: MenuOrigin| {
            target(&state, AgentId::new(1), ItemId::new(item), 10, 20, origin).unwrap()
        };

        let done = menu(1, MenuOrigin::Map);
        let live = menu(2, MenuOrigin::Block);
        let older = menu(3, MenuOrigin::Map);

        assert_eq!(
            rows(&done),
            [
                (SubagentItem::ShowInChat, true),
                (SubagentItem::CopyPrompt, true),
                (SubagentItem::CopyResult, true),
            ]
        );
        assert_eq!(done.prompt.as_deref(), Some("Do task 1"));
        assert_eq!(done.result.as_deref(), Some("All done"));
        assert_eq!(
            rows(&live),
            [
                (SubagentItem::OpenInMap, true),
                (SubagentItem::ShowInChat, true),
                (SubagentItem::CopyPrompt, true),
                (SubagentItem::Stop(StopKind::Turn), true),
            ]
        );
        assert_eq!(
            rows(&older),
            [
                (SubagentItem::ShowInChat, false),
                (SubagentItem::CopyPrompt, false),
                (SubagentItem::CopyResult, false),
            ],
            "its call is older than the messages the app holds"
        );
    }

    #[test]
    fn the_menu_opens_in_the_map_or_the_block_and_closes_from_either() {
        let mut state = state_of(
            vec![subagent(1, SubagentStatus::Done, SubagentRun::Foreground)],
            vec![call(1, None)],
        );
        state.ui.selected = Some(AgentId::new(1));
        agent_map::toggle(&mut state);

        open_in_map(&mut state, ItemId::new(1), 5, 6);
        let in_map = map_target(&state).map(|target| (target.item, target.x, target.y));
        close(&mut state);
        let after_close = map_target(&state);
        state.ui.dialog = None;
        open_in_block(&mut state, AgentId::new(1), ItemId::new(1), 7, 8);
        let in_block = block_target(&state).map(|target| (target.origin, target.x));
        close(&mut state);

        assert_eq!(in_map, Some((ItemId::new(1), 5, 6)));
        assert_eq!(after_close, None);
        assert_eq!(state.ui.dialog, None);
        assert_eq!(in_block, Some((MenuOrigin::Block, 7)));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn esc_closes_the_map_menu_before_the_map() {
        let mut state = state_of(
            vec![subagent(1, SubagentStatus::Done, SubagentRun::Foreground)],
            vec![call(1, None)],
        );
        state.ui.selected = Some(AgentId::new(1));
        agent_map::toggle(&mut state);
        open_in_map(&mut state, ItemId::new(1), 5, 6);

        let first = state.ui.close_overlays();
        let menu_after_first = map_target(&state);
        let map_after_first = matches!(state.ui.dialog, Some(Dialog::AgentMap(_)));
        state.ui.close_overlays();

        assert!(first);
        assert_eq!(menu_after_first, None);
        assert!(map_after_first, "the map stays open");
        assert_eq!(state.ui.dialog, None);
    }
}

//! How the floating blocks show: as cards at the chat's top right, or folded into badges on its
//! right edge. The user folds and unfolds them while the chat has room for the cards. Without that
//! room, beside the file view or in a narrow window, they fold on their own, and come back when
//! the chat widens.

use cockpit_protocol::AgentId;

use super::app_state::AppState;
use super::terminals;
use crate::prefs::BlocksForm;

/// The narrowest chat, in pixels, that shows the blocks as cards: their 296 px column, and about
/// 460 px of text beside it.
pub const CARDS_ROOM: u16 = 760;

/// Whether the chat is wide enough for the cards, as the page measures it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatRoom {
    #[default]
    ForCards,
    TooNarrow,
}

/// How an agent's blocks show, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlocksView {
    Cards,
    /// Folded by the user: their button unfolds them.
    Folded,
    /// The file view takes the room of the cards: their button cannot unfold them.
    BesideFileView,
    /// The chat is too narrow for the cards: their button cannot unfold them.
    TooNarrow,
}

impl BlocksView {
    pub fn shows_badges(self) -> bool {
        self != Self::Cards
    }

    /// Folded for want of room, which the button cannot give them.
    pub fn squeezed(self) -> bool {
        matches!(self, Self::BesideFileView | Self::TooNarrow)
    }
}

pub fn view(state: &AppState, agent: AgentId) -> BlocksView {
    if terminals::side_open(state, agent) {
        return BlocksView::BesideFileView;
    }
    if state.ui.chat_room == ChatRoom::TooNarrow {
        return BlocksView::TooNarrow;
    }
    match state.prefs.blocks.form {
        BlocksForm::Cards => BlocksView::Cards,
        BlocksForm::Badges => BlocksView::Folded,
    }
}

pub fn shows_badges(state: &AppState, agent: AgentId) -> bool {
    view(state, agent).shows_badges()
}

/// The blocks' button and its shortcut: folds the selected agent's blocks into badges, or unfolds
/// them, for every agent. Without room for the cards, it does nothing.
pub fn toggle(state: &mut AppState) {
    let Some(agent) = state.ui.selected else {
        return;
    };
    state.prefs.blocks.form = match view(state, agent) {
        BlocksView::Cards => BlocksForm::Badges,
        BlocksView::Folded => BlocksForm::Cards,
        BlocksView::BesideFileView | BlocksView::TooNarrow => return,
    };
    // A badge's card goes with its badge, rather than open again with the next badges.
    state.ui.badge = None;
}

/// The page measured the chat's width again.
pub fn set_chat_room(state: &mut AppState, room: ChatRoom) {
    if state.ui.chat_room == room {
        return;
    }
    let before = selected_shows_badges(state);
    state.ui.chat_room = room;
    if before && !selected_shows_badges(state) {
        state.ui.badge = None;
    }
}

fn selected_shows_badges(state: &AppState) -> bool {
    state
        .ui
        .selected
        .is_some_and(|agent| shows_badges(state, agent))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{AgentStatus, TerminalId, TerminalInfo};

    use super::*;
    use crate::prefs::Dock;
    use crate::state::{Block, actions, state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn selected() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state
    }

    #[test]
    fn the_button_folds_and_unfolds_the_blocks_for_every_agent() {
        let other = AgentId::new(2);
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.ui.selected = Some(agent());

        let first = view(&state, agent());
        toggle(&mut state);
        let folded = (view(&state, agent()), view(&state, other));
        let form = state.prefs.blocks.form;
        toggle(&mut state);

        assert_eq!(first, BlocksView::Cards);
        assert_eq!(folded, (BlocksView::Folded, BlocksView::Folded));
        assert_eq!(form, BlocksForm::Badges);
        assert_eq!(view(&state, other), BlocksView::Cards);
    }

    #[test]
    fn a_narrow_chat_folds_the_blocks_until_it_widens_and_the_button_waits() {
        let mut state = selected();

        set_chat_room(&mut state, ChatRoom::TooNarrow);
        let squeezed = view(&state, agent());
        toggle(&mut state);
        let form = state.prefs.blocks.form;
        set_chat_room(&mut state, ChatRoom::ForCards);

        assert_eq!(squeezed, BlocksView::TooNarrow);
        assert_eq!(form, BlocksForm::Cards, "the button does nothing");
        assert_eq!(view(&state, agent()), BlocksView::Cards);
    }

    #[test]
    fn blocks_the_user_folded_stay_folded_when_the_chat_widens() {
        let mut state = selected();
        toggle(&mut state);

        set_chat_room(&mut state, ChatRoom::TooNarrow);
        set_chat_room(&mut state, ChatRoom::ForCards);

        assert_eq!(view(&state, agent()), BlocksView::Folded);
    }

    #[test]
    fn the_file_view_squeezes_the_blocks_whatever_the_width() {
        let mut state = selected();
        actions::open_file(&mut state, agent(), PathBuf::from("src/lexer.rs"), None);

        assert_eq!(view(&state, agent()), BlocksView::BesideFileView);
    }

    #[test]
    fn a_badge_opens_its_card_beside_the_terminal_alone_in_the_side_dock() {
        let mut state = selected();
        state.prefs.terminal.place = Dock::Side;
        let terminal = TerminalInfo {
            terminal: TerminalId::new(1),
            title: "zsh".to_owned(),
            exited: None,
        };
        terminals::listed(&mut state, agent(), Some(vec![terminal]), Vec::new());
        terminals::toggle_pane(&mut state);
        let open_before = state.ui.open_blocks.clone();

        actions::toggle_block(&mut state, agent(), Block::Tasks);

        assert_eq!(view(&state, agent()), BlocksView::BesideFileView);
        assert_eq!(state.ui.badge, Some(Block::Tasks));
        assert_eq!(
            state.ui.open_blocks, open_before,
            "the column's cards stay as they were"
        );
    }

    /// A badge's card would open again by itself the next time the blocks fold.
    #[test]
    fn coming_back_to_the_cards_closes_the_badges_card() {
        let mut narrow = selected();
        set_chat_room(&mut narrow, ChatRoom::TooNarrow);
        actions::toggle_block(&mut narrow, agent(), Block::Tasks);
        let mut folded = selected();
        toggle(&mut folded);
        actions::toggle_block(&mut folded, agent(), Block::Tasks);
        let opened = (narrow.ui.badge, folded.ui.badge);

        set_chat_room(&mut narrow, ChatRoom::ForCards);
        toggle(&mut folded);

        assert_eq!(opened, (Some(Block::Tasks), Some(Block::Tasks)));
        assert_eq!(narrow.ui.badge, None);
        assert_eq!(folded.ui.badge, None);
    }
}

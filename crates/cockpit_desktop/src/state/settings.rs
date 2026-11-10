//! The Settings dialog: the app's choices that are not made where they act, kept in the
//! preferences.

use super::app_state::AppState;
use super::ui_state::Dialog;
use crate::prefs::{CommentPlacement, PaneState};

/// Opens the dialog, from the gear in the sidebar's foot.
pub fn open(state: &mut AppState) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Settings);
}

/// Where new comments on quotes show. The comments already written keep their place.
pub fn set_comment_placement(state: &mut AppState, placement: CommentPlacement) {
    state.prefs.chat.comments = placement;
}

/// The files pane of an agent never opened or closed. The others keep theirs.
pub fn set_new_agents_pane(state: &mut AppState, pane: PaneState) {
    state.prefs.files_pane.new_agents = pane;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Popover;

    #[test]
    fn opening_the_settings_closes_the_open_menu() {
        let mut state = crate::state::state_with(Vec::new());
        state.ui.popover = Some(Popover::AppNotifications);

        open(&mut state);
        set_comment_placement(&mut state, CommentPlacement::Hidden);
        set_new_agents_pane(&mut state, PaneState::Open);

        assert_eq!(state.ui.popover, None);
        assert_eq!(state.ui.dialog, Some(Dialog::Settings));
        assert_eq!(state.prefs.chat.comments, CommentPlacement::Hidden);
        assert_eq!(state.prefs.files_pane.new_agents, PaneState::Open);
    }
}

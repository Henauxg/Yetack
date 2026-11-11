//! The Settings dialog: the app's choices that are not made where they act, kept in the
//! preferences. Its Keyboard tab records new shortcuts for the commands.

use super::app_state::AppState;
use super::ui_state::{Dialog, RefusedChord, SettingsTab, SettingsView};
use crate::keymap::{Chord, Command, Refusal};
use crate::prefs::{CommentPlacement, PaneState, SteerHold};

/// Opens the dialog, from the gear in the sidebar's foot.
pub fn open(state: &mut AppState) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Settings(SettingsView::default()));
}

/// Where new comments on quotes show. The comments already written keep their place.
pub fn set_comment_placement(state: &mut AppState, placement: CommentPlacement) {
    state.prefs.chat.comments = placement;
}

/// How long a message steered to a busy agent waits in the app. The messages already held keep
/// their hold.
pub fn set_steer_hold(state: &mut AppState, hold: SteerHold) {
    state.prefs.chat.steer_hold = hold;
}

/// The files pane of an agent never opened or closed. The others keep theirs.
pub fn set_new_agents_pane(state: &mut AppState, pane: PaneState) {
    state.prefs.files_pane.new_agents = pane;
}

pub fn show_tab(state: &mut AppState, tab: SettingsTab) {
    if let Some(view) = view_mut(state) {
        *view = SettingsView {
            tab,
            ..SettingsView::default()
        };
    }
}

/// Waits for the command's new shortcut, or stops waiting when it already did.
pub fn toggle_recording(state: &mut AppState, command: Command) {
    if let Some(view) = view_mut(state) {
        view.recording = (view.recording != Some(command)).then_some(command);
        view.refused = None;
    }
}

/// The focus left the recording button: the dialog stops waiting for keys, and keeps saying why
/// the last chord was refused, so that its swap still works.
pub fn recording_left(state: &mut AppState) {
    if let Some(view) = view_mut(state) {
        view.recording = None;
    }
}

pub fn stop_recording(state: &mut AppState) {
    if let Some(view) = view_mut(state) {
        view.recording = None;
        view.refused = None;
    }
}

/// A chord pressed while the dialog records: the command takes it, or the dialog says why not and
/// keeps recording.
pub fn press_chord(state: &mut AppState, chord: Chord) {
    let Some(command) = view_mut(state).and_then(|view| view.recording) else {
        return;
    };
    let result = state.prefs.keys.set(command, chord.clone());
    settle(state, command, chord, result);
}

/// Gives the refused chord to its command, and the command that had it the chord given up.
pub fn swap_refused(state: &mut AppState) {
    let Some(RefusedChord { command, chord, .. }) =
        view_mut(state).and_then(|view| view.refused.clone())
    else {
        return;
    };
    let result = state.prefs.keys.swap(command, chord.clone());
    settle(state, command, chord, result);
}

/// Gives the command its default back. While another command has it, the dialog offers a swap.
pub fn reset(state: &mut AppState, command: Command) {
    let result = state.prefs.keys.reset(command);
    settle(state, command, command.default_chord(), result);
}

pub fn reset_all(state: &mut AppState) {
    state.prefs.keys.reset_all();
    stop_recording(state);
}

fn settle(state: &mut AppState, command: Command, chord: Chord, result: Result<(), Refusal>) {
    let Some(view) = view_mut(state) else {
        return;
    };
    match result {
        Ok(()) => {
            view.recording = None;
            view.refused = None;
        }
        Err(refusal) => {
            view.refused = Some(RefusedChord {
                command,
                chord,
                refusal,
            });
        }
    }
}

fn view_mut(state: &mut AppState) -> Option<&mut SettingsView> {
    match &mut state.ui.dialog {
        Some(Dialog::Settings(view)) => Some(view),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Popover;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap()
    }

    fn view(state: &AppState) -> SettingsView {
        match &state.ui.dialog {
            Some(Dialog::Settings(view)) => view.clone(),
            _ => panic!("the settings are not open"),
        }
    }

    fn keyboard() -> AppState {
        let mut state = crate::state::state_with(Vec::new());
        open(&mut state);
        show_tab(&mut state, SettingsTab::Keyboard);
        state
    }

    #[test]
    fn opening_the_settings_closes_the_open_menu() {
        let mut state = crate::state::state_with(Vec::new());
        state.ui.popover = Some(Popover::AppNotifications);

        open(&mut state);
        set_comment_placement(&mut state, CommentPlacement::Hidden);
        set_new_agents_pane(&mut state, PaneState::Open);

        assert_eq!(state.ui.popover, None);
        assert_eq!(view(&state), SettingsView::default());
        assert_eq!(state.prefs.chat.comments, CommentPlacement::Hidden);
        assert_eq!(state.prefs.files_pane.new_agents, PaneState::Open);
    }

    #[test]
    fn a_chord_pressed_while_recording_becomes_the_shortcut() {
        let mut state = keyboard();

        toggle_recording(&mut state, Command::Switcher);
        let recording = view(&state).recording;
        press_chord(&mut state, chord("Ctrl+j"));

        assert_eq!(recording, Some(Command::Switcher));
        assert_eq!(view(&state).recording, None);
        assert_eq!(state.prefs.keys.shown(Command::Switcher), "Ctrl+J");
    }

    #[test]
    fn a_refused_chord_keeps_recording_and_a_swap_settles_it() {
        let mut state = keyboard();
        toggle_recording(&mut state, Command::CloseAgent);

        press_chord(&mut state, chord("j"));
        let typed = view(&state).refused.map(|refused| refused.refusal);
        press_chord(&mut state, chord("Ctrl+k"));
        let taken = view(&state);
        swap_refused(&mut state);

        assert_eq!(typed, Some(Refusal::TypesText));
        assert_eq!(taken.recording, Some(Command::CloseAgent));
        assert_eq!(
            taken.refused.map(|refused| refused.refusal),
            Some(Refusal::Taken(Command::Switcher))
        );
        assert_eq!(
            view(&state),
            SettingsView {
                tab: SettingsTab::Keyboard,
                ..SettingsView::default()
            }
        );
        assert_eq!(state.prefs.keys.shown(Command::CloseAgent), "Ctrl+K");
        assert_eq!(state.prefs.keys.shown(Command::Switcher), "Ctrl+Shift+W");
    }

    #[test]
    fn a_reset_onto_a_taken_default_offers_the_swap() {
        let mut state = keyboard();
        state
            .prefs
            .keys
            .set(Command::Switcher, chord("Ctrl+j"))
            .unwrap();
        state
            .prefs
            .keys
            .set(Command::NewAgent, chord("Ctrl+k"))
            .unwrap();

        reset(&mut state, Command::Switcher);
        let offered = view(&state).refused;
        swap_refused(&mut state);
        reset_all(&mut state);

        assert_eq!(
            offered,
            Some(RefusedChord {
                command: Command::Switcher,
                chord: chord("Ctrl+k"),
                refusal: Refusal::Taken(Command::NewAgent),
            })
        );
        assert_eq!(state.prefs.keys.changed_count(), 0);
    }

    #[test]
    fn leaving_the_recording_keeps_the_refusal_and_its_swap() {
        let mut state = keyboard();
        toggle_recording(&mut state, Command::CloseAgent);
        press_chord(&mut state, chord("Ctrl+k"));

        recording_left(&mut state);
        let left = view(&state);
        swap_refused(&mut state);

        assert_eq!(left.recording, None);
        assert!(left.refused.is_some());
        assert_eq!(state.prefs.keys.shown(Command::CloseAgent), "Ctrl+K");
    }

    #[test]
    fn another_tab_stops_the_recording() {
        let mut state = keyboard();
        toggle_recording(&mut state, Command::Rename);

        show_tab(&mut state, SettingsTab::General);
        press_chord(&mut state, chord("F7"));

        assert_eq!(view(&state).recording, None);
        assert_eq!(state.prefs.keys.shown(Command::Rename), "F2");
    }
}

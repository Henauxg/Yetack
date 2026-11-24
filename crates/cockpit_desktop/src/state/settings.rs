//! The Settings dialog: the app's choices that are not made where they act, kept in the
//! preferences. Where Local's agents run is kept in the config instead, as the "No daemon
//! answers" screen keeps its choice. Its Keyboard tab records new shortcuts for the commands.

use cockpit_protocol::Repository;
use strum::VariantArray;

use super::app_state::AppState;
use super::machines::THIS_COMPUTER;
use super::no_daemon;
use super::projects::{self, NEW_PROJECT_NAME, ProjectKey, RuleKind};
use super::ui_state::{Dialog, RefusedChord, SettingsTab, SettingsView};
use crate::config::{DAEMON_SECTION, DaemonConfig};
use crate::keymap::{Chord, Command, Refusal};
use crate::prefs::{
    AfterDictation, CommentPlacement, DictationLanguage, EscStop, LiveTextRefresh, PaneState,
    ProjectId, Scrollback, SteerHold, ToolCallDetail,
};

/// What the Local section says where the background service does not run.
const IN_APP_ONLY: &str = "On Windows, the agents always run in this app. Windows has no background service. To run agents elsewhere, add a server in Machines.";

/// Where Local's agents run from the next start, as the Settings dialog sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, VariantArray)]
pub enum StartDaemon {
    /// The agents stop when the app closes.
    InApp,
    /// The app starts the background service when none answers, and the agents outlive the app.
    Service,
    /// The app reaches a service that runs, and asks what to do when none answers.
    Ask,
}

impl StartDaemon {
    /// The config of this choice. A socket `current` names stays.
    fn config(self, current: Option<&DaemonConfig>) -> DaemonConfig {
        let socket = match current {
            Some(DaemonConfig::Local { socket, .. }) => socket.clone(),
            Some(DaemonConfig::Ssh { .. } | DaemonConfig::InApp) | None => None,
        };
        match self {
            Self::InApp => DaemonConfig::InApp,
            Self::Service => DaemonConfig::Local {
                socket,
                start_service: true,
            },
            Self::Ask => DaemonConfig::Local {
                socket,
                start_service: false,
            },
        }
    }
}

/// What the Local section shows of where its agents run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalStart {
    /// The choices, with the one the config makes.
    Choices(StartDaemon),
    /// Why the section offers no choice.
    Note(String),
}

/// What the Local section shows for the config's daemon. A daemon over SSH is set in the config
/// file alone. A config without a daemon asks, as "Ask" does.
pub fn local_start(state: &AppState) -> LocalStart {
    let chosen = match &state.machines.configured {
        Some(DaemonConfig::Ssh { host, .. }) => {
            return LocalStart::Note(format!(
                "The agents run on {host}, over SSH, as the config file's [{DAEMON_SECTION}] section says. Change it in the file."
            ));
        }
        Some(DaemonConfig::InApp) => StartDaemon::InApp,
        Some(DaemonConfig::Local {
            start_service: true,
            ..
        }) => StartDaemon::Service,
        Some(DaemonConfig::Local {
            start_service: false,
            ..
        })
        | None => StartDaemon::Ask,
    };
    if state.machines.service_offered {
        LocalStart::Choices(chosen)
    } else {
        LocalStart::Note(IN_APP_ONLY.to_owned())
    }
}

/// What the "No daemon answers" screen says of changing its choice later, while "Use this every
/// time" is ticked and where the Local section offers choices.
pub fn change_later(state: &AppState) -> Option<String> {
    (state.machines.service_offered && no_daemon::every_time(state)).then(|| {
        format!(
            "You can change it later under {THIS_COMPUTER} in Settings, from the gear at the foot of the sidebar."
        )
    })
}

/// A click on one of the Local section's choices. Returns the daemon for the config to take. The
/// section shows the choice once the config took it.
pub fn choose_start(state: &mut AppState, choice: StartDaemon) -> Option<DaemonConfig> {
    if let Some(view) = view_mut(state) {
        view.start_error = None;
    }
    if !matches!(local_start(state), LocalStart::Choices(_)) {
        return None;
    }
    Some(choice.config(state.machines.configured.as_ref()))
}

/// Writes `next` to the config with `save`, unless the config names it already: a click sent
/// earlier may have made it so. A config without a daemon takes every choice, "Ask" included, so
/// that the question's "Use this every time" starts unticked from then on. A refusal, as a
/// read-only config gives, keeps the config's choice, and the Local section says why under it. The
/// window's notes sit under the dialog's backdrop, so they say it only when the section is not on
/// screen.
pub fn save_start(
    state: &mut AppState,
    next: DaemonConfig,
    save: impl FnOnce(&DaemonConfig) -> Result<(), String>,
) {
    if state.machines.configured.as_ref() == Some(&next) {
        return;
    }
    let error = match save(&next) {
        Ok(()) => {
            state.machines.configured = Some(next);
            return;
        }
        Err(error) => error,
    };
    match view_mut(state) {
        Some(view) if view.tab == SettingsTab::General => {
            view.start_error = Some(format!("Not saved: {error}"));
        }
        Some(_) | None => state.ui.report(format!(
            "Cannot save where {THIS_COMPUTER}'s agents run: {error}"
        )),
    }
}

/// Opens the dialog, from the gear in the sidebar's foot.
pub fn open(state: &mut AppState) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Settings(SettingsView::default()));
}

/// Where new comments on quotes show. The comments already written keep their place.
pub fn set_comment_placement(state: &mut AppState, placement: CommentPlacement) {
    state.prefs.chat.comments = placement;
}

/// How much of each tool call the chat shows. A call unfolded or folded by a click keeps it.
pub fn set_tool_calls(state: &mut AppState, detail: ToolCallDetail) {
    state.prefs.chat.tool_calls = detail;
}

/// How long a message steered to a busy agent waits in the app. The messages already held keep
/// their hold.
pub fn set_steer_hold(state: &mut AppState, hold: SteerHold) {
    state.prefs.chat.steer_hold = hold;
}

/// The history the terminals opened from now on keep. The open ones keep theirs: ghostty-web
/// cannot change it.
pub fn set_scrollback(state: &mut AppState, scrollback: Scrollback) {
    state.prefs.terminal.scrollback = scrollback;
}

/// Off hides the mic button and frees the talk key.
pub fn set_voice_enabled(state: &mut AppState, enabled: bool) {
    state.prefs.voice.enabled = enabled;
}

pub fn set_voice_after(state: &mut AppState, after: AfterDictation) {
    state.prefs.voice.after = after;
}

pub fn set_voice_language(state: &mut AppState, language: DictationLanguage) {
    state.prefs.voice.language = language;
}

/// Takes effect at the next dictation.
pub fn set_voice_live(state: &mut AppState, live: LiveTextRefresh) {
    state.prefs.voice.live = live;
}

/// `None` records from the system default.
pub fn set_microphone(state: &mut AppState, microphone: Option<String>) {
    state.prefs.voice.microphone = microphone;
}

pub fn set_esc_stop(state: &mut AppState, esc_stop: EscStop) {
    state.prefs.chat.esc_stop = esc_stop;
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

/// Opens the dialog on the Projects tab, with the card of `project` open and its name field
/// focused.
pub fn open_project(state: &mut AppState, project: ProjectId) {
    show_project(state, project);
    if let Some(view) = view_mut(state) {
        view.created = Some(project);
    }
}

/// Opens the dialog on the Projects tab, with the card of `project` open.
pub fn show_project(state: &mut AppState, project: ProjectId) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Settings(SettingsView {
        tab: SettingsTab::Projects,
        project: Some(ProjectKey::Kept(project)),
        ..SettingsView::default()
    }));
}

/// Opens the project's card in the Projects tab, or closes it when it is open.
pub fn toggle_project(state: &mut AppState, project: &ProjectKey) {
    if let Some(view) = view_mut(state) {
        view.project = (view.project.as_ref() != Some(project)).then(|| project.clone());
        view.adding_rule = None;
        view.created = None;
    }
}

/// Shows the open card's field for a new rule of `kind`, or hides it.
pub fn show_rule_field(state: &mut AppState, kind: Option<RuleKind>) {
    if let Some(view) = view_mut(state) {
        view.adding_rule = kind;
    }
}

pub fn add_rule(state: &mut AppState, project: ProjectId, kind: RuleKind, text: String) {
    projects::add_rule(state, project, kind.rule(text));
    show_rule_field(state, None);
}

/// A new project with no rule, its card open on its name.
pub fn new_project(state: &mut AppState) {
    let id = projects::create(state, NEW_PROJECT_NAME);
    if let Some(view) = view_mut(state) {
        view.project = Some(ProjectKey::Kept(id));
        view.adding_rule = None;
        view.created = Some(id);
    }
}

/// Keeps a temporary project, and leaves its card open under its kept key.
pub fn keep_project(state: &mut AppState, repository: &Repository) {
    let id = projects::keep(state, repository);
    if let Some(view) = view_mut(state) {
        view.project = Some(ProjectKey::Kept(id));
    }
}

pub fn delete_project(state: &mut AppState, project: ProjectId) {
    projects::delete(state, project);
    if let Some(view) = view_mut(state) {
        view.project = None;
        view.adding_rule = None;
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
    use std::path::PathBuf;

    use super::*;
    use crate::config::Config;
    use crate::daemon_label::DaemonLabel;
    use crate::prefs::ProjectRule;
    use crate::state::Popover;
    use crate::state::no_daemon::{Absence, DaemonChoice};

    const READ_ONLY: &str =
        "/nix/store/made-up-home-manager-files/.config/yetack/config.toml is read-only";

    /// The General tab, where the background service runs and the config names no daemon.
    fn general() -> AppState {
        let mut state = crate::state::state_with(Vec::new());
        state.machines.service_offered = true;
        open(&mut state);
        state
    }

    #[test]
    fn each_start_choice_names_its_daemon_and_keeps_a_socket_the_config_names() {
        let socket = Some(PathBuf::from("/run/cockpit/agents.sock"));
        let mut state = general();
        state.machines.configured = Some(DaemonConfig::Local {
            socket: socket.clone(),
            start_service: false,
        });

        let to_service = choose_start(&mut state, StartDaemon::Service);
        let to_app = choose_start(&mut state, StartDaemon::InApp);

        assert_eq!(
            to_service,
            Some(DaemonConfig::Local {
                socket,
                start_service: true,
            })
        );
        assert_eq!(to_app, Some(DaemonConfig::InApp));
        assert_eq!(
            local_start(&state),
            LocalStart::Choices(StartDaemon::Ask),
            "the section waits for the config to take a choice"
        );
    }

    #[test]
    fn quick_clicks_end_on_the_last_choice_and_write_each_change_once() {
        let mut state = general();
        let mut written = Vec::new();

        // The clicks come before the connection task writes the first one.
        let mut sent = Vec::new();
        for choice in [StartDaemon::Service, StartDaemon::Service, StartDaemon::Ask] {
            sent.extend(choose_start(&mut state, choice));
        }
        for next in sent {
            save_start(&mut state, next, |next| {
                written.push(next.clone());
                Ok(())
            });
        }
        let current = choose_start(&mut state, StartDaemon::Ask);
        if let Some(current) = current {
            save_start(&mut state, current, |_| Err(READ_ONLY.to_owned()));
        }

        assert_eq!(local_start(&state), LocalStart::Choices(StartDaemon::Ask));
        assert_eq!(
            written,
            [
                DaemonConfig::Local {
                    socket: None,
                    start_service: true,
                },
                DaemonConfig::default(),
            ]
        );
        assert_eq!(
            view(&state).start_error,
            None,
            "the config names the current choice already"
        );
    }

    #[test]
    fn a_start_choice_the_config_refuses_keeps_the_old_one_and_says_why_under_it() {
        let mut state = general();
        let Some(next) = choose_start(&mut state, StartDaemon::InApp) else {
            panic!("the section offers no choice");
        };

        save_start(&mut state, next, |_| Err(READ_ONLY.to_owned()));
        let refused = (local_start(&state), view(&state).start_error);
        let service = choose_start(&mut state, StartDaemon::Service);
        let cleared = view(&state).start_error;
        if let Some(service) = service {
            save_start(&mut state, service, |_| Ok(()));
        }

        assert_eq!(
            refused,
            (
                LocalStart::Choices(StartDaemon::Ask),
                Some(format!("Not saved: {READ_ONLY}"))
            )
        );
        assert!(state.ui.errors.is_empty(), "the section says it once");
        assert_eq!(cleared, None, "a new choice clears the line");
        assert_eq!(
            local_start(&state),
            LocalStart::Choices(StartDaemon::Service)
        );
    }

    #[test]
    fn a_refusal_the_section_cannot_show_goes_to_the_notes() {
        let mut state = crate::state::state_with(Vec::new());

        save_start(&mut state, DaemonConfig::InApp, |_| {
            Err(READ_ONLY.to_owned())
        });
        open(&mut state);
        show_tab(&mut state, SettingsTab::Keyboard);
        save_start(&mut state, DaemonConfig::InApp, |_| {
            Err(READ_ONLY.to_owned())
        });

        assert_eq!(state.machines.configured, None);
        assert_eq!(state.ui.errors.len(), 2);
        assert_eq!(view(&state).start_error, None);
    }

    #[test]
    fn where_no_background_service_runs_the_agents_run_in_the_app() {
        let mut state = general();
        state.machines.service_offered = false;
        show_question(&mut state);

        let section = local_start(&state);
        let chosen = choose_start(&mut state, StartDaemon::InApp);
        let hint = change_later(&state);
        state.machines.service_offered = true;

        assert_eq!(section, LocalStart::Note(IN_APP_ONLY.to_owned()));
        assert_eq!(chosen, None);
        assert_eq!(hint, None, "the section offers no choice to change");
        assert_eq!(change_later(&state).as_deref(), Some(CHANGE_LATER));
    }

    /// The hint under a ticked "Use this every time".
    const CHANGE_LATER: &str = "You can change it later under Local in Settings, from the gear at the foot of the sidebar.";

    /// The "No daemon answers" screen, at a start where nothing runs, as a dialog beside servers.
    fn show_question(state: &mut AppState) {
        no_daemon::show(
            state,
            &DaemonLabel::socket(),
            "no daemon listens on its socket".to_owned(),
            Absence::NothingRuns,
            None,
        );
    }

    #[test]
    fn the_hint_shows_only_while_the_box_is_ticked() {
        let mut state = general();
        show_question(&mut state);

        let ticked = change_later(&state);
        no_daemon::toggle_every_time(&mut state);
        let unticked = change_later(&state);
        no_daemon::toggle_every_time(&mut state);

        assert_eq!(ticked.as_deref(), Some(CHANGE_LATER));
        assert_eq!(unticked, None);
        assert_eq!(change_later(&state).as_deref(), Some(CHANGE_LATER));
    }

    #[test]
    fn ask_chosen_in_settings_keeps_asking_on_the_screen() {
        let mut state = general();
        state.machines.configured = "".parse::<Config>().unwrap().daemon;
        show_question(&mut state);
        let mut written = Vec::new();

        let ask = choose_start(&mut state, StartDaemon::Ask);
        if let Some(ask) = ask {
            save_start(&mut state, ask, |next| {
                written.push(next.clone());
                Ok(())
            });
        }
        let hint = change_later(&state);
        let chosen = no_daemon::choose(&mut state, DaemonChoice::InApp);

        assert_eq!(
            written,
            [DaemonConfig::Local {
                socket: None,
                start_service: false,
            }],
            "the config names the daemon from then on"
        );
        assert_eq!(chosen.map(|chosen| chosen.every_time), Some(false));
        assert_eq!(hint, None);

        // Shown afresh, as at the next start.
        show_question(&mut state);
        assert_eq!(
            no_daemon::choose(&mut state, DaemonChoice::InApp).map(|chosen| chosen.every_time),
            Some(false)
        );
    }

    #[test]
    fn a_daemon_over_ssh_is_changed_in_the_config_file() {
        let mut state = general();
        state.machines.configured = Some(DaemonConfig::Ssh {
            host: "build-box".to_owned(),
            remote_command: "/opt/yetack".to_owned(),
            source_dir: None,
        });

        let section = local_start(&state);
        let chosen = choose_start(&mut state, StartDaemon::InApp);

        assert_eq!(
            section,
            LocalStart::Note(
                "The agents run on build-box, over SSH, as the config file's [daemon] section says. Change it in the file.".to_owned()
            )
        );
        assert_eq!(chosen, None);
    }

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

    fn projects_tab() -> AppState {
        let mut state = crate::state::state_with(Vec::new());
        open(&mut state);
        show_tab(&mut state, SettingsTab::Projects);
        state
    }

    #[test]
    fn a_new_project_opens_its_card_and_esc_closes_its_rule_field_first() {
        let mut state = projects_tab();

        new_project(&mut state);
        let id = state.prefs.projects.list[0].id;
        let created = view(&state);
        show_rule_field(&mut state, Some(RuleKind::Folder));
        let first_esc = state.ui.close_overlays();
        let after_first = view(&state).adding_rule;
        show_rule_field(&mut state, Some(RuleKind::Repository));
        add_rule(
            &mut state,
            id,
            RuleKind::Repository,
            "https://github.com/acme/app.git".to_owned(),
        );

        assert_eq!(created.project, Some(ProjectKey::Kept(id)));
        assert_eq!(created.created, Some(id));
        assert!(first_esc);
        assert_eq!(after_first, None);
        assert_eq!(view(&state).adding_rule, None);
        assert_eq!(
            state.prefs.projects.list[0].rules,
            [ProjectRule::Repository(Repository::from_remote(
                "github.com/acme/app"
            ))]
        );
    }

    #[test]
    fn keeping_a_temporary_project_leaves_its_card_open_and_deleting_closes_it() {
        let mut state = projects_tab();
        let repository = Repository::from_remote("github.com/acme/app");
        let temporary = ProjectKey::Repository(repository.clone());
        toggle_project(&mut state, &temporary);

        keep_project(&mut state, &repository);
        let id = state.prefs.projects.list[0].id;
        let kept = view(&state).project;
        delete_project(&mut state, id);

        assert_eq!(kept, Some(ProjectKey::Kept(id)));
        assert_eq!(view(&state).project, None);
        assert!(state.prefs.projects.list.is_empty());
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

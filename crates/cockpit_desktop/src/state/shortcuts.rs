//! The keyboard shortcuts of R4-parts, resolved against the state. The page's script decides
//! which key presses reach here (`bridge`), so typing in a field is never taken over.

use cockpit_protocol::{AgentId, AgentStatus, ApprovalDecision, PlanDecision};
use serde::Deserialize;

use super::actions::{self, Direction};
use super::app_state::AppState;
use super::drafts::Drafts;
use super::requests::Outgoing;
use super::transcript::WaitingKind;
use super::ui_state::{Dialog, PaletteOrigin, Popover};

/// `KeyboardEvent.key` values.
const KEY_ESCAPE: &str = "Escape";
const KEY_ENTER: &str = "Enter";
const KEY_TAB: &str = "Tab";
const KEY_UP: &str = "ArrowUp";
const KEY_DOWN: &str = "ArrowDown";
const KEY_F2: &str = "F2";

/// The letters that answer an approval or a plan.
const KEY_YES: &str = "y";
const KEY_NO: &str = "n";

/// A key press the page's script passes on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyPress {
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub target: KeyTarget,
    /// The composer's text when the key went to it, which may hold a keystroke the draft has not
    /// received yet.
    #[serde(default)]
    pub composer_text: String,
}

/// Where the key went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyTarget {
    Composer,
    /// Another text field: only the shortcuts with Ctrl, and Esc.
    Field,
    /// No text field has the focus.
    Page,
}

pub fn handle_key(state: &mut AppState, drafts: &mut Drafts, press: &KeyPress) -> Vec<Outgoing> {
    let command = press.ctrl || press.meta;
    let letter = press.key.to_lowercase();
    if command && !press.alt && !press.shift {
        match letter.as_str() {
            "k" => actions::toggle_switcher(state, drafts),
            "n" => actions::open_new_agent(state, drafts),
            _ => {}
        }
        return Vec::new();
    }
    // Ctrl+Shift+W asks to close the selected agent, and confirms when asked.
    if command && !press.alt && press.shift && letter == "w" {
        return match (&state.ui.dialog, state.ui.selected) {
            (Some(Dialog::CloseAgent(agent)), _) => actions::close_agent(state, drafts, *agent),
            (None, Some(agent)) => actions::request_close(state, drafts, agent),
            (Some(_), _) | (None, None) => Vec::new(),
        };
    }
    if press.key == KEY_ESCAPE {
        if state.ui.close_overlays() {
            return Vec::new();
        }
        // Stop works only while the agent works: one that waits for you stops with the Stop
        // button.
        let working = state
            .selected()
            .filter(|entry| entry.summary.status == AgentStatus::Working);
        return working
            .map(|entry| actions::interrupt(state, entry.id()))
            .unwrap_or_default();
    }
    // The script passes F2 on from outside text fields, and from the title fields.
    let plain = !command && !press.alt && !press.shift;
    if press.key == KEY_F2 && plain && state.ui.dialog.is_none() {
        actions::rename_key(state, drafts);
        return Vec::new();
    }
    // Alt+Up and Alt+Down move the selected agent in the sidebar, from anywhere but a text field
    // other than the composer.
    let moves = press.alt && !command && !press.shift;
    if moves && state.ui.dialog.is_none() && press.target != KeyTarget::Field {
        match press.key.as_str() {
            KEY_UP => return actions::move_selected(state, Direction::Up),
            KEY_DOWN => return actions::move_selected(state, Direction::Down),
            _ => {}
        }
    }
    if command || press.alt {
        return Vec::new();
    }
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    match press.target {
        KeyTarget::Composer => composer_key(state, drafts, agent, press),
        KeyTarget::Page if state.ui.dialog.is_none() => card_key(state, drafts, agent, &press.key),
        KeyTarget::Page | KeyTarget::Field => Vec::new(),
    }
}

fn composer_key(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    press: &KeyPress,
) -> Vec<Outgoing> {
    if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)) {
        match press.key.as_str() {
            KEY_UP => actions::palette_move(state, drafts, agent, Direction::Up),
            KEY_DOWN => actions::palette_move(state, drafts, agent, Direction::Down),
            // Opened by typing, the palette lists commands alone. A pick puts one in the
            // composer, which already has the focus the pick asks the window for.
            KEY_ENTER | KEY_TAB => {
                sync_composer_text(state, drafts, agent, &press.composer_text);
                if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)) {
                    return actions::palette_pick(state, drafts, agent, None).requests;
                }
            }
            _ => {}
        }
        // The text the key saw closed the palette: only Enter goes on, to send it.
        if press.key != KEY_ENTER {
            return Vec::new();
        }
    }
    if press.composer_text.is_empty() {
        let answered = card_key(state, drafts, agent, &press.key);
        if !answered.is_empty() || press.key != KEY_ENTER {
            return answered;
        }
    }
    if press.key == KEY_ENTER && !press.shift {
        let typed = Some(press.composer_text.clone());
        return actions::send_prompt(state, drafts, agent, typed);
    }
    Vec::new()
}

/// The draft can lag a keystroke behind the composer: a key that acts on the typed text first
/// gives the draft the text the key saw. The same text leaves the palette's highlight in place.
fn sync_composer_text(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, text: &str) {
    let draft = drafts.draft(agent).map(|draft| draft.text.as_str());
    if draft == Some(text) {
        return;
    }
    if let Some(change) = actions::composer_input(state, drafts, agent, text.to_owned()) {
        actions::change_palette(state, agent, change);
    }
}

/// Y and N answer an approval or a plan; 1 to 9 pick an answer to a question, Enter sends it.
fn card_key(state: &AppState, drafts: &mut Drafts, agent: AgentId, key: &str) -> Vec<Outgoing> {
    let Some(waiting) = state
        .agents
        .get(&agent)
        .and_then(|entry| entry.transcript.waiting())
    else {
        return Vec::new();
    };
    let letter = key.to_lowercase();
    match (waiting.kind, letter.as_str()) {
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
        (WaitingKind::Question, _) if key == KEY_ENTER => {
            actions::submit_question(state, drafts, agent, waiting.item)
        }
        (WaitingKind::Question, digit) => {
            if let Ok(number) = digit.parse::<usize>() {
                actions::pick_by_number(state, drafts, agent, waiting.item, number);
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentDetails, AgentStatus, Answer, AnswerState, Approval, AskedQuestion, CommandKind,
        ItemBody, ItemId, PendingId, Question, QuestionOption, Request, SlashCommand,
        TranscriptItem,
    };
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::app_state::{ConnectionState, Loadable};
    use crate::state::ui_state::Dialog;

    fn press(key: &str, target: KeyTarget) -> KeyPress {
        KeyPress {
            key: key.to_owned(),
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
            target,
            composer_text: String::new(),
        }
    }

    fn ctrl(key: &str) -> KeyPress {
        KeyPress {
            ctrl: true,
            ..press(key, KeyTarget::Field)
        }
    }

    fn state(status: AgentStatus, body: Option<ItemBody>) -> AppState {
        let mut state = state_with(vec![summary(1, status)]);
        state.connection = ConnectionState::Connected;
        state.ui.selected = Some(AgentId::new(1));
        if let (Some(body), Some(entry)) = (body, state.agents.get_mut(&AgentId::new(1))) {
            entry.transcript.append(TranscriptItem {
                id: ItemId::new(3),
                parent: None,
                body,
            });
        }
        state
    }

    fn approval() -> ItemBody {
        ItemBody::Approval(Approval {
            pending: PendingId::new("request-3".to_owned()),
            tool: "Bash".to_owned(),
            summary: "cargo test".to_owned(),
            input: Value::Null,
            edit: None,
            reason: None,
            answer: AnswerState::Waiting,
            tool_call: None,
        })
    }

    fn question() -> ItemBody {
        ItemBody::Question(Question {
            pending: PendingId::new("request-3".to_owned()),
            questions: vec![AskedQuestion {
                header: "Scope".to_owned(),
                text: "Which tests?".to_owned(),
                options: vec![
                    QuestionOption {
                        label: "Unit".to_owned(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "All".to_owned(),
                        description: String::new(),
                    },
                ],
                multi_select: false,
            }],
            answer: AnswerState::Waiting,
        })
    }

    fn sent_answer(requests: &[Outgoing]) -> Option<&Answer> {
        match &requests.first()?.request {
            Request::AgentAnswer(params) => Some(&params.answer),
            _ => None,
        }
    }

    #[test]
    fn ctrl_k_and_ctrl_n_open_their_dialogs() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);

        handle_key(&mut state, &mut drafts, &ctrl("k"));
        assert!(matches!(state.ui.dialog, Some(Dialog::Switcher)));

        handle_key(&mut state, &mut drafts, &ctrl("N"));
        assert!(matches!(state.ui.dialog, Some(Dialog::NewAgent(_))));
    }

    #[test]
    fn escape_closes_what_is_open_before_stopping_the_agent() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        state.ui.popover = Some(Popover::Usage);

        let first = handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));
        let second = handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));

        assert!(first.is_empty());
        assert_eq!(state.ui.popover, None);
        assert!(matches!(
            second.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentInterrupt(_))
        ));
    }

    #[test]
    fn enter_and_tab_in_a_typed_palette_insert_the_command_and_send_nothing() {
        let agent = AgentId::new(1);
        for key in [KEY_ENTER, KEY_TAB] {
            let mut drafts = Drafts::default();
            let mut state = state(AgentStatus::Idle, None);
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.details = Loadable::Loaded(AgentDetails {
                    commands: vec![SlashCommand {
                        name: "review".to_owned(),
                        description: "Review the changes on the branch".to_owned(),
                        kind: CommandKind::Skill,
                    }],
                    ..AgentDetails::default()
                });
            }
            let typed = actions::composer_input(&state, &mut drafts, agent, "/rev".to_owned());
            if let Some(change) = typed {
                actions::change_palette(&mut state, agent, change);
            }
            let enter = KeyPress {
                composer_text: "/rev".to_owned(),
                ..press(key, KeyTarget::Composer)
            };

            let requests = handle_key(&mut state, &mut drafts, &enter);

            assert!(requests.is_empty(), "{key} sends nothing");
            let text = drafts.draft(agent).map(|draft| draft.text.as_str());
            assert_eq!(text, Some("/review "), "{key}");
            assert_eq!(state.ui.popover, None, "{key}");
        }
    }

    /// The draft can lag a keystroke behind the field: "/re" in the draft and "/ren" in the field
    /// must pick "rename-branch", not "review".
    #[test]
    fn enter_in_a_typed_palette_picks_from_the_text_the_key_saw() {
        let agent = AgentId::new(1);
        let command = |name: &str| SlashCommand {
            name: name.to_owned(),
            description: String::new(),
            kind: CommandKind::Command,
        };
        let pick = |field: &str, down: bool| {
            let mut drafts = Drafts::default();
            let mut state = state(AgentStatus::Idle, None);
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.details = Loadable::Loaded(AgentDetails {
                    commands: vec![command("review"), command("rename-branch")],
                    ..AgentDetails::default()
                });
            }
            let typed = actions::composer_input(&state, &mut drafts, agent, "/re".to_owned());
            if let Some(change) = typed {
                actions::change_palette(&mut state, agent, change);
            }
            if down {
                handle_key(
                    &mut state,
                    &mut drafts,
                    &press(KEY_DOWN, KeyTarget::Composer),
                );
            }
            let enter = KeyPress {
                composer_text: field.to_owned(),
                ..press(KEY_ENTER, KeyTarget::Composer)
            };
            let requests = handle_key(&mut state, &mut drafts, &enter);
            let text = drafts.draft(agent).map(|draft| draft.text.clone());
            (requests.is_empty(), text)
        };

        assert_eq!(
            pick("/ren", false),
            (true, Some("/rename-branch ".to_owned()))
        );
        // The same text keeps the highlight the keys moved.
        assert_eq!(
            pick("/re", true),
            (true, Some("/rename-branch ".to_owned()))
        );
    }

    #[test]
    fn escape_closes_the_palette_whatever_opened_it() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let agent = AgentId::new(1);
        let escape = press(KEY_ESCAPE, KeyTarget::Field);

        actions::toggle_palette(&mut state, &mut drafts, agent);
        handle_key(&mut state, &mut drafts, &escape);
        let after_button = state.ui.popover;
        let typed = actions::composer_input(&state, &mut drafts, agent, "/".to_owned());
        if let Some(change) = typed {
            actions::change_palette(&mut state, agent, change);
        }
        handle_key(&mut state, &mut drafts, &escape);

        assert_eq!(after_button, None);
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn escape_does_not_stop_an_agent_that_waits_for_you() {
        let mut drafts = Drafts::default();
        let mut waiting = state(AgentStatus::NeedsYou, Some(question()));
        let mut idle = state(AgentStatus::Idle, None);

        // The question's "Other" field is a field like any other.
        let from_field = handle_key(
            &mut waiting,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        let from_page = handle_key(
            &mut waiting,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Page),
        );
        let from_composer = handle_key(
            &mut idle,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );

        assert!(from_field.is_empty());
        assert!(from_page.is_empty());
        assert!(from_composer.is_empty());
    }

    #[test]
    fn escape_in_a_field_closes_a_menu_or_a_dialog_before_stopping() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        state.ui.popover = Some(Popover::Model);

        let menu = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        actions::toggle_switcher(&mut state, &mut drafts);
        let dialog = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        let stop = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );

        assert!(menu.is_empty() && dialog.is_empty());
        assert_eq!((state.ui.popover, &state.ui.dialog), (None, &None));
        assert!(matches!(
            stop.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentInterrupt(_))
        ));
    }

    #[test]
    fn y_and_n_answer_an_approval_from_the_page_or_an_empty_composer() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(approval()));

        let allowed = handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page));
        let denied = handle_key(&mut state, &mut drafts, &press("N", KeyTarget::Composer));

        assert!(matches!(
            sent_answer(&allowed),
            Some(Answer::Approval(answer)) if answer.decision == ApprovalDecision::Allow
        ));
        assert!(matches!(
            sent_answer(&denied),
            Some(Answer::Approval(answer)) if answer.decision == ApprovalDecision::Deny
        ));
    }

    #[test]
    fn letters_in_a_composer_with_text_are_typing() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(approval()));
        let typing = KeyPress {
            composer_text: "no, ".to_owned(),
            ..press("y", KeyTarget::Composer)
        };

        assert!(handle_key(&mut state, &mut drafts, &typing).is_empty());
        assert!(handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Field)).is_empty());
    }

    #[test]
    fn numbers_pick_and_enter_sends_a_question_answer() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(question()));

        handle_key(&mut state, &mut drafts, &press("2", KeyTarget::Page));
        let requests = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ENTER, KeyTarget::Composer),
        );

        assert!(matches!(
            sent_answer(&requests),
            Some(Answer::Question { answers }) if answers.first().is_some_and(|answer| answer.selected == ["All"])
        ));
    }

    #[test]
    fn enter_sends_the_composer_text_and_shift_enter_does_not() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let typed = KeyPress {
            composer_text: "Run the linter".to_owned(),
            ..press(KEY_ENTER, KeyTarget::Composer)
        };
        let new_line = KeyPress {
            shift: true,
            ..typed.clone()
        };

        assert!(handle_key(&mut state, &mut drafts, &new_line).is_empty());
        let sent = handle_key(&mut state, &mut drafts, &typed);
        assert!(matches!(
            sent.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentPrompt(params)) if params.prompt.text == "Run the linter"
        ));
    }

    #[test]
    fn alt_up_and_alt_down_move_the_selected_agent_except_from_a_field_or_a_dialog() {
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        // Equal positions list the newer agent first: agent 1 is at the bottom.
        state.ui.selected = Some(AgentId::new(1));
        let alt = |key: &str, target| KeyPress {
            alt: true,
            ..press(key, target)
        };

        let from_field = handle_key(&mut state, &mut drafts, &alt(KEY_UP, KeyTarget::Field));
        let from_composer = handle_key(&mut state, &mut drafts, &alt(KEY_UP, KeyTarget::Composer));
        let from_page = handle_key(&mut state, &mut drafts, &alt(KEY_DOWN, KeyTarget::Page));
        actions::toggle_switcher(&mut state, &mut drafts);
        let behind_a_dialog = handle_key(&mut state, &mut drafts, &alt(KEY_DOWN, KeyTarget::Page));

        let method =
            |requests: &[Outgoing]| requests.first().map(|outgoing| outgoing.request.method());
        assert!(from_field.is_empty());
        assert_eq!(
            method(&from_composer),
            Some(cockpit_protocol::Method::AgentMove)
        );
        assert_eq!(
            method(&from_page),
            Some(cockpit_protocol::Method::AgentMove)
        );
        assert!(behind_a_dialog.is_empty());
    }

    #[test]
    fn f2_opens_the_rows_title_field_and_moves_it_from_the_headers() {
        use crate::state::TitlePlace;

        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let place = |state: &AppState| state.ui.title_edit.as_ref().map(|edit| edit.place);

        handle_key(&mut state, &mut drafts, &press(KEY_F2, KeyTarget::Page));
        let from_page = place(&state);
        state.ui.title_edit = None;
        actions::start_rename(&mut state, &mut drafts, AgentId::new(1));
        handle_key(&mut state, &mut drafts, &press(KEY_F2, KeyTarget::Field));
        let from_header = place(&state);
        state.ui.title_edit = None;
        actions::toggle_switcher(&mut state, &mut drafts);
        handle_key(&mut state, &mut drafts, &press(KEY_F2, KeyTarget::Page));

        assert_eq!(from_page, Some(TitlePlace::Sidebar));
        assert_eq!(from_header, Some(TitlePlace::Sidebar));
        assert_eq!(place(&state), None, "nothing behind a dialog");
    }

    #[test]
    fn ctrl_shift_w_asks_to_close_the_selected_agent_and_confirms_when_asked() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        let close = KeyPress {
            shift: true,
            key: "W".to_owned(),
            ..ctrl("w")
        };

        let asked = handle_key(&mut state, &mut drafts, &close);
        let dialog = state.ui.dialog.clone();
        let confirmed = handle_key(&mut state, &mut drafts, &close);

        assert!(asked.is_empty());
        assert_eq!(dialog, Some(Dialog::CloseAgent(AgentId::new(1))));
        assert!(matches!(
            confirmed.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentClose(params)) if params.agent == AgentId::new(1)
        ));
        assert!(state.agents.is_empty());
    }

    #[test]
    fn key_targets_parse_from_the_script() {
        let targets: Vec<KeyTarget> =
            serde_json::from_str(r#"["composer","field","page"]"#).unwrap();

        assert_eq!(
            targets,
            [KeyTarget::Composer, KeyTarget::Field, KeyTarget::Page]
        );
    }

    #[test]
    fn page_keys_do_nothing_behind_a_dialog() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(approval()));
        actions::toggle_switcher(&mut state, &mut drafts);

        assert!(handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page)).is_empty());
    }
}

//! The keyboard shortcuts of R4-parts, resolved against the state. The page's script decides
//! which key presses reach here (`bridge`), so typing in a field is never taken over.

use cockpit_protocol::{AgentId, AgentStatus, ApprovalDecision, PlanDecision};
use serde::Deserialize;

use super::actions::{self, Direction};
use super::app_state::AppState;
use super::drafts::Drafts;
use super::requests::Outgoing;
use super::transcript::WaitingKind;
use super::ui_state::{PaletteOrigin, Popover};

/// `KeyboardEvent.key` values.
const KEY_ESCAPE: &str = "Escape";
const KEY_ENTER: &str = "Enter";
const KEY_TAB: &str = "Tab";
const KEY_UP: &str = "ArrowUp";
const KEY_DOWN: &str = "ArrowDown";

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
            "k" => actions::toggle_switcher(state),
            "n" => actions::open_new_agent(state),
            _ => {}
        }
        return Vec::new();
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
                return actions::palette_pick(state, drafts, agent, None).requests;
            }
            _ => {}
        }
        return Vec::new();
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
        AgentStatus, Answer, AnswerState, Approval, AskedQuestion, ItemBody, ItemId, PendingId,
        Question, QuestionOption, Request, TranscriptItem,
    };
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::ConnectionState;
    use crate::state::app_state::tests::{state_with, summary};
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
        assert!(matches!(state.ui.dialog, Some(Dialog::Switcher(_))));

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
        actions::toggle_switcher(&mut state);
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
        actions::toggle_switcher(&mut state);

        assert!(handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page)).is_empty());
    }
}

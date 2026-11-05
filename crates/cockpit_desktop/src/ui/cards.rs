//! The cards that wait for your answer: an approval, a question, a plan.

use std::path::PathBuf;

use cockpit_protocol::{
    AgentId, AnswerState, Approval, ApprovalDecision, ItemId, Plan, PlanDecision, Question,
    QuestionAnswer,
};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::render::{DiffRow, Language, diff_lines};
use crate::state::actions;
use crate::state::file_edits::relative_path;
use crate::ui::code::diff_row;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::markdown::MarkdownView;

/// The tool whose approval reads "run a command".
const COMMAND_TOOL: &str = "Bash";

/// Diff rows an approval shows; the rest is in the file view once allowed.
const APPROVAL_DIFF_ROWS: usize = 16;

fn card_class(waiting: bool, kind: &str) -> String {
    if waiting {
        format!("card {kind} waiting")
    } else {
        format!("card {kind}")
    }
}

#[component]
pub fn ApprovalCard(agent: AgentId, folder: PathBuf, approval: Approval) -> Element {
    let cockpit = use_context::<Cockpit>();
    let waiting = approval.answer == AnswerState::Waiting;
    let heading = if approval.tool == COMMAND_TOOL {
        "Claude asks to run a command".to_owned()
    } else if let Some(edit) = &approval.edit {
        format!(
            "Claude asks to change {}",
            relative_path(&folder, &edit.path).display()
        )
    } else {
        format!("Claude asks to use {}", approval.tool)
    };
    let rows: Vec<DiffRow> = approval
        .edit
        .as_ref()
        .map(|edit| {
            diff_lines(
                &edit.old_text,
                &edit.new_text,
                Language::from_path(&edit.path),
                2,
                1,
            )
        })
        .unwrap_or_default();
    let created = approval
        .edit
        .as_ref()
        .is_some_and(|edit| edit.old_text.is_empty());
    let outcome = match &approval.answer {
        AnswerState::Waiting => None,
        AnswerState::Answered(answer) => Some(match (answer.decision, &answer.message) {
            (ApprovalDecision::Allow, _) => "You allowed it.".to_owned(),
            (ApprovalDecision::Deny, Some(message)) => format!("You denied it: {message}"),
            (ApprovalDecision::Deny, None) => "You denied it.".to_owned(),
        }),
        AnswerState::Cancelled => Some("Withdrawn when the turn stopped.".to_owned()),
    };
    let pending = approval.pending.clone();
    let allow_pending = pending.clone();
    rsx! {
        div { class: card_class(waiting, "approval-card"),
            div { class: "card-head",
                Icon { kind: IconKind::Warning, size: 16 }
                span { "{heading}" }
            }
            if approval.edit.is_none() {
                code { class: "approval-command mono", "{approval.summary}" }
            }
            if !rows.is_empty() {
                div { class: "diff-box scroll-x",
                    for row in rows.iter().take(APPROVAL_DIFF_ROWS) {
                        {diff_row(row, created, None)}
                    }
                }
            }
            if let Some(reason) = &approval.reason {
                span { class: "card-note", "{reason}" }
            }
            if waiting {
                div { class: "card-actions",
                    button {
                        class: "button primary",
                        onclick: move |_| {
                            let pending = allow_pending.clone();
                            cockpit.change(|_| actions::answer_approval(agent, pending, ApprovalDecision::Allow));
                        },
                        span { class: "key-hint", "Y" }
                        "Allow once"
                    }
                    button {
                        class: "button warning-outline",
                        onclick: move |_| {
                            let pending = pending.clone();
                            cockpit.change(|_| actions::answer_approval(agent, pending, ApprovalDecision::Deny));
                        },
                        span { class: "key-hint", "N" }
                        "Deny"
                    }
                }
            }
            if let Some(outcome) = outcome {
                span { class: "card-note", "{outcome}" }
            }
        }
    }
}

#[component]
pub fn QuestionCard(agent: AgentId, item: ItemId, question: Question) -> Element {
    let cockpit = use_context::<Cockpit>();
    let draft = cockpit
        .state
        .read()
        .ui
        .questions
        .get(&question.pending)
        .cloned()
        .unwrap_or_default();
    let waiting = question.answer == AnswerState::Waiting;
    let ready = actions::question_answers(&question, Some(&draft)).is_some();
    let outcome = match &question.answer {
        AnswerState::Waiting => None,
        AnswerState::Answered(answers) => Some(format!("You answered: {}", answers_text(answers))),
        AnswerState::Cancelled => Some("Withdrawn before an answer.".to_owned()),
    };
    let most_options = question
        .questions
        .iter()
        .map(|asked| asked.options.len())
        .max()
        .unwrap_or(0)
        .min(9);
    let hint = if most_options > 0 {
        format!("Press 1 to {most_options} to pick, Enter to send")
    } else {
        "Enter to send".to_owned()
    };
    rsx! {
        div { class: card_class(waiting, "question-card"),
            for (asked_index, asked) in question.questions.iter().enumerate() {
                div { key: "{asked_index}", class: "question",
                    div { class: "card-head",
                        Icon { kind: IconKind::Question, size: 16 }
                        span { "Claude asks a question" }
                        if !asked.header.is_empty() {
                            span { class: "question-chip", "{asked.header}" }
                        }
                    }
                    span { class: "question-text", "{asked.text}" }
                    if waiting {
                        div { class: "question-options",
                            for (option_index, option) in asked.options.iter().enumerate() {
                                {
                                    let picked = draft
                                        .picks
                                        .get(asked_index)
                                        .is_some_and(|picks| picks.contains(&option_index));
                                    let number = option_index + 1;
                                    let question = question.clone();
                                    rsx! {
                                        button {
                                            key: "{option_index}",
                                            class: if picked { "option picked" } else { "option" },
                                            "aria-pressed": if picked { "true" } else { "false" },
                                            onclick: move |_| {
                                                let question = question.clone();
                                                cockpit.update(|state| {
                                                    actions::pick_option(state, &question, asked_index, option_index);
                                                });
                                            },
                                            span { class: "option-key mono", "{number}" }
                                            span { class: "option-text",
                                                span { class: "option-label", "{option.label}" }
                                                if !option.description.is_empty() {
                                                    span { class: "option-description", "{option.description}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            {
                                let question = question.clone();
                                let other = draft.other.get(asked_index).cloned().unwrap_or_default();
                                rsx! {
                                    input {
                                        class: "text-field",
                                        r#type: "text",
                                        "aria-label": "Other answer",
                                        placeholder: "Other answer: type your own",
                                        value: "{other}",
                                        oninput: move |event: FormEvent| {
                                            let question = question.clone();
                                            cockpit.update(|state| {
                                                actions::set_other_answer(state, &question, asked_index, event.value());
                                            });
                                        },
                                        onkeydown: move |event: KeyboardEvent| {
                                            if event.key() == Key::Enter {
                                                cockpit.change(|state| actions::submit_question(state, agent, item));
                                            }
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if waiting {
                div { class: "card-actions",
                    button {
                        class: if ready { "button primary" } else { "button disabled" },
                        disabled: !ready,
                        onclick: move |_| cockpit.change(|state| actions::submit_question(state, agent, item)),
                        "Send the answer"
                    }
                    span { class: "card-note", "{hint}" }
                }
            }
            if let Some(outcome) = outcome {
                span { class: "card-note", "{outcome}" }
            }
        }
    }
}

fn answers_text(answers: &[QuestionAnswer]) -> String {
    let parts: Vec<String> = answers
        .iter()
        .map(|answer| {
            let mut words: Vec<String> = answer.selected.clone();
            if let Some(other) = &answer.other {
                words.push(other.clone());
            }
            words.join(", ")
        })
        .collect();
    parts.join("; ")
}

#[component]
pub fn PlanCard(agent: AgentId, plan: Plan) -> Element {
    let cockpit = use_context::<Cockpit>();
    let waiting = plan.answer == AnswerState::Waiting;
    let outcome = match &plan.answer {
        AnswerState::Waiting => None,
        AnswerState::Answered(answer) => Some(match answer.decision {
            PlanDecision::AcceptEdits => {
                "You approved the plan. Claude edits files without asking.".to_owned()
            }
            PlanDecision::AskBeforeEdits => {
                "You approved the plan. Claude asks before each edit.".to_owned()
            }
            PlanDecision::KeepPlanning => match &answer.message {
                Some(message) => format!("You asked Claude to keep planning: {message}"),
                None => "You asked Claude to keep planning.".to_owned(),
            },
        }),
        AnswerState::Cancelled => Some("Withdrawn when the turn stopped.".to_owned()),
    };
    let decide = move |decision: PlanDecision| {
        let pending = plan.pending.clone();
        move |_: MouseEvent| {
            let pending = pending.clone();
            cockpit.change(|_| actions::answer_plan(agent, pending, decision));
        }
    };
    rsx! {
        div { class: card_class(waiting, "plan-card"),
            div { class: "card-head",
                Icon { kind: IconKind::List, size: 16 }
                span { "Claude proposes a plan" }
            }
            MarkdownView { agent, markdown: plan.markdown.clone() }
            if waiting {
                div { class: "card-actions",
                    button { class: "button primary", onclick: decide(PlanDecision::AcceptEdits),
                        span { class: "key-hint", "Y" }
                        "Yes, accept edits"
                    }
                    button { class: "button", onclick: decide(PlanDecision::AskBeforeEdits),
                        "Yes, ask before edits"
                    }
                    button { class: "button warning-outline", onclick: decide(PlanDecision::KeepPlanning),
                        span { class: "key-hint", "N" }
                        "No, keep planning"
                    }
                }
            }
            if let Some(outcome) = outcome {
                span { class: "card-note", "{outcome}" }
            }
        }
    }
}

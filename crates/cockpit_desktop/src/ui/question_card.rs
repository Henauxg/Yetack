//! The card of a question Claude asks. While it waits, it shows one question at a time behind
//! tabs. Once answered or withdrawn, it folds to one line that opens on the full questions.

use cockpit_protocol::{
    AgentId, AnswerState, AskedQuestion, ItemId, Question, QuestionAnswer, Selection,
};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::{QuestionDraft, actions};
use crate::ui::icons::{Icon, IconKind};

#[component]
pub fn QuestionCard(agent: AgentId, item: ItemId, question: Question) -> Element {
    if question.answer == AnswerState::Waiting {
        rsx! {
            WaitingQuestion { agent, item, question }
        }
    } else {
        rsx! {
            EndedQuestion { question }
        }
    }
}

/// A tab's label: the question's header, or its place when Claude gave none.
fn tab_label(asked: &AskedQuestion, index: usize) -> String {
    if asked.header.is_empty() {
        format!("Question {}", index + 1)
    } else {
        asked.header.clone()
    }
}

#[component]
fn WaitingQuestion(agent: AgentId, item: ItemId, question: Question) -> Element {
    let cockpit: Cockpit = use_context();
    // Only this question's draft: typing in the composer leaves the card alone.
    let draft = use_memo(use_reactive((&question.pending,), move |(pending,)| {
        let drafts = cockpit.drafts.read();
        drafts.questions.get(&pending).cloned().unwrap_or_default()
    }))();
    // The "Other" fields start from the draft, then follow the keyboard alone: a render that lags
    // behind typing would put an older text back.
    let other_seeds = use_hook(|| draft.other.clone());
    let count = question.questions.len();
    let shown = actions::shown_question(&question, Some(&draft));
    let ready = actions::question_answers(&question, Some(&draft)).is_some();
    let answered = (0..count).filter(|&asked| draft.answers(asked)).count();
    let title = if count == 1 {
        "Claude asks a question".to_owned()
    } else {
        format!("Claude asks {count} questions")
    };
    let next = question
        .questions
        .get(shown + 1)
        .map(|asked| tab_label(asked, shown + 1));
    let options = question
        .questions
        .get(shown)
        .map_or(0, |asked| asked.options.len().min(9));
    let enter_word = if ready || count == 1 { "send" } else { "next" };
    let send_label = if count == 1 {
        "Send the answer"
    } else {
        "Send the answers"
    };
    let next_question = question.clone();
    rsx! {
        div { class: "card question-card waiting",
            div { class: "question-head",
                Icon { kind: IconKind::Question, size: 16 }
                span { class: "question-title", "{title}" }
                span { class: "waiting-pill", "Waiting for you" }
                if count > 1 {
                    span { class: "question-count", "{answered} of {count} answered" }
                }
            }
            if count > 1 {
                div { class: "question-tabs", role: "tablist",
                    for (asked_index, asked) in question.questions.iter().enumerate() {
                        {
                            let question = question.clone();
                            let current = asked_index == shown;
                            let done = draft.answers(asked_index);
                            let label = tab_label(asked, asked_index);
                            rsx! {
                                button {
                                    key: "{asked_index}",
                                    class: if current { "question-tab current" } else { "question-tab" },
                                    role: "tab",
                                    aria_selected: current,
                                    onclick: move |_| {
                                        let question = question.clone();
                                        cockpit.with_drafts(|_, drafts| actions::show_question(drafts, &question, asked_index));
                                    },
                                    "{label}"
                                    if done {
                                        span { class: "tab-done", Icon { kind: IconKind::Check, size: 12 } }
                                    } else {
                                        span { class: "tab-open", aria_hidden: true }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "question-panels",
                for asked_index in 0..count {
                    QuestionPanel {
                        key: "{asked_index}",
                        agent,
                        item,
                        question: question.clone(),
                        asked_index,
                        current: asked_index == shown,
                        draft: draft.clone(),
                        other_seed: other_seeds.get(asked_index).cloned().unwrap_or_default(),
                    }
                }
            }
            div { class: "card-actions",
                if let Some(next) = next {
                    button {
                        class: "button",
                        onclick: move |_| {
                            let question = next_question.clone();
                            cockpit.with_drafts(|_, drafts| actions::show_question(drafts, &question, shown + 1));
                        },
                        "Next: {next}"
                    }
                }
                button {
                    class: if ready { "button primary" } else { "button disabled" },
                    disabled: !ready,
                    onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::submit_question(state, drafts, agent, item)),
                    "{send_label}"
                }
                span { class: "question-keys",
                    if options > 0 {
                        span { class: "key-hint", "1" }
                        "–"
                        span { class: "key-hint", "{options}" }
                        " pick · "
                    }
                    if count > 1 {
                        span { class: "key-hint", "←" }
                        span { class: "key-hint", "→" }
                        " question · "
                    }
                    span { class: "key-hint", "Enter" }
                    " {enter_word}"
                }
            }
        }
    }
}

/// One question of a waiting card. Every panel stays in the card, the hidden ones invisible, so
/// the card keeps the height of its tallest question.
#[component]
fn QuestionPanel(
    agent: AgentId,
    item: ItemId,
    question: Question,
    asked_index: usize,
    current: bool,
    draft: QuestionDraft,
    other_seed: String,
) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(asked) = question.questions.get(asked_index).cloned() else {
        return rsx! {};
    };
    let multiple = asked.selection == Selection::Multiple;
    let picks = draft.picks.get(asked_index).cloned().unwrap_or_default();
    let typed = draft
        .other
        .get(asked_index)
        .is_some_and(|text| !text.trim().is_empty());
    // A pick replaces the "Other" text of a single-choice question.
    let other_on = typed && (multiple || picks.is_empty());
    let mark = if multiple {
        "option-mark box"
    } else {
        "option-mark"
    };
    let role = if multiple { "checkbox" } else { "radio" };
    let other_question = question.clone();
    rsx! {
        div {
            class: if current { "question-panel current" } else { "question-panel" },
            role: "tabpanel",
            inert: (!current).then_some(true),
            aria_hidden: !current,
            span { class: "question-text",
                "{asked.text}"
                if multiple {
                    span { class: "question-kind", "Pick any" }
                }
            }
            div { class: "option-list", role: if multiple { "group" } else { "radiogroup" },
                for (option_index, option) in asked.options.iter().enumerate() {
                    {
                        let picked = picks.contains(&option_index);
                        let question = question.clone();
                        let number = option_index + 1;
                        rsx! {
                            button {
                                key: "{option_index}",
                                class: if picked { "option-row picked" } else { "option-row" },
                                role,
                                aria_checked: picked,
                                onclick: move |_| {
                                    let question = question.clone();
                                    cockpit.with_drafts(|_, drafts| actions::pick_option(drafts, &question, asked_index, option_index));
                                },
                                span { class: mark }
                                span { class: "option-text",
                                    span { class: "option-label", "{option.label}" }
                                    if !option.description.is_empty() {
                                        span { class: "option-description", "{option.description}" }
                                    }
                                }
                                if number <= 9 {
                                    span { class: "option-key mono", "{number}" }
                                }
                            }
                        }
                    }
                }
                label { class: if other_on { "option-row other picked" } else { "option-row other" },
                    span { class: mark }
                    input {
                        class: "other-field",
                        r#type: "text",
                        aria_label: "Other answer",
                        placeholder: "Other: type your own…",
                        initial_value: "{other_seed}",
                        oninput: move |event: FormEvent| {
                            let question = other_question.clone();
                            cockpit.with_drafts(|_, drafts| {
                                actions::set_other_answer(drafts, &question, asked_index, event.value());
                            });
                        },
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter {
                                cockpit.change_with_drafts(|state, drafts| actions::enter_on_question(state, drafts, agent, item));
                            }
                        },
                    }
                }
            }
        }
    }
}

/// An answer in words: the picked labels, then the "Other" text in quotes.
fn answer_words(answer: &QuestionAnswer) -> String {
    let mut words = answer.selected.clone();
    if let Some(other) = &answer.other {
        words.push(format!("“{other}”"));
    }
    words.join(", ")
}

/// A question once answered or withdrawn: one line, which opens on every question in full.
#[component]
fn EndedQuestion(question: Question) -> Element {
    let mut open = use_signal(|| false);
    let answers: &[QuestionAnswer] = match &question.answer {
        AnswerState::Answered(answers) => answers,
        AnswerState::Waiting | AnswerState::Cancelled => &[],
    };
    let answered = matches!(question.answer, AnswerState::Answered(_));
    let count = question.questions.len();
    let what = match (answered, count) {
        (false, _) => "Withdrawn before an answer".to_owned(),
        (true, 1) => "You answered".to_owned(),
        (true, count) => format!("You answered {count} questions"),
    };
    let is_open = open();
    rsx! {
        div { class: if is_open { "question-ended open" } else { "question-ended" },
            button {
                class: "question-ended-line",
                aria_expanded: is_open,
                onclick: move |_| open.toggle(),
                span { class: if answered { "ended-icon answered" } else { "ended-icon" },
                    if answered {
                        Icon { kind: IconKind::Check, size: 14 }
                    } else {
                        Icon { kind: IconKind::Close, size: 14 }
                    }
                }
                span { class: "ended-what", "{what}" }
                span { class: "ended-summary",
                    for (asked_index, asked) in question.questions.iter().enumerate() {
                        span { key: "{asked_index}", class: "ended-pair",
                            if !asked.header.is_empty() {
                                span { class: "ended-header", "{asked.header}" }
                            }
                            if let Some(answer) = answers.get(asked_index) {
                                "{answer_words(answer)}"
                            }
                        }
                    }
                }
                span { class: "ended-chevron", Icon { kind: IconKind::ChevronDown, size: 14 } }
            }
            div { class: "question-ended-fold", inert: (!is_open).then_some(true),
                div { class: "question-ended-clip",
                    div { class: "question-ended-body",
                        for (asked_index, asked) in question.questions.iter().enumerate() {
                            EndedPart {
                                key: "{asked_index}",
                                asked: asked.clone(),
                                index: asked_index,
                                answer: answers.get(asked_index).cloned(),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One question in an opened ended card: its text, the answer, and the options left out.
#[component]
fn EndedPart(asked: AskedQuestion, index: usize, answer: Option<QuestionAnswer>) -> Element {
    let picked: Vec<(String, String)> = asked
        .options
        .iter()
        .filter(|option| {
            answer
                .as_ref()
                .is_some_and(|answer| answer.selected.contains(&option.label))
        })
        .map(|option| (option.label.clone(), option.description.clone()))
        .collect();
    let rest: Vec<String> = asked
        .options
        .iter()
        .filter(|option| {
            !answer
                .as_ref()
                .is_some_and(|answer| answer.selected.contains(&option.label))
        })
        .map(|option| option.label.clone())
        .collect();
    let rest_label = if answer.is_some() {
        "Not picked"
    } else {
        "Options"
    };
    let other = answer.and_then(|answer| answer.other);
    let rest_text = rest.join(" · ");
    rsx! {
        div { class: "ended-question",
            span { class: "question-tag", "{tab_label(&asked, index)}" }
            div { class: "ended-question-body",
                span { class: "question-text",
                    "{asked.text}"
                    if asked.selection == Selection::Multiple {
                        span { class: "question-kind", "Pick any" }
                    }
                }
                for (label, description) in picked {
                    div { key: "{label}", class: "ended-answer",
                        Icon { kind: IconKind::Check, size: 14 }
                        span { class: "option-label", "{label}" }
                        if !description.is_empty() {
                            span { class: "option-description", "{description}" }
                        }
                    }
                }
                if let Some(other) = other {
                    div { class: "ended-answer",
                        Icon { kind: IconKind::Check, size: 14 }
                        span { class: "option-label", "“{other}”" }
                        span { class: "option-description", "your own answer" }
                    }
                }
                if !rest.is_empty() {
                    span { class: "ended-rest", "{rest_label}: {rest_text}" }
                }
            }
        }
    }
}

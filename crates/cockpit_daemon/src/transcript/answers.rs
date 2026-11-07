//! A client's answer, as the `control_response` Claude Code waits for.

use claude_protocol::{PermissionDecision, QuestionAnswer as ClaudeQuestionAnswer, ToolInput};
use cockpit_protocol::{
    ApprovalAnswer, ApprovalDecision, AskedQuestion, PermissionMode, PlanAnswer, PlanDecision,
    QuestionAnswer, QuestionOption, Selection,
};
use serde_json::{Map, Value};

/// What Claude reads when a client denies a call without a message.
const DEFAULT_DENIAL: &str = "The user denied this tool call.";
/// What Claude reads when a client keeps it planning without a message.
const DEFAULT_KEEP_PLANNING: &str = "The user wants to keep planning.";

pub(crate) fn approval(input: Map<String, Value>, answer: &ApprovalAnswer) -> PermissionDecision {
    match answer.decision {
        ApprovalDecision::Allow => PermissionDecision::allow(input),
        ApprovalDecision::Deny => PermissionDecision::deny(
            answer
                .message
                .clone()
                .unwrap_or_else(|| DEFAULT_DENIAL.to_owned()),
        ),
    }
}

/// `questions` and `answers` are in the same order, one answer per question.
pub(crate) fn questions(
    input: Map<String, Value>,
    questions: &[AskedQuestion],
    answers: &[QuestionAnswer],
) -> PermissionDecision {
    let answers: Vec<ClaudeQuestionAnswer> = questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| {
            let mut choices = answer.selected.clone();
            choices.extend(answer.other.clone());
            ClaudeQuestionAnswer {
                question: question.text.clone(),
                choices,
            }
        })
        .collect();
    PermissionDecision::answer_questions(input, answers)
}

/// The decision, and the mode to switch to after an approved plan.
pub(crate) fn plan(
    input: Map<String, Value>,
    answer: &PlanAnswer,
) -> (PermissionDecision, Option<PermissionMode>) {
    match answer.decision {
        PlanDecision::AcceptEdits => (
            PermissionDecision::allow(input),
            Some(PermissionMode::AcceptEdits),
        ),
        PlanDecision::AskBeforeEdits => (
            PermissionDecision::allow(input),
            Some(PermissionMode::Default),
        ),
        PlanDecision::KeepPlanning => (
            PermissionDecision::deny(
                answer
                    .message
                    .clone()
                    .unwrap_or_else(|| DEFAULT_KEEP_PLANNING.to_owned()),
            ),
            None,
        ),
    }
}

/// The questions of an `AskUserQuestion` call, as the transcript shows them.
pub(crate) fn asked_questions(input: &ToolInput) -> Option<Vec<AskedQuestion>> {
    let ToolInput::AskUserQuestion(asked) = input else {
        return None;
    };
    let questions: Vec<AskedQuestion> = asked
        .questions
        .iter()
        .map(|question| AskedQuestion {
            header: question.header.clone(),
            text: question.question.clone(),
            options: question
                .options
                .iter()
                .map(|option| QuestionOption {
                    label: option.label.clone(),
                    description: option.description.clone(),
                })
                .collect(),
            selection: if question.multi_select {
                Selection::Multiple
            } else {
                Selection::Single
            },
        })
        .collect();
    Some(questions)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object");
        };
        map
    }

    #[test]
    fn a_denial_without_a_message_gets_one() {
        let answer = ApprovalAnswer {
            decision: ApprovalDecision::Deny,
            message: None,
        };

        let decision = approval(Map::new(), &answer);

        assert_eq!(
            decision,
            PermissionDecision::deny(DEFAULT_DENIAL.to_owned())
        );
    }

    #[test]
    fn question_answers_join_the_choices_and_the_other_text() {
        let input = object(json!({"questions": [{"question": "Which database?"}]}));
        let asked = vec![AskedQuestion {
            header: "Database".to_owned(),
            text: "Which database?".to_owned(),
            options: Vec::new(),
            selection: Selection::Multiple,
        }];
        let answers = vec![QuestionAnswer {
            selected: vec!["Postgres".to_owned()],
            other: Some("SQLite for tests".to_owned()),
        }];

        let decision = questions(input, &asked, &answers);

        let PermissionDecision::Allow { updated_input, .. } = decision else {
            panic!("expected an allow");
        };
        assert_eq!(
            updated_input["answers"],
            json!({"Which database?": "Postgres, SQLite for tests"})
        );
    }

    #[test]
    fn plan_decisions_pick_the_mode() {
        let answer = |decision| PlanAnswer {
            decision,
            message: None,
        };

        assert_eq!(
            plan(Map::new(), &answer(PlanDecision::AcceptEdits)).1,
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            plan(Map::new(), &answer(PlanDecision::AskBeforeEdits)).1,
            Some(PermissionMode::Default)
        );
        let (decision, mode) = plan(Map::new(), &answer(PlanDecision::KeepPlanning));
        assert_eq!(mode, None);
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }
}

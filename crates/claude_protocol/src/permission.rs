use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::open_word::OpenWord;
use crate::tool_input::QuestionAnswer;
use crate::wire;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[serde(into = "&'static str")]
#[strum(serialize_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
    DontAsk,
    Auto,
}

/// A change to the permission rules or mode.
///
/// A `can_use_tool` request suggests some. Echoing one back in
/// [`PermissionDecision::Allow`] applies it, so that matching calls skip the prompt.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PermissionUpdate {
    AddRules(RulesUpdate),
    ReplaceRules(RulesUpdate),
    RemoveRules(RulesUpdate),
    SetMode(ModeUpdate),
    AddDirectories(DirectoriesUpdate),
    RemoveDirectories(DirectoriesUpdate),
    /// An update of a type this crate does not know, as Claude Code wrote it.
    #[serde(untagged)]
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesUpdate {
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<OpenWord<PermissionBehavior>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<OpenWord<PermissionDestination>>,
}

/// A rule such as `Bash(npm test:*)`: the tool, and what the rule matches in its input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub tool_name: String,
    /// `None` matches every call of the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<OpenWord<PermissionMode>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<OpenWord<PermissionDestination>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoriesUpdate {
    #[serde(default)]
    pub directories: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<OpenWord<PermissionDestination>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

/// Where an update is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "camelCase")]
pub enum PermissionDestination {
    /// `~/.claude/settings.json`.
    UserSettings,
    /// `.claude/settings.json` in the project.
    ProjectSettings,
    /// `.claude/settings.local.json` in the project.
    LocalSettings,
    /// This session only.
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "camelCase")]
enum PermissionUpdateType {
    AddRules,
    ReplaceRules,
    RemoveRules,
    SetMode,
    AddDirectories,
    RemoveDirectories,
}

impl<'de> Deserialize<'de> for PermissionUpdate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let value = Value::deserialize(deserializer)?;
        let Some(update_type) = wire::word::<PermissionUpdateType>(&value, wire::TYPE) else {
            return Ok(Self::Unknown(value));
        };
        let update = match update_type {
            PermissionUpdateType::AddRules => RulesUpdate::deserialize(&value).map(Self::AddRules),
            PermissionUpdateType::ReplaceRules => {
                RulesUpdate::deserialize(&value).map(Self::ReplaceRules)
            }
            PermissionUpdateType::RemoveRules => {
                RulesUpdate::deserialize(&value).map(Self::RemoveRules)
            }
            PermissionUpdateType::SetMode => ModeUpdate::deserialize(&value).map(Self::SetMode),
            PermissionUpdateType::AddDirectories => {
                DirectoriesUpdate::deserialize(&value).map(Self::AddDirectories)
            }
            PermissionUpdateType::RemoveDirectories => {
                DirectoriesUpdate::deserialize(&value).map(Self::RemoveDirectories)
            }
        };
        update.map_err(D::Error::custom)
    }
}

/// The answer to a `can_use_tool` request.
///
/// For `ExitPlanMode`, allowing approves the plan and denying keeps Claude planning, with the
/// message as the user's feedback.
#[must_use]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum PermissionDecision {
    #[serde(rename_all = "camelCase")]
    Allow {
        /// The input the tool runs with: the request's input, or a changed one.
        updated_input: Map<String, Value>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        updated_permissions: Vec<PermissionUpdate>,
    },
    Deny {
        /// Claude reads it, so it can say why or what to do instead.
        message: String,
        /// Also ends the turn, as an interrupt does.
        #[serde(skip_serializing_if = "is_false")]
        interrupt: bool,
    },
}

impl PermissionDecision {
    /// Allows the call with the input of the request.
    pub fn allow(input: Map<String, Value>) -> Self {
        Self::Allow {
            updated_input: input,
            updated_permissions: Vec::new(),
        }
    }

    pub fn deny(message: String) -> Self {
        Self::Deny {
            message,
            interrupt: false,
        }
    }

    /// Allows an `AskUserQuestion` call with the user's answers.
    ///
    /// `input` is the request's input. Claude Code reads the answers from an `answers` object
    /// added to it, which maps each question's text to the chosen labels joined with `", "`.
    pub fn answer_questions(mut input: Map<String, Value>, answers: Vec<QuestionAnswer>) -> Self {
        let mut answers_by_question = Map::new();
        for answer in answers {
            answers_by_question.insert(answer.question, Value::String(answer.choices.join(", ")));
        }
        input.insert("answers".to_owned(), Value::Object(answers_by_question));
        Self::allow(input)
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_types_are_pinned() {
        let words: Vec<&str> = PermissionUpdateType::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();
        assert_eq!(
            words,
            [
                "addRules",
                "replaceRules",
                "removeRules",
                "setMode",
                "addDirectories",
                "removeDirectories",
            ]
        );
    }
}

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::open_word::OpenWord;
use crate::parse_error::ParseError;

/// The tools whose input this crate reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
pub enum ToolName {
    TodoWrite,
    AskUserQuestion,
    ExitPlanMode,
    Edit,
    Write,
    Bash,
    Read,
    /// Loads a skill: Claude Code then adds its instructions as a synthetic `user` line.
    Skill,
    /// Starts a subagent.
    Agent,
    /// The name older Claude Code versions give [`ToolName::Agent`].
    Task,
}

/// The input of a tool call, for the tools this crate knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInput {
    TodoWrite(TodoWriteInput),
    AskUserQuestion(AskUserQuestionInput),
    ExitPlanMode(ExitPlanModeInput),
    Edit(EditInput),
    Write(WriteInput),
    Bash(BashInput),
    Read(ReadInput),
    Skill(SkillInput),
    /// `Agent` or `Task`.
    Agent(AgentInput),
    /// Another tool: its raw input is all there is.
    Unknown,
}

impl ToolInput {
    /// Reads the input of a call to `tool_name`, from a `tool_use` block or a `can_use_tool`
    /// request.
    pub fn parse(tool_name: &str, input: &Map<String, Value>) -> Result<Self, ParseError> {
        let Ok(tool) = tool_name.parse::<ToolName>() else {
            return Ok(Self::Unknown);
        };
        let parsed = match tool {
            ToolName::TodoWrite => TodoWriteInput::deserialize(input).map(Self::TodoWrite),
            ToolName::AskUserQuestion => {
                AskUserQuestionInput::deserialize(input).map(Self::AskUserQuestion)
            }
            ToolName::ExitPlanMode => ExitPlanModeInput::deserialize(input).map(Self::ExitPlanMode),
            ToolName::Edit => EditInput::deserialize(input).map(Self::Edit),
            ToolName::Write => WriteInput::deserialize(input).map(Self::Write),
            ToolName::Bash => BashInput::deserialize(input).map(Self::Bash),
            ToolName::Read => ReadInput::deserialize(input).map(Self::Read),
            ToolName::Skill => SkillInput::deserialize(input).map(Self::Skill),
            ToolName::Agent | ToolName::Task => AgentInput::deserialize(input).map(Self::Agent),
        };
        parsed.map_err(|error| ParseError::MalformedToolInput { tool, error })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SkillInput {
    /// Without its slash, such as `review`.
    pub skill: String,
    pub args: Option<String>,
}

/// A subagent to start. Its own lines carry the call's id as their `parent_tool_use_id`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct AgentInput {
    /// A few words, such as "Survey the test folder".
    pub description: String,
    /// What the subagent is told to do.
    pub prompt: String,
    /// Such as `Explore` or `general-purpose`; `None` for the default one.
    pub subagent_type: Option<String>,
    /// The call returns at once, and the subagent runs as a background task.
    pub run_in_background: bool,
}

/// The task list, which each call replaces whole.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TodoWriteInput {
    pub todos: Vec<Todo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Todo {
    pub content: String,
    pub status: OpenWord<TodoStatus>,
    /// The task in the present continuous, shown while it runs: "Running the tests".
    pub active_form: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Questions for the user. Answer them with
/// [`PermissionDecision::answer_questions`](crate::PermissionDecision::answer_questions).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AskUserQuestionInput {
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// The full question, which is also the key of its answer.
    pub question: String,
    /// A short label, such as "Format".
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    /// Whether the user may pick several options.
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// The user's answer to one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionAnswer {
    /// The question's text.
    pub question: String,
    /// The labels of the options the user picked, or the text they typed instead.
    pub choices: Vec<String>,
}

/// The plan Claude asks to start on.
///
/// Approve it with [`PermissionDecision::allow`](crate::PermissionDecision::allow). Keep Claude
/// planning with [`PermissionDecision::deny`](crate::PermissionDecision::deny), whose message
/// tells Claude what to change.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExitPlanModeInput {
    /// Markdown.
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EditInput {
    pub file_path: PathBuf,
    pub old_string: String,
    pub new_string: String,
    /// Replaces every occurrence of `old_string`, not only the single one.
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WriteInput {
    pub file_path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BashInput {
    pub command: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReadInput {
    pub file_path: PathBuf,
    /// The line number to start reading from.
    pub offset: Option<u32>,
    /// How many lines to read.
    pub limit: Option<u32>,
}

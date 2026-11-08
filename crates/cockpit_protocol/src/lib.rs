//! The daemon API: ids, the transcript model, requests, responses, events and line framing.
//!
//! A connection carries one JSON object per line ([`FrameReader`], [`FrameWriter`]):
//!
//! ```text
//! client: {"type":"hello","protocol_version":4,"last_seq":41}
//! daemon: {"type":"welcome","protocol_version":4,"catchup":{"type":"replay"}}
//! daemon: {"type":"event","seq":42,"event":{"type":"agent_removed","agent":3}}
//! client: {"type":"request","id":1,"request":{"method":"agent.interrupt","params":{"agent":2}}}
//! daemon: {"type":"response","id":1,"result":{"ok":{"type":"done"}}}
//! ```
//!
//! [`ClientMessage`] and [`ServerMessage`] are the two directions. This crate does not know
//! Claude Code's wire format: the daemon converts it into these types.

mod account;
mod agent;
mod agent_ids;
mod details;
mod event;
mod frame;
mod ids;
mod machine_load;
mod position;
mod request;
mod response;
mod session;
mod settings;
mod stored_bool;
mod time;
mod transcript;
mod usage;

pub use account::{
    Account, AccountState, CredentialSource, EnvironmentCredentials, FOLDER_SEPARATOR,
    MAX_ACCOUNT_NAME_CHARS, SignIn, account_folder_name,
};
pub use agent::{
    AgentStatus, AgentSummary, Effort, EffortSource, PermissionMode, SignInWait, TaskProgress,
};
pub use details::{
    AgentDetails, BackgroundTask, BackgroundTaskKind, BackgroundTaskStatus, ChangedFile,
    CommandKind, ContextCategory, ContextUsage, McpScope, McpServer, McpServerStatus, McpTool,
    McpTransport, SlashCommand, Subagent, SubagentRun, SubagentStatus, SubagentToolCall,
    TaskListItem, TaskStatus, WaitingPrompt, WaitingState,
};
pub use event::{Event, EventRecord};
pub use frame::{Frame, FrameError, FrameReader, FrameWriter, MAX_LINE_BYTES};
pub use ids::{
    AccountId, AgentId, BackgroundTaskId, ClaudeSessionId, ItemId, PendingId, ProcessId, PromptId,
    RequestId, Seq,
};
pub use machine_load::{
    AgentHistory, AgentLoad, CommandLoad, Cores, LoadAverage, LoadHistory, MachineLoad,
    ResourceUse, ServerLoad, ServerPoint,
};
pub use position::{Placed, Position, list_order, move_agent, place_on_top};
pub use request::{
    AccountAddParams, AccountChangeParams, AccountParams, AgentParams, Answer, AnswerParams,
    COMMAND_PREFIX, FindFilesParams, FoldersListParams, HistoryParams, ListDirParams,
    MAX_TITLE_CHARS, McpRemoveParams, McpServerParams, McpToggleAction, McpToggleParams, Method,
    MoveParams, Prompt, PromptDelivery, PromptParams, QueuedPromptParams, ReadFileParams,
    RenameParams, Request, SessionResumeParams, SetEffortParams, SetModeParams, SetModelParams,
    StartParams, StopProcessParams, StopTaskParams,
};
pub use response::{
    Completeness, DirEntry, DirListing, EntryKind, EntryLink, ErrorBody, ErrorCode, HistoryPage,
    OlderItems, PastSession, ResponseBody, ResponseResult,
};
pub use session::{Catchup, ClientMessage, PROTOCOL_VERSION, ServerMessage, Snapshot};
pub use settings::Settings;
pub use stored_bool::{FromStoredBool, enum_or_stored_bool};
pub use time::Timestamp;
pub use transcript::{
    AnswerState, Approval, ApprovalAnswer, ApprovalDecision, AskedQuestion, FileEdit, Image,
    ImageMediaType, ItemBody, Plan, PlanAnswer, PlanDecision, Question, QuestionAnswer,
    QuestionOption, Selection, ToolCall, ToolCallStatus, TranscriptItem, TurnEnd, TurnOutcome,
    UserMessage,
};
pub use usage::{
    AccountPlanUsage, CostUsd, PlanUsage, RateLimitStatus, TokenUsage, UsageWindow, WindowUsage,
};

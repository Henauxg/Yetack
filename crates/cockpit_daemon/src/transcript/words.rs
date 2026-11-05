//! Words that both formats name: permission modes, statuses, scopes, windows.
//!
//! Each mapping matches every variant, so a new word in either crate does not compile until it is
//! mapped here.

use claude_protocol::{
    McpScope as ClaudeMcpScope, McpServerState, OpenWord, PermissionMode as ClaudePermissionMode,
    RateLimitStatus as ClaudeRateLimitStatus, RateLimitType, TaskStatus as ClaudeTaskStatus,
    TodoStatus,
};
use cockpit_protocol::{
    BackgroundTaskStatus, Effort, ImageMediaType, McpScope, McpServerStatus, PermissionMode,
    RateLimitStatus, TaskStatus, UsageWindow,
};
use serde::Serialize;
use serde_json::Value;

pub(crate) fn mode_to_cockpit(mode: ClaudePermissionMode) -> PermissionMode {
    match mode {
        ClaudePermissionMode::Default => PermissionMode::Default,
        ClaudePermissionMode::AcceptEdits => PermissionMode::AcceptEdits,
        ClaudePermissionMode::Plan => PermissionMode::Plan,
        ClaudePermissionMode::BypassPermissions => PermissionMode::BypassPermissions,
        ClaudePermissionMode::DontAsk => PermissionMode::DontAsk,
        ClaudePermissionMode::Auto => PermissionMode::Auto,
    }
}

pub(crate) fn mode_to_claude(mode: PermissionMode) -> ClaudePermissionMode {
    match mode {
        PermissionMode::Default => ClaudePermissionMode::Default,
        PermissionMode::AcceptEdits => ClaudePermissionMode::AcceptEdits,
        PermissionMode::Plan => ClaudePermissionMode::Plan,
        PermissionMode::Auto => ClaudePermissionMode::Auto,
        PermissionMode::DontAsk => ClaudePermissionMode::DontAsk,
        PermissionMode::BypassPermissions => ClaudePermissionMode::BypassPermissions,
    }
}

/// The level as `/effort` and `--effort` take it: the protocol's own word.
pub(crate) fn effort_word(effort: Effort) -> Option<String> {
    serde_word(effort)
}

/// Reads a level Claude Code wrote; `None` for one the cockpit does not know.
pub(crate) fn effort_from_word(word: &str) -> Option<Effort> {
    serde_json::from_value(Value::String(word.to_owned())).ok()
}

/// Such as `image/png`: the protocol's own word.
pub(crate) fn media_type_word(media_type: ImageMediaType) -> Option<String> {
    serde_word(media_type)
}

/// Reads a media type Claude Code wrote; `None` for a type the cockpit does not show.
pub(crate) fn media_type_from_word(word: &str) -> Option<ImageMediaType> {
    serde_json::from_value(Value::String(word.to_owned())).ok()
}

/// The scopes `claude mcp remove -s` accepts.
pub(crate) fn removable_scope(scope: McpScope) -> Option<ClaudeMcpScope> {
    match scope {
        McpScope::User => Some(ClaudeMcpScope::User),
        McpScope::Project => Some(ClaudeMcpScope::Project),
        McpScope::Local => Some(ClaudeMcpScope::Local),
        McpScope::Dynamic | McpScope::Enterprise | McpScope::Managed | McpScope::ClaudeAi => None,
    }
}

pub(crate) fn mcp_scope(scope: &OpenWord<ClaudeMcpScope>) -> Option<McpScope> {
    let OpenWord::Known(scope) = scope else {
        return None;
    };
    Some(match scope {
        ClaudeMcpScope::Local => McpScope::Local,
        ClaudeMcpScope::User => McpScope::User,
        ClaudeMcpScope::Project => McpScope::Project,
        ClaudeMcpScope::Claudeai => McpScope::ClaudeAi,
        ClaudeMcpScope::Managed => McpScope::Managed,
    })
}

/// A status this daemon does not know reads as `Pending`: the server is neither usable nor
/// known to have failed.
pub(crate) fn mcp_status(state: &OpenWord<McpServerState>) -> McpServerStatus {
    let OpenWord::Known(state) = state else {
        return McpServerStatus::Pending;
    };
    match state {
        McpServerState::Connected => McpServerStatus::Connected,
        McpServerState::Failed => McpServerStatus::Failed,
        McpServerState::NeedsAuth => McpServerStatus::NeedsAuth,
        McpServerState::Pending => McpServerStatus::Pending,
        McpServerState::Disabled => McpServerStatus::Disabled,
    }
}

pub(crate) fn usage_window(window: &OpenWord<RateLimitType>) -> Option<UsageWindow> {
    let OpenWord::Known(window) = window else {
        return None;
    };
    Some(match window {
        RateLimitType::FiveHour => UsageWindow::FiveHour,
        RateLimitType::SevenDay => UsageWindow::SevenDay,
        RateLimitType::SevenDayOpus => UsageWindow::SevenDayOpus,
        RateLimitType::SevenDaySonnet => UsageWindow::SevenDaySonnet,
        RateLimitType::Overage => UsageWindow::Overage,
    })
}

pub(crate) fn rate_limit_status(
    status: &OpenWord<ClaudeRateLimitStatus>,
) -> Option<RateLimitStatus> {
    let OpenWord::Known(status) = status else {
        return None;
    };
    Some(match status {
        ClaudeRateLimitStatus::Allowed => RateLimitStatus::Allowed,
        ClaudeRateLimitStatus::AllowedWarning => RateLimitStatus::AllowedWarning,
        ClaudeRateLimitStatus::Rejected => RateLimitStatus::Rejected,
    })
}

/// A status this daemon does not know reads as `Pending`.
pub(crate) fn todo_status(status: &OpenWord<TodoStatus>) -> TaskStatus {
    let OpenWord::Known(status) = status else {
        return TaskStatus::Pending;
    };
    match status {
        TodoStatus::Pending => TaskStatus::Pending,
        TodoStatus::InProgress => TaskStatus::InProgress,
        TodoStatus::Completed => TaskStatus::Completed,
    }
}

/// `None` while the task still runs.
pub(crate) fn ended_task_status(status: ClaudeTaskStatus) -> Option<BackgroundTaskStatus> {
    match status {
        ClaudeTaskStatus::Pending | ClaudeTaskStatus::Running | ClaudeTaskStatus::Paused => None,
        ClaudeTaskStatus::Completed => Some(BackgroundTaskStatus::Done),
        ClaudeTaskStatus::Failed => Some(BackgroundTaskStatus::Failed),
        ClaudeTaskStatus::Stopped | ClaudeTaskStatus::Killed => Some(BackgroundTaskStatus::Stopped),
    }
}

fn serde_word<T: Serialize>(value: T) -> Option<String> {
    match serde_json::to_value(value).ok()? {
        Value::String(word) => Some(word),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use strum::VariantArray;

    use super::*;

    #[test]
    fn modes_round_trip() {
        for &mode in PermissionMode::VARIANTS {
            assert_eq!(mode_to_cockpit(mode_to_claude(mode)), mode);
        }
    }

    #[test]
    fn every_effort_and_media_type_has_a_word() {
        for &effort in Effort::VARIANTS {
            assert!(effort_word(effort).is_some(), "{effort:?}");
        }
        for &media_type in ImageMediaType::VARIANTS {
            let word = media_type_word(media_type).unwrap();
            assert_eq!(media_type_from_word(&word), Some(media_type));
        }
        assert_eq!(effort_word(Effort::Xhigh).as_deref(), Some("xhigh"));
        for &effort in Effort::VARIANTS {
            let word = effort_word(effort).unwrap_or_default();
            assert_eq!(effort_from_word(&word), Some(effort), "{word}");
        }
        assert_eq!(effort_from_word("auto"), None);
        assert_eq!(media_type_from_word("image/tiff"), None);
    }
}

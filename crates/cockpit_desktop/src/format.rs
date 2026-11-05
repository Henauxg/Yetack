//! How values read on screen: durations, token counts, costs, moments, and the words for
//! statuses, modes, models and scopes.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, NaiveDateTime};
use cockpit_protocol::{
    AgentStatus, CostUsd, Effort, McpScope, McpServerStatus, PermissionMode, Timestamp, UsageWindow,
};

/// A model Claude Code accepts by its alias, for the model menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelChoice {
    pub alias: &'static str,
    pub label: &'static str,
}

pub const MODEL_CHOICES: [ModelChoice; 3] = [
    ModelChoice {
        alias: "opus",
        label: "Opus",
    },
    ModelChoice {
        alias: "sonnet",
        label: "Sonnet",
    },
    ModelChoice {
        alias: "haiku",
        label: "Haiku",
    },
];

/// The modes the mode menu offers; the others show only when an agent already runs in them.
pub const MODE_CHOICES: [PermissionMode; 4] = [
    PermissionMode::Default,
    PermissionMode::AcceptEdits,
    PermissionMode::Plan,
    PermissionMode::Auto,
];

/// The words of a model id that are not its family or version.
const MODEL_VENDOR: &str = "claude";

/// A model id part this long is a release date, not a version number.
const DATE_PART_LENGTH: usize = 8;

/// chrono's format for a month's short name, as in "8 Nov".
const SHORT_MONTH: &str = "%b";

pub fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::NeedsYou => "Needs you",
        AgentStatus::Working => "Working",
        AgentStatus::Idle => "Idle",
        AgentStatus::Exited => "Exited",
    }
}

/// The class that gives a status its colors in the stylesheet.
pub fn status_class(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::NeedsYou => "status-needs-you",
        AgentStatus::Working => "status-working",
        AgentStatus::Idle => "status-idle",
        AgentStatus::Exited => "status-exited",
    }
}

pub fn mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "Ask first",
        PermissionMode::AcceptEdits => "Accept edits",
        PermissionMode::Plan => "Plan",
        PermissionMode::Auto => "Auto",
        PermissionMode::DontAsk => "Don't ask",
        PermissionMode::BypassPermissions => "Bypass permissions",
    }
}

pub fn mode_description(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "Asks before edits and commands",
        PermissionMode::AcceptEdits => "Edits files without asking, asks before commands",
        PermissionMode::Plan => "Reads and plans, changes nothing",
        PermissionMode::Auto => "A classifier approves most actions, asks for the rest",
        PermissionMode::DontAsk => "Refuses what the settings do not allow, without asking",
        PermissionMode::BypassPermissions => "Runs everything without asking",
    }
}

pub fn effort_label(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "Low",
        Effort::Medium => "Medium",
        Effort::High => "High",
        Effort::Xhigh => "Extra high",
        Effort::Max => "Max",
    }
}

/// For the effort buttons of the model menu, five in a row.
pub fn effort_short_label(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => "Low",
        Effort::Medium => "Med",
        Effort::High => "High",
        Effort::Xhigh => "X-high",
        Effort::Max => "Max",
    }
}

/// "claude-opus-5-5" reads "Opus 5.5", "claude-sonnet-4-5-20250929" reads "Sonnet 4.5". An id
/// in another shape stays as it is.
pub fn model_label(model: &str) -> String {
    let id = model.split('[').next().unwrap_or(model);
    let mut family: Vec<String> = Vec::new();
    let mut version: Vec<&str> = Vec::new();
    for part in id.split('-') {
        if part.is_empty() || part.eq_ignore_ascii_case(MODEL_VENDOR) {
            continue;
        }
        if part.chars().all(|character| character.is_ascii_digit()) {
            if part.len() < DATE_PART_LENGTH {
                version.push(part);
            }
            continue;
        }
        if !part
            .chars()
            .all(|character| character.is_ascii_alphabetic())
        {
            return model.to_owned();
        }
        family.push(capitalized(part));
    }
    if family.is_empty() {
        return model.to_owned();
    }
    let mut label = family.join(" ");
    if !version.is_empty() {
        label.push(' ');
        label.push_str(&version.join("."));
    }
    label
}

fn capitalized(word: &str) -> String {
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

pub fn window_label(window: UsageWindow) -> &'static str {
    match window {
        UsageWindow::FiveHour => "5-hour window",
        UsageWindow::SevenDay => "7-day window",
        UsageWindow::SevenDayOpus => "7-day window, Opus",
        UsageWindow::SevenDaySonnet => "7-day window, Sonnet",
        UsageWindow::Overage => "Extra usage",
    }
}

pub fn scope_word(scope: McpScope) -> &'static str {
    match scope {
        McpScope::User => "user",
        McpScope::Project => "project",
        McpScope::Local => "local",
        McpScope::Dynamic => "dynamic",
        McpScope::Enterprise => "enterprise",
        McpScope::Managed => "managed",
        McpScope::ClaudeAi => "claude.ai",
    }
}

/// Whether `claude mcp remove` can remove a server of this scope.
pub fn removable_scope(scope: McpScope) -> bool {
    matches!(scope, McpScope::User | McpScope::Project | McpScope::Local)
}

pub fn mcp_status_label(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Connected => "Connected",
        McpServerStatus::Failed => "Failed",
        McpServerStatus::NeedsAuth => "Needs sign-in",
        McpServerStatus::Pending => "Connecting",
        McpServerStatus::Disabled => "Disabled",
    }
}

pub fn mcp_status_class(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Connected => "mcp-connected",
        McpServerStatus::Failed => "mcp-failed",
        McpServerStatus::NeedsAuth => "mcp-needs-auth",
        McpServerStatus::Pending => "mcp-pending",
        McpServerStatus::Disabled => "mcp-disabled",
    }
}

/// "3 files", "1 file".
pub fn count(number: usize, singular: &str, plural: &str) -> String {
    let noun = if number == 1 { singular } else { plural };
    format!("{number} {noun}")
}

/// "12 s", "1 min 42 s", "3 h 12 min".
pub fn duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes) = (seconds / 3600, seconds % 3600 / 60);
    match (hours, minutes, seconds % 60) {
        (0, 0, seconds) => format!("{seconds} s"),
        (0, minutes, 0) => format!("{minutes} min"),
        (0, minutes, seconds) => format!("{minutes} min {seconds} s"),
        (hours, 0, _) => format!("{hours} h"),
        (hours, minutes, _) => format!("{hours} h {minutes} min"),
    }
}

/// "950", "12k", "88.3k", "1.2M".
pub fn tokens(count: u64) -> String {
    const THOUSAND: u64 = 1_000;
    const MILLION: u64 = 1_000_000;
    let scaled = |value: u64, unit: u64, suffix: &str| -> String {
        let tenths = value / (unit / 10);
        if tenths.is_multiple_of(10) || tenths >= 1000 {
            format!("{}{suffix}", tenths / 10)
        } else {
            format!("{}.{}{suffix}", tenths / 10, tenths % 10)
        }
    };
    if count >= MILLION {
        scaled(count, MILLION, "M")
    } else if count >= THOUSAND {
        scaled(count, THOUSAND, "k")
    } else {
        count.to_string()
    }
}

/// "$3.40".
pub fn cost(cost: CostUsd) -> String {
    format!("${:.2}", cost.get())
}

/// A rate limit's utilization, from 0 to 1, as a whole percentage.
pub fn percent(utilization: f64) -> u8 {
    let clamped = (utilization * 100.0).round().clamp(0.0, 100.0);
    // In 0..=100 after the clamp, so the cast is exact.
    clamped as u8
}

/// Whole minutes until `moment`, rounded up; `None` once it has passed.
pub fn minutes_until(moment: Timestamp, now: SystemTime) -> Option<u64> {
    let remaining = moment.to_system_time()?.duration_since(now).ok()?;
    if remaining.is_zero() {
        return None;
    }
    Some(remaining.as_secs().div_ceil(60))
}

/// `None` past chrono's calendar, where its conversions from `SystemTime` and to local time panic.
fn local(moment: Timestamp) -> Option<NaiveDateTime> {
    let millis = i64::try_from(moment.unix_millis()).ok()?;
    let time = DateTime::from_timestamp_millis(millis)?.with_timezone(&Local);
    time.naive_utc().checked_add_offset(*time.offset())
}

fn local_now(now: SystemTime) -> NaiveDateTime {
    DateTime::<Local>::from(now).naive_local()
}

/// When a plan window resets: "at 16:00, in 3 h 12 min" today, "Monday at 09:00" within a week.
pub fn reset_time(moment: Timestamp, now: SystemTime) -> Option<String> {
    let when = clock_time(local(moment)?, local_now(now));
    let remaining = moment.to_system_time()?.duration_since(now).ok();
    Some(match remaining {
        Some(remaining) if remaining < Duration::from_secs(24 * 3600) => {
            let rounded = Duration::from_secs(remaining.as_secs() / 60 * 60);
            format!("Resets {when}, in {}", duration(rounded))
        }
        Some(_) | None => format!("Resets {when}"),
    })
}

/// "at 16:00" the same day, "Monday at 09:00" within a week, "8 Nov at 09:00" later.
fn clock_time(moment: NaiveDateTime, now: NaiveDateTime) -> String {
    use chrono::Datelike;

    let time = moment.format("%H:%M");
    let days = (moment.date() - now.date()).num_days();
    match days {
        0 => format!("at {time}"),
        1..=6 => format!("{} at {time}", moment.format("%A")),
        _ => format!("{} {} at {time}", moment.day(), moment.format(SHORT_MONTH)),
    }
}

/// "now", "5 min ago", "2 h ago", "yesterday", "3 days ago", "8 Nov".
pub fn ago(moment: Timestamp, now: SystemTime) -> String {
    let (Some(then), Some(elapsed)) = (
        local(moment),
        moment
            .to_system_time()
            .and_then(|time| now.duration_since(time).ok()),
    ) else {
        return "now".to_owned();
    };
    ago_between(then, local_now(now), elapsed)
}

fn ago_between(then: NaiveDateTime, now: NaiveDateTime, elapsed: Duration) -> String {
    use chrono::Datelike;

    let minutes = elapsed.as_secs() / 60;
    let days = (now.date() - then.date()).num_days();
    match (minutes, days) {
        (0, _) => "now".to_owned(),
        (1..=59, _) => format!("{minutes} min ago"),
        (_, 0) => format!("{} h ago", minutes / 60),
        (_, 1) => "yesterday".to_owned(),
        (_, 2..=6) => format!("{days} days ago"),
        _ => format!("{} {}", then.day(), then.format(SHORT_MONTH)),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, Utc};

    use super::*;

    fn at(day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, day)
            .and_then(|date| date.and_hms_opt(hour, minute, 0))
            .unwrap()
    }

    #[test]
    fn model_ids_read_as_family_and_version() {
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_label("claude-3-5-haiku-20241022"), "Haiku 3.5");
        assert_eq!(model_label("opus"), "Opus");
        assert_eq!(model_label("claude-opus-4-6[1m]"), "Opus 4.6");
        assert_eq!(model_label("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn durations_read_in_the_largest_units() {
        assert_eq!(duration(Duration::from_secs(12)), "12 s");
        assert_eq!(duration(Duration::from_secs(102)), "1 min 42 s");
        assert_eq!(duration(Duration::from_secs(240)), "4 min");
        assert_eq!(
            duration(Duration::from_secs(3 * 3600 + 12 * 60 + 5)),
            "3 h 12 min"
        );
        assert_eq!(duration(Duration::from_secs(7200)), "2 h");
    }

    #[test]
    fn token_counts_shorten() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(12_000), "12k");
        assert_eq!(tokens(88_340), "88.3k");
        assert_eq!(tokens(200_000), "200k");
        assert_eq!(tokens(1_250_000), "1.2M");
    }

    #[test]
    fn costs_and_percentages() {
        assert_eq!(cost(CostUsd::new(3.4)), "$3.40");
        assert_eq!(percent(0.424), 42);
        assert_eq!(percent(1.7), 100);
        assert_eq!(percent(-0.1), 0);
    }

    #[test]
    fn minutes_until_rounds_up_and_ends() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let soon = Timestamp::from_unix_millis(1_000_000 + 61_000);
        let past = Timestamp::from_unix_millis(999_000);

        assert_eq!(minutes_until(soon, now), Some(2));
        assert_eq!(minutes_until(past, now), None);
    }

    #[test]
    fn clock_times_name_the_day_when_it_is_not_today() {
        let now = at(5, 13, 0);

        assert_eq!(clock_time(at(5, 16, 0), now), "at 16:00");
        assert_eq!(clock_time(at(9, 9, 0), now), "Monday at 09:00");
        assert_eq!(clock_time(at(20, 9, 30), now), "20 Nov at 09:30");
    }

    #[test]
    fn ago_reads_in_minutes_hours_then_days() {
        let now = at(5, 13, 0);
        let minutes = |count: u64| Duration::from_secs(count * 60);

        assert_eq!(ago_between(at(5, 12, 55), now, minutes(5)), "5 min ago");
        assert_eq!(ago_between(at(5, 10, 0), now, minutes(180)), "3 h ago");
        assert_eq!(ago_between(at(4, 22, 0), now, minutes(900)), "yesterday");
        assert_eq!(ago_between(at(1, 9, 0), now, minutes(6000)), "4 days ago");
        assert_eq!(
            ago_between(at(1, 9, 0), at(20, 9, 0), minutes(27_360)),
            "1 Nov"
        );
    }

    #[test]
    fn a_moment_past_the_calendar_reads_without_failing() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let far = Timestamp::from_unix_millis(u64::MAX);
        let last = u64::try_from(DateTime::<Utc>::MAX_UTC.timestamp_millis()).unwrap();

        assert_eq!(reset_time(far, now), None);
        assert_eq!(ago(far, now), "now");
        // East of UTC, the calendar's last moment is past it in local time.
        assert!(
            reset_time(Timestamp::from_unix_millis(last), now)
                .is_none_or(|text| text.starts_with("Resets"))
        );
    }

    #[test]
    fn the_largest_token_count_reads_without_overflow() {
        assert_eq!(tokens(u64::MAX), "18446744073709M");
    }

    #[test]
    fn counts_agree_with_their_noun() {
        assert_eq!(count(1, "file", "files"), "1 file");
        assert_eq!(count(3, "file", "files"), "3 files");
    }
}

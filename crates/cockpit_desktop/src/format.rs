//! How values read on screen: durations, token counts, costs, moments, and the words for
//! statuses, modes, models and scopes.

use std::time::{Duration, SystemTime};

use cockpit_protocol::{
    AgentStatus, AgentSummary, ClaudeRoute, Cores, CostUsd, Effort, McpScope, McpServerStatus,
    PermissionMode, SubagentStatus, Timestamp, UsageWindow,
};
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;

/// A model Claude Code accepts by its alias, for the model menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelChoice {
    pub alias: &'static str,
    pub label: &'static str,
}

pub const OPUS: ModelChoice = ModelChoice {
    alias: "opus",
    label: "Opus",
};

pub const MODEL_CHOICES: [ModelChoice; 4] = [
    ModelChoice {
        alias: "fable",
        label: "Fable",
    },
    OPUS,
    ModelChoice {
        alias: "sonnet",
        label: "Sonnet",
    },
    ModelChoice {
        alias: "haiku",
        label: "Haiku",
    },
];

/// A plan window this full, in percent, is close to its limit.
pub const CLOSE_TO_LIMIT_PERCENT: u8 = 80;

/// The modes the mode menu offers; the others show only when an agent already runs in them.
pub const MODE_CHOICES: [PermissionMode; 4] = [
    PermissionMode::Default,
    PermissionMode::AcceptEdits,
    PermissionMode::Plan,
    PermissionMode::Auto,
];

/// The words of a model id that are not its family or version.
const MODEL_VENDOR: &str = "claude";

/// What starts a model id's suffix, as in "claude-opus-4-6[1m]".
const MODEL_SUFFIX: char = '[';

/// A model id part this long is a release date, not a version number.
const DATE_PART_LENGTH: usize = 8;

/// jiff's `strftime` format for a month's short name, as in "8 Nov".
const SHORT_MONTH: &str = "%b";

/// jiff's `strftime` format for a weekday's name, as in "Monday".
const WEEKDAY: &str = "%A";

/// jiff's `strftime` format for a time to the minute, as in "09:05".
const TIME_TO_MINUTE: &str = "%H:%M";

/// jiff's `strftime` format for a time to the second, as in "09:05:42".
const TIME_TO_SECOND: &str = "%H:%M:%S";

/// The class of an idle agent whose background tasks still run.
const BACKGROUND_WORK_CLASS: &str = "background-work";

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

/// The background tasks that still run for an idle agent, which its status alone does not show.
fn background_work(summary: &AgentSummary) -> Option<u16> {
    (summary.status == AgentStatus::Idle && summary.background_running > 0)
        .then_some(summary.background_running)
}

/// An agent's status as the sidebar and the header say it.
pub fn agent_status_label(summary: &AgentSummary) -> String {
    match background_work(summary) {
        Some(count) => format!("Idle, {count} running in the background"),
        None => status_label(summary.status).to_owned(),
    }
}

/// An agent's status classes. An idle agent with background work shows the working color's ring.
pub fn agent_status_class(summary: &AgentSummary) -> String {
    let class = status_class(summary.status);
    match background_work(summary) {
        Some(_) => format!("{class} {BACKGROUND_WORK_CLASS}"),
        None => class.to_owned(),
    }
}

pub fn subagent_status_label(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Running => "Running",
        SubagentStatus::NeedsYou => "Waits for you",
        SubagentStatus::Done => "Done",
        SubagentStatus::Failed => "Failed",
        SubagentStatus::Stopped => "Stopped",
    }
}

/// The class that gives a subagent's status its colors: an agent's, or green and red for the
/// way it ended.
pub fn subagent_status_class(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Running => status_class(AgentStatus::Working),
        SubagentStatus::NeedsYou => status_class(AgentStatus::NeedsYou),
        SubagentStatus::Done => "status-done",
        SubagentStatus::Failed => "status-failed",
        SubagentStatus::Stopped => status_class(AgentStatus::Exited),
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
    let id = model.split(MODEL_SUFFIX).next().unwrap_or(model);
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

/// The model menu's alias for the family of the id Claude Code reports: "claude-opus-5-5" is
/// "opus". `None` for a family the menu does not offer.
pub fn model_alias(model: &str) -> Option<&'static str> {
    let id = model.split(MODEL_SUFFIX).next().unwrap_or(model);
    MODEL_CHOICES
        .iter()
        .map(|choice| choice.alias)
        .find(|alias| id.split('-').any(|part| part.eq_ignore_ascii_case(alias)))
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

/// The route's name where room is short, as on the header's chip.
pub fn route_short_label(route: ClaudeRoute) -> &'static str {
    match route {
        ClaudeRoute::Anthropic => "Anthropic",
        ClaudeRoute::Bedrock => "Bedrock",
        ClaudeRoute::Vertex => "Vertex AI",
        ClaudeRoute::Foundry => "Foundry",
        ClaudeRoute::CustomServer => "Custom server",
    }
}

/// The service an account's agents send their requests to.
pub fn route_label(route: ClaudeRoute) -> &'static str {
    match route {
        ClaudeRoute::Anthropic => "Anthropic",
        ClaudeRoute::Bedrock => "Amazon Bedrock",
        ClaudeRoute::Vertex => "Google Vertex AI",
        ClaudeRoute::Foundry => "Microsoft Foundry",
        ClaudeRoute::CustomServer => "A custom server",
    }
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

/// The daemon's message as a sentence.
pub fn sentence(message: &str) -> String {
    format!("{}.", capitalized(message))
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

/// The largest whole unit only, for a narrow place: "12 s", "4 min", "2 h", "3 d".
pub fn elapsed(duration: Duration) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match duration.as_secs() {
        seconds @ 0..MINUTE => format!("{seconds} s"),
        seconds @ MINUTE..HOUR => format!("{} min", seconds / MINUTE),
        seconds @ HOUR..DAY => format!("{} h", seconds / HOUR),
        seconds => format!("{} d", seconds / DAY),
    }
}

/// The time from `moment` to `now`, or zero when `moment` is later, as with a daemon whose clock
/// runs ahead of this machine's.
pub fn since(moment: Timestamp, now: SystemTime) -> Duration {
    moment
        .to_system_time()
        .and_then(|time| now.duration_since(time).ok())
        .unwrap_or_default()
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

/// A file's size, to the nearest tenth: "512 B", "4.2 KB", "13 MB", "2.1 GB", in units of 1,024
/// bytes.
pub fn bytes(size: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if size < 1024 {
        return format!("{size} B");
    }
    let tenths_of = |unit_bytes: u128| (u128::from(size) * 10 + unit_bytes / 2) / unit_bytes;
    let mut unit_bytes: u128 = 1024;
    let mut unit = 0;
    // The unit is picked on the rounded size, so that 1023.96 KB reads "1.0 MB".
    while tenths_of(unit_bytes) >= 10_240 && unit + 1 < UNITS.len() {
        unit_bytes *= 1024;
        unit += 1;
    }
    let tenths = tenths_of(unit_bytes);
    let name = UNITS.get(unit).copied().unwrap_or_default();
    if tenths >= 100 {
        format!("{} {name}", tenths / 10)
    } else {
        format!("{}.{} {name}", tenths / 10, tenths % 10)
    }
}

/// "$3.40".
pub fn cost(cost: CostUsd) -> String {
    format!("${:.2}", cost.get())
}

/// Cores in use, with one decimal: "18.4".
pub fn cores(cores: Cores) -> String {
    format!("{:.1}", cores.get())
}

/// A gigabyte of 2^30 bytes, as `free` and the kernel count memory.
const GIGABYTE: u64 = 1 << 30;

/// "412 MB" under a gigabyte, "22.1 GB", and without the decimal from 100 GB: "312 GB".
pub fn memory(bytes: u64) -> String {
    const MEGABYTE: u64 = 1 << 20;
    let megabytes = bytes.saturating_add(MEGABYTE / 2) / MEGABYTE;
    if megabytes < 1024 {
        return format!("{megabytes} MB");
    }
    format!("{} GB", gigabytes(bytes, bytes))
}

/// "312 / 503 GB": the total decides whether both show a decimal.
pub fn memory_of(used: u64, total: u64) -> String {
    format!(
        "{} / {} GB",
        gigabytes(used, total),
        gigabytes(total, total)
    )
}

fn gigabytes(bytes: u64, scale: u64) -> String {
    const NO_DECIMAL_FROM: u64 = 100 * GIGABYTE;
    let half = GIGABYTE / 2;
    if scale >= NO_DECIMAL_FROM {
        return (bytes.saturating_add(half) / GIGABYTE).to_string();
    }
    let tenths = bytes.saturating_mul(10).saturating_add(half) / GIGABYTE;
    format!("{}.{}", tenths / 10, tenths % 10)
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

/// `None` past jiff's calendar, which ends in the year 9999.
fn local(moment: Timestamp) -> Option<DateTime> {
    let millis = i64::try_from(moment.unix_millis()).ok()?;
    let moment = jiff::Timestamp::from_millisecond(millis).ok()?;
    Some(TimeZone::system().to_datetime(moment))
}

/// A clock past either end of jiff's calendar reads as that end.
fn local_now(now: SystemTime) -> DateTime {
    let now = jiff::Timestamp::try_from(now).unwrap_or(if now < SystemTime::UNIX_EPOCH {
        jiff::Timestamp::MIN
    } else {
        jiff::Timestamp::MAX
    });
    TimeZone::system().to_datetime(now)
}

/// The local time of day, to the second: "14:07:11".
pub fn time_of_day(moment: Timestamp) -> Option<String> {
    Some(local(moment)?.strftime(TIME_TO_SECOND).to_string())
}

/// The day `now` falls on, in this computer's time zone.
pub fn local_day(now: SystemTime) -> Date {
    local_now(now).date()
}

/// The local time of day at `now`, to the second: "14:07:11".
pub fn local_clock(now: SystemTime) -> String {
    local_now(now).strftime(TIME_TO_SECOND).to_string()
}

/// When a message was sent, under it in the chat: "13:38" today, "Yesterday 17:05", "8 Nov 09:12"
/// this year, "8 Nov 2025 09:12" before.
pub fn message_time(moment: Timestamp, today: Date) -> Option<String> {
    Some(message_time_on(local(moment)?, today))
}

fn message_time_on(moment: DateTime, today: Date) -> String {
    let time = moment.strftime(TIME_TO_MINUTE);
    match (today - moment.date()).get_days() {
        0 => time.to_string(),
        1 => format!("Yesterday {time}"),
        _ if moment.year() == today.year() => {
            format!("{} {} {time}", moment.day(), moment.strftime(SHORT_MONTH))
        }
        _ => format!(
            "{} {} {} {time}",
            moment.day(),
            moment.strftime(SHORT_MONTH),
            moment.year()
        ),
    }
}

/// The whole moment, for a tooltip: "Wednesday 12 Nov 2026, 13:38:05".
pub fn full_time(moment: Timestamp) -> Option<String> {
    let moment = local(moment)?;
    Some(format!(
        "{} {} {} {}, {}",
        moment.strftime(WEEKDAY),
        moment.day(),
        moment.strftime(SHORT_MONTH),
        moment.year(),
        moment.strftime(TIME_TO_SECOND)
    ))
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
fn clock_time(moment: DateTime, now: DateTime) -> String {
    let time = moment.strftime(TIME_TO_MINUTE);
    let days = (moment.date() - now.date()).get_days();
    match days {
        0 => format!("at {time}"),
        1..=6 => format!("{} at {time}", moment.strftime(WEEKDAY)),
        _ => format!(
            "{} {} at {time}",
            moment.day(),
            moment.strftime(SHORT_MONTH)
        ),
    }
}

/// What `ago` says for less than a minute.
pub const NOW: &str = "now";

/// "now", "5 min ago", "2 h ago", "yesterday", "3 days ago", "8 Nov".
pub fn ago(moment: Timestamp, now: SystemTime) -> String {
    match past(moment, now) {
        Past::Now => NOW.to_owned(),
        Past::Ago(words) | Past::Date(words) => words,
    }
}

/// `ago` within a sentence, after a verb: "just now", "5 min ago", "yesterday", "on 8 Nov".
pub fn ago_in_sentence(moment: Timestamp, now: SystemTime) -> String {
    match past(moment, now) {
        Past::Now => "just now".to_owned(),
        Past::Ago(words) => words,
        Past::Date(date) => format!("on {date}"),
    }
}

/// How long ago a moment was, in words.
#[derive(Debug, PartialEq, Eq)]
enum Past {
    /// Less than a minute ago, or a moment the clocks cannot place.
    Now,
    /// "5 min ago", "yesterday", "3 days ago".
    Ago(String),
    /// "8 Nov", a week ago or more.
    Date(String),
}

fn past(moment: Timestamp, now: SystemTime) -> Past {
    let (Some(then), Some(elapsed)) = (
        local(moment),
        moment
            .to_system_time()
            .and_then(|time| now.duration_since(time).ok()),
    ) else {
        return Past::Now;
    };
    past_between(then, local_now(now), elapsed)
}

fn past_between(then: DateTime, now: DateTime, elapsed: Duration) -> Past {
    let minutes = elapsed.as_secs() / 60;
    let days = (now.date() - then.date()).get_days();
    match (minutes, days) {
        (0, _) => Past::Now,
        (1..=59, _) => Past::Ago(format!("{minutes} min ago")),
        (_, 0) => Past::Ago(format!("{} h ago", minutes / 60)),
        (_, 1) => Past::Ago("yesterday".to_owned()),
        (_, 2..=6) => Past::Ago(format!("{days} days ago")),
        _ => Past::Date(format!("{} {}", then.day(), then.strftime(SHORT_MONTH))),
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil;

    use super::*;

    #[test]
    fn cores_and_memory_read_as_the_mockups_show_them() {
        const GIB: u64 = 1 << 30;

        assert_eq!(cores(Cores::new(18.44)), "18.4");
        assert_eq!(cores(Cores::new(0.0)), "0.0");
        assert_eq!(memory(22 * GIB + GIB / 10), "22.1 GB");
        assert_eq!(memory(GIB * 4 / 10), "410 MB");
        assert_eq!(memory(3 << 20), "3 MB");
        assert_eq!(memory(312 * GIB), "312 GB");
        assert_eq!(memory_of(312 * GIB, 503 * GIB), "312 / 503 GB");
        assert_eq!(memory_of(6 * GIB, 16 * GIB), "6.0 / 16.0 GB");
    }

    fn at(day: i8, hour: i8, minute: i8) -> DateTime {
        civil::date(2026, 11, day).at(hour, minute, 0, 0)
    }

    #[test]
    fn sizes_read_in_the_largest_whole_unit() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(4_300), "4.2 KB");
        assert_eq!(bytes(13 * 1024 * 1024 + 1), "13 MB");
        assert_eq!(bytes(2_254_857_830), "2.1 GB");
    }

    #[test]
    fn a_size_just_under_a_unit_reads_in_that_unit() {
        const MIB: u64 = 1 << 20;

        assert_eq!(bytes(1_048_535), "1.0 MB");
        assert_eq!(bytes(1_047_000), "1022 KB");
        assert_eq!(memory(1023 * MIB + MIB * 6 / 10), "1.0 GB");
        assert_eq!(memory(1023 * MIB), "1023 MB");
    }

    #[test]
    fn a_sparse_file_of_two_exbibytes_shows_its_size() {
        assert_eq!(bytes(1 << 61), "2097152 TB");
        assert_eq!(bytes(u64::MAX), "16777216 TB");
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
    fn model_ids_map_to_the_menu_alias_of_their_family() {
        assert_eq!(model_alias("claude-opus-5-5"), Some("opus"));
        assert_eq!(model_alias("claude-sonnet-4-5-20250929"), Some("sonnet"));
        assert_eq!(model_alias("claude-3-5-haiku-20241022"), Some("haiku"));
        assert_eq!(model_alias("claude-opus-4-6[1m]"), Some("opus"));
        assert_eq!(model_alias("sonnet"), Some("sonnet"));
        assert_eq!(model_alias("claude-mythical-1"), None);
        assert_eq!(model_alias("gpt-4o"), None);
    }

    #[test]
    fn fable_heads_the_model_menu_and_its_ids_map_to_it() {
        let labels: Vec<&str> = MODEL_CHOICES.iter().map(|choice| choice.label).collect();

        assert_eq!(labels, ["Fable", "Opus", "Sonnet", "Haiku"]);
        assert_eq!(model_alias("claude-fable-5-2"), Some("fable"));
        assert_eq!(model_label("claude-fable-5-2"), "Fable 5.2");
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
    fn elapsed_times_read_in_one_whole_unit() {
        assert_eq!(elapsed(Duration::ZERO), "0 s");
        assert_eq!(elapsed(Duration::from_millis(12_900)), "12 s");
        assert_eq!(elapsed(Duration::from_secs(59)), "59 s");
        assert_eq!(elapsed(Duration::from_secs(60)), "1 min");
        assert_eq!(elapsed(Duration::from_secs(4 * 60 + 59)), "4 min");
        assert_eq!(elapsed(Duration::from_secs(2 * 3600 + 59 * 60)), "2 h");
        assert_eq!(elapsed(Duration::from_secs(24 * 3600)), "1 d");
        assert_eq!(
            elapsed(Duration::from_secs(3 * 24 * 3600 + 5 * 3600)),
            "3 d"
        );
    }

    #[test]
    fn the_time_since_a_later_moment_is_zero() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        assert_eq!(
            since(Timestamp::from_unix_millis(988_000), now),
            Duration::from_secs(12)
        );
        assert_eq!(
            since(Timestamp::from_unix_millis(1_005_000), now),
            Duration::ZERO
        );
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
    fn a_message_time_names_the_day_only_when_it_is_not_today() {
        let today = at(12, 13, 40).date();
        let last_year = civil::date(2025, 11, 8).at(9, 12, 0, 0);

        assert_eq!(message_time_on(at(12, 13, 38), today), "13:38");
        assert_eq!(message_time_on(at(11, 17, 5), today), "Yesterday 17:05");
        assert_eq!(message_time_on(at(8, 9, 12), today), "8 Nov 09:12");
        assert_eq!(message_time_on(last_year, today), "8 Nov 2025 09:12");
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

        let ago = |words: &str| Past::Ago(words.to_owned());

        assert_eq!(past_between(at(5, 12, 59), now, minutes(0)), Past::Now);
        assert_eq!(
            past_between(at(5, 12, 55), now, minutes(5)),
            ago("5 min ago")
        );
        assert_eq!(
            past_between(at(5, 10, 0), now, minutes(180)),
            ago("3 h ago")
        );
        assert_eq!(
            past_between(at(4, 22, 0), now, minutes(900)),
            ago("yesterday")
        );
        assert_eq!(
            past_between(at(1, 9, 0), now, minutes(6000)),
            ago("4 days ago")
        );
        assert_eq!(
            past_between(at(1, 9, 0), at(20, 9, 0), minutes(27_360)),
            Past::Date("1 Nov".to_owned())
        );
    }

    #[test]
    fn ago_in_a_sentence_puts_on_before_a_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let before =
            |seconds: u64| Timestamp::from_system_time(now - Duration::from_secs(seconds)).unwrap();

        assert_eq!(ago_in_sentence(before(10), now), "just now");
        assert_eq!(ago_in_sentence(before(300), now), "5 min ago");
        assert!(ago_in_sentence(before(30 * 86_400), now).starts_with("on "));
    }

    #[test]
    fn a_moment_past_the_calendar_reads_without_failing() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let far = Timestamp::from_unix_millis(u64::MAX);
        let last_second = u64::try_from(jiff::Timestamp::MAX.as_second()).unwrap() * 1000;

        assert_eq!(reset_time(far, now), None);
        assert_eq!(ago(far, now), "now");
        assert!(
            reset_time(Timestamp::from_unix_millis(last_second), now)
                .is_some_and(|text| text.starts_with("Resets"))
        );
    }

    /// jiff's calendar ends in the year 9999, so a later moment has no local time.
    #[test]
    fn a_moment_after_the_year_9999_reads_as_unknown() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let last = u64::try_from(jiff::Timestamp::MAX.as_millisecond()).unwrap();
        let after = Timestamp::from_unix_millis(last + 1);

        assert_eq!(time_of_day(after), None);
        assert_eq!(full_time(after), None);
        assert_eq!(message_time(after, local_day(now)), None);
        assert_eq!(reset_time(after, now), None);
        assert_eq!(ago(after, now), "now");
    }

    #[test]
    fn a_clock_after_the_year_9999_reads_as_its_last_day() {
        let far = SystemTime::UNIX_EPOCH + Duration::from_secs(400_000_000_000);

        assert_eq!(local_day(far).year(), 9999);
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

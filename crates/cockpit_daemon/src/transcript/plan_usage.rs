//! The account's plan usage, from `rate_limit_event` lines of any agent.

use std::time::SystemTime;

use claude_protocol::RateLimitInfo;
use cockpit_protocol::{PlanUsage, RateLimitStatus, Timestamp, UsageWindow, WindowUsage};

use crate::transcript::words;

/// Applies an event to the usage. Returns whether the usage changed.
///
/// An event names one window, and may carry the state of others in `unified_windows`. A value
/// the event leaves out keeps the last one reported.
pub(crate) fn apply(usage: &mut PlanUsage, info: &RateLimitInfo) -> bool {
    let before = usage.clone();
    let event_status = words::rate_limit_status(&info.status);
    for (window_type, window) in &info.unified_windows {
        let Some(window_key) = words::usage_window(window_type) else {
            continue;
        };
        let status = window
            .status
            .as_ref()
            .and_then(words::rate_limit_status)
            .or(event_status);
        update(
            usage,
            window_key,
            window.utilization,
            window.resets_at,
            status,
        );
    }
    if let Some(window_key) = info.rate_limit_type.as_ref().and_then(words::usage_window) {
        update(
            usage,
            window_key,
            info.utilization,
            info.resets_at,
            event_status,
        );
    }
    *usage != before
}

fn update(
    usage: &mut PlanUsage,
    window: UsageWindow,
    utilization: Option<f64>,
    resets_at: Option<SystemTime>,
    status: Option<RateLimitStatus>,
) {
    let entry = usage.windows.entry(window).or_insert(WindowUsage {
        utilization: None,
        resets_at: None,
        status: RateLimitStatus::Allowed,
    });
    if utilization.is_some() {
        entry.utilization = utilization;
    }
    if let Some(resets_at) = resets_at.and_then(Timestamp::from_system_time) {
        entry.resets_at = Some(resets_at);
    }
    if let Some(status) = status {
        entry.status = status;
    }
}

#[cfg(test)]
mod tests {
    use claude_protocol::OutputLine;

    use super::*;

    fn info(line: &str) -> RateLimitInfo {
        let OutputLine::RateLimitEvent(event) = line.parse().unwrap() else {
            panic!("not a rate limit event");
        };
        event.rate_limit_info
    }

    #[test]
    fn the_utilization_comes_from_the_unified_windows_and_stays_when_left_out() {
        let mut usage = PlanUsage::default();
        let first = info(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"seven_day","resetsAt":1800000000,"unifiedWindows":{"seven_day":{"utilization":0.25,"resetsAt":1800000000,"status":"allowed"}}}}"#,
        );
        let second = info(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","rateLimitType":"seven_day"}}"#,
        );

        assert!(apply(&mut usage, &first));
        assert!(!apply(&mut usage, &first));
        assert!(apply(&mut usage, &second));

        let window = usage.windows[&UsageWindow::SevenDay];
        assert_eq!(window.utilization, Some(0.25));
        assert_eq!(window.status, RateLimitStatus::AllowedWarning);
        assert_eq!(
            window.resets_at,
            Some(Timestamp::from_unix_millis(1_800_000_000_000))
        );
    }

    #[test]
    fn an_unknown_window_is_ignored() {
        let mut usage = PlanUsage::default();
        let event = info(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"one_minute","utilization":0.5}}"#,
        );

        assert!(!apply(&mut usage, &event));
        assert!(usage.windows.is_empty());
    }
}

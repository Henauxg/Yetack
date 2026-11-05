use std::collections::BTreeMap;
use std::time::SystemTime;

use serde::Deserialize;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::ids::{MessageUuid, SessionId};
use crate::open_word::OpenWord;
use crate::wire;

/// A `rate_limit_event` line: the account's plan usage changed.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RateLimitEvent {
    pub rate_limit_info: RateLimitInfo,
    pub session_id: Option<SessionId>,
    pub uuid: Option<MessageUuid>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    pub status: OpenWord<RateLimitStatus>,
    /// The window this event is about.
    pub rate_limit_type: Option<OpenWord<RateLimitType>>,
    /// From 0 to 1.
    pub utilization: Option<f64>,
    #[serde(default, deserialize_with = "wire::optional_unix_seconds")]
    pub resets_at: Option<SystemTime>,
    pub overage_status: Option<OpenWord<RateLimitStatus>>,
    #[serde(default, deserialize_with = "wire::optional_unix_seconds")]
    pub overage_resets_at: Option<SystemTime>,
    pub overage_disabled_reason: Option<String>,
    /// Each window's state, which may carry the utilization the top level lacks.
    #[serde(default)]
    pub unified_windows: BTreeMap<OpenWord<RateLimitType>, RateLimitWindow>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub status: Option<OpenWord<RateLimitStatus>>,
    /// From 0 to 1.
    pub utilization: Option<f64>,
    #[serde(default, deserialize_with = "wire::optional_unix_seconds")]
    pub resets_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum RateLimitStatus {
    Allowed,
    /// Close to the limit.
    AllowedWarning,
    Rejected,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Display,
    EnumString,
    IntoStaticStr,
    VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum RateLimitType {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    Overage,
}

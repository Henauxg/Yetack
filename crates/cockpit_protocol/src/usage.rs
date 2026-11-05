//! Costs, token counts and the account's plan usage.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::time::Timestamp;

/// An amount in US dollars, as Claude Code estimates it at API prices.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CostUsd(f64);

impl CostUsd {
    #[must_use]
    pub fn new(dollars: f64) -> Self {
        Self(dollars)
    }

    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

/// The latest rate limit state of each plan window. It belongs to the account, not to an agent.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlanUsage {
    pub windows: BTreeMap<UsageWindow, WindowUsage>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum UsageWindow {
    FiveHour,
    SevenDay,
    SevenDayOpus,
    SevenDaySonnet,
    Overage,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowUsage {
    /// From 0 to 1. `None` when Claude Code reported only a status.
    pub utilization: Option<f64>,
    pub resets_at: Option<Timestamp>,
    pub status: RateLimitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitStatus {
    Allowed,
    AllowedWarning,
    Rejected,
}

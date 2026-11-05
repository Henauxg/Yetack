//! Moments and durations on the wire.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A moment on the daemon's clock, in milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(u64);

impl Timestamp {
    pub fn from_unix_millis(millis: u64) -> Self {
        Self(millis)
    }

    pub fn unix_millis(self) -> u64 {
        self.0
    }

    /// `None` before the epoch, or past `u64::MAX` milliseconds after it.
    pub fn from_system_time(time: SystemTime) -> Option<Self> {
        let since_epoch = time.duration_since(UNIX_EPOCH).ok()?;
        u64::try_from(since_epoch.as_millis()).ok().map(Self)
    }

    /// `None` when the platform's `SystemTime` cannot hold the moment.
    pub fn to_system_time(self) -> Option<SystemTime> {
        UNIX_EPOCH.checked_add(Duration::from_millis(self.0))
    }
}

/// Writes a `Duration` as whole milliseconds.
pub(crate) fn serialize_millis<S: Serializer>(
    duration: &Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::Error;

    let millis = u64::try_from(duration.as_millis()).map_err(S::Error::custom)?;
    serializer.serialize_u64(millis)
}

pub(crate) fn deserialize_millis<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Duration, D::Error> {
    u64::deserialize(deserializer).map(Duration::from_millis)
}

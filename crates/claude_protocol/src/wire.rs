//! Helpers to read Claude Code's JSON.

use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Number, Value};

use crate::parse_error::ParseError;

/// The key whose word tells lines, blocks and permission updates apart.
pub(crate) const TYPE: &str = "type";
/// The key whose word tells the kinds of a `system` line or a control message apart.
pub(crate) const SUBTYPE: &str = "subtype";

/// Reads the word under `key`, or `None` when it is missing or not a word of `W`.
pub(crate) fn word<W: FromStr>(value: &Value, key: &str) -> Option<W> {
    value.get(key)?.as_str()?.parse().ok()
}

/// Reads a line of kind `kind`.
pub(crate) fn read<T: DeserializeOwned>(
    value: &Value,
    kind: &'static str,
) -> Result<T, ParseError> {
    T::deserialize(value).map_err(|error| ParseError::MalformedLine {
        kind,
        request_id: None,
        error,
    })
}

/// Reads `null` as the default value, for lists Claude Code may send as `null`.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    let value: Option<T> = Option::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

/// Reads a value that is `null` or does not read as a `T`, even deep inside it, as the default.
pub(crate) fn default_on_error<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// Reads a value that is `null` or not a number of milliseconds as `None`.
pub(crate) fn milliseconds_or_none<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(optional_milliseconds(value).unwrap_or_default())
}

/// Reads a number of milliseconds, whole or not.
pub(crate) fn optional_milliseconds<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;

    let Some(number) = Option::<Number>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let duration = match number.as_u64() {
        Some(milliseconds) => Some(Duration::from_millis(milliseconds)),
        None => number
            .as_f64()
            .and_then(|milliseconds| Duration::try_from_secs_f64(milliseconds / 1000.0).ok()),
    };
    match duration {
        Some(duration) => Ok(Some(duration)),
        None => Err(D::Error::custom(format!(
            "{number} is not a duration in milliseconds"
        ))),
    }
}

/// Reads a Unix time in seconds, whole or not.
pub(crate) fn optional_unix_seconds<'de, D>(deserializer: D) -> Result<Option<SystemTime>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;

    let Some(number) = Option::<Number>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let since_epoch = match number.as_u64() {
        Some(seconds) => Some(Duration::from_secs(seconds)),
        None => number
            .as_f64()
            .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok()),
    };
    match since_epoch.and_then(|since_epoch| SystemTime::UNIX_EPOCH.checked_add(since_epoch)) {
        Some(time) => Ok(Some(time)),
        None => Err(D::Error::custom(format!(
            "{number} is not a Unix time in seconds"
        ))),
    }
}

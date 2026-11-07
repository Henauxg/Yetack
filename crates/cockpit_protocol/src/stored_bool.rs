//! Reads the fields that files of older daemons, `settings.json` and the event logs, hold as a
//! `bool` where the protocol has an enum.

use serde::{Deserialize, Deserializer};

/// An enum that replaced a `bool` field of a saved file.
pub trait FromStoredBool {
    fn from_stored_bool(value: bool) -> Self;
}

/// Reads the enum, or the `bool` a file saved by an older daemon holds in its place.
pub fn enum_or_stored_bool<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + FromStoredBool,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored<T> {
        Enum(T),
        Bool(bool),
    }

    Ok(match Stored::deserialize(deserializer)? {
        Stored::Enum(value) => value,
        Stored::Bool(value) => T::from_stored_bool(value),
    })
}

//! `--key value`, `--key=value` and `--flag` arguments, without a parser crate.

use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};

pub struct Args {
    values: HashMap<String, String>,
    pub positional: Vec<String>,
}

impl Args {
    pub fn from_env() -> Self {
        Self::parse(std::env::args().skip(1))
    }

    pub fn parse(items: impl Iterator<Item = String>) -> Self {
        let mut values = HashMap::new();
        let mut positional = Vec::new();
        let mut items = items.peekable();
        while let Some(item) = items.next() {
            let Some(key) = item.strip_prefix("--") else {
                positional.push(item);
                continue;
            };
            if let Some((key, value)) = key.split_once('=') {
                values.insert(key.to_owned(), value.to_owned());
            } else if items.peek().is_some_and(|next| !next.starts_with("--")) {
                let value = items.next().unwrap_or_default();
                values.insert(key.to_owned(), value);
            } else {
                values.insert(key.to_owned(), String::new());
            }
        }
        Self { values, positional }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub fn has(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn required(&self, key: &str) -> Result<&str> {
        self.get(key).ok_or_else(|| anyhow!("missing --{key}"))
    }

    pub fn or<T: FromStr>(&self, key: &str, default: T) -> Result<T>
    where
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        match self.get(key) {
            Some(value) => value.parse().with_context(|| format!("--{key} {value}")),
            None => Ok(default),
        }
    }

    pub fn optional<T: FromStr>(&self, key: &str) -> Result<Option<T>>
    where
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        self.get(key)
            .map(|value| value.parse().with_context(|| format!("--{key} {value}")))
            .transpose()
    }
}

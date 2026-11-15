//! How two versions compare, as Cargo versions do: this app's releases, and Claude Code's.

use std::cmp::Ordering;

/// "0.10.0" is after "0.9.0", and "1.0.0-beta" before "1.0.0". `None` when either is not such a
/// version.
pub fn compare(left: &str, right: &str) -> Option<Ordering> {
    let (left_numbers, left_pre) = parse(left)?;
    let (right_numbers, right_pre) = parse(right)?;
    let order = left_numbers
        .cmp(&right_numbers)
        .then_with(|| match (left_pre, right_pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => pre_release_order(left, right),
        });
    Some(order)
}

/// The three numbers of a version, and its pre-release. Build metadata, after `+`, counts for
/// nothing.
fn parse(text: &str) -> Option<([u64; 3], Option<&str>)> {
    let text = text.split_once('+').map_or(text, |(version, _)| version);
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (text, None),
    };
    let mut parts = core.split('.');
    let mut numbers = [0; 3];
    for number in &mut numbers {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *number = part.parse().ok()?;
    }
    parts.next().is_none().then_some((numbers, pre))
}

/// Semantic versioning's order of pre-releases: identifier by identifier, numbers by value and
/// before words, and a longer list after its own start.
fn pre_release_order(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        let order = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => match (left.parse::<u64>(), right.parse::<u64>()) {
                (Ok(left), Ok(right)) => left.cmp(&right),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => left.cmp(right),
            },
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_their_numbers_then_their_pre_releases() {
        assert_eq!(compare("0.1.0", "0.2.0"), Some(Ordering::Less));
        assert_eq!(compare("0.10.0", "0.9.0"), Some(Ordering::Greater));
        assert_eq!(compare("1.0.0", "0.99.99"), Some(Ordering::Greater));
        assert_eq!(compare("2.1.320", "2.1.319"), Some(Ordering::Greater));
        assert_eq!(compare("2.10.0", "2.9.9"), Some(Ordering::Greater));
        assert_eq!(compare("0.4.2", "0.4.2"), Some(Ordering::Equal));
        assert_eq!(compare("1.0.0+nix", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare("1.0.0-beta", "1.0.0"), Some(Ordering::Less));
        assert_eq!(
            compare("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare("1.0.0-alpha.9", "1.0.0-alpha.beta"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare("1.0.0-alpha", "1.0.0-alpha.1"),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn a_version_the_app_cannot_read_compares_with_nothing() {
        for unreadable in [
            "",
            "1.0",
            "1.0.0.0",
            "one.two.three",
            "1..0",
            "+1.0.0",
            "v1.0.0",
            "2.1.319 (Claude Code)",
        ] {
            assert_eq!(compare(unreadable, "1.0.0"), None, "{unreadable:?}");
            assert_eq!(compare("1.0.0", unreadable), None, "{unreadable:?}");
        }
    }
}

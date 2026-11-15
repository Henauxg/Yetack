//! Whether an agent runs a Claude Code newer than the one this release was tested with. The
//! daemon reads Claude Code's JSON output, whose format can change between versions.

use std::cmp::Ordering;

use cockpit_protocol::AgentSummary;

use crate::version_order;

/// The Claude Code version this release was tested with. Each release checks the current one and
/// sets it here (docs/releasing.md).
pub const TESTED_VERSION: &str = "2.1.319";

/// The note the agent's header shows when its Claude Code is newer than [`TESTED_VERSION`].
/// `None` for the tested version, an older one, one the app cannot read, and before the agent's
/// `claude` reports one.
pub fn untested_note(summary: &AgentSummary) -> Option<String> {
    let version = summary.claude_code_version.as_ref()?;
    let order = version_order::compare(version.as_str(), TESTED_VERSION)?;
    (order == Ordering::Greater).then(|| {
        format!(
            "Claude Code {version} is newer than this release was tested with ({TESTED_VERSION})."
        )
    })
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentStatus, ClaudeCodeVersion};

    use super::*;
    use crate::state::summary;

    fn running(version: Option<&str>) -> AgentSummary {
        AgentSummary {
            claude_code_version: version.map(|text| ClaudeCodeVersion::new(text.to_owned())),
            ..summary(1, AgentStatus::Idle)
        }
    }

    #[test]
    fn a_newer_claude_code_gets_a_note() {
        assert_eq!(
            untested_note(&running(Some("999.0.0"))),
            Some(format!(
                "Claude Code 999.0.0 is newer than this release was tested with ({TESTED_VERSION})."
            ))
        );
    }

    #[test]
    fn the_tested_an_older_or_an_unreadable_version_gets_none() {
        for version in [
            Some(TESTED_VERSION),
            Some("0.0.1"),
            Some("2.1"),
            Some("next"),
            None,
        ] {
            assert_eq!(untested_note(&running(version)), None, "{version:?}");
        }
    }

    #[test]
    fn the_tested_version_is_one_the_app_reads() {
        assert_eq!(
            version_order::compare(TESTED_VERSION, TESTED_VERSION),
            Some(Ordering::Equal)
        );
    }
}

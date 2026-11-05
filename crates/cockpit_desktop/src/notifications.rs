//! Desktop notifications, through the freedesktop notification service. Clicking one opens its
//! agent.

use cockpit_protocol::AgentId;
use notify_rust::{Notification, Urgency};
use tokio::sync::mpsc;

use crate::APP_NAME;
use crate::state::{Alert, AlertKind};

/// The action a click on the notification's body triggers.
const OPEN_ACTION: &str = "default";

/// Shows the alert on a blocking thread, since the notification service answers over D-Bus, and
/// sends its agent to `opened` when the user clicks it. An agent waiting for you stays on screen
/// until you act; one that finished goes away on the service's timeout.
pub fn show(alert: Alert, opened: mpsc::UnboundedSender<AgentId>) {
    let urgency = match alert.kind {
        AlertKind::NeedsYou => Urgency::Critical,
        AlertKind::Finished => Urgency::Normal,
    };
    tokio::task::spawn_blocking(move || {
        let shown = Notification::new()
            .appname(APP_NAME)
            .urgency(urgency)
            .summary(&alert.title)
            .body(&escaped_body(&alert.body))
            .action(OPEN_ACTION, "Open")
            .show();
        match shown {
            Ok(handle) => handle.wait_for_action(|action| {
                if action == OPEN_ACTION {
                    // The window may have closed meanwhile.
                    let _ = opened.send(alert.agent);
                }
            }),
            Err(error) => tracing::warn!("cannot show a notification: {error}"),
        }
    });
}

/// The body as text in the notification markup, which most services read: the agent's own words
/// cannot add a link, an image or formatting.
fn escaped_body(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_cannot_carry_markup() {
        assert_eq!(
            escaped_body("Running <a href=\"https://example.org\">cargo</a> 2>&1"),
            "Running &lt;a href=\"https://example.org\"&gt;cargo&lt;/a&gt; 2&gt;&amp;1"
        );
    }
}

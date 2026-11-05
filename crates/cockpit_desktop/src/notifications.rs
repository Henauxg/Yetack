//! Desktop notifications, through the freedesktop notification service. Clicking one opens its
//! agent.

use cockpit_protocol::AgentId;
use notify_rust::{Notification, Urgency};
use tokio::sync::mpsc;

use crate::state::{Alert, AlertKind};

const APP_NAME: &str = "agents-cockpit";

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
            .body(&alert.body)
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

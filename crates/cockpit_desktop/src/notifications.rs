//! Desktop notifications: at most one per agent, and a click on one opens its agent. On Linux
//! they go through the freedesktop notification service, on Windows they are toasts.
//!
//! The toasts carry PowerShell's name and icon: Windows shows toasts only for an app the Start
//! menu registers, which the app is not, and notify-rust then borrows PowerShell's. A click
//! reaches the app only while the toast is on screen. Once the toast is in the notification
//! center, a click goes to PowerShell instead. notify-rust cannot withdraw a toast, so a "needs
//! you" toast stays until it times out, even after the agent is answered.

use std::collections::BTreeMap;

use cockpit_protocol::{AgentId, AgentStatus};
#[cfg(unix)]
use notify_rust::Urgency;
use notify_rust::{Notification, NotificationResponse};
use tokio::sync::{mpsc, oneshot};

#[cfg(unix)]
use crate::APP_NAME;
use crate::state::{Alert, AlertKind, AppState, notification_levels};

/// The action a click on the notification's body triggers.
#[cfg(unix)]
const OPEN_ACTION: &str = "default";

/// The notifications on screen. On Linux each runs as a task that waits on D-Bus, with no thread
/// of its own. On Windows each waits on a blocking thread while its toast is on screen.
pub struct Notifier {
    ledger: Ledger,
    /// Ends the task of each agent's notification, which then closes it.
    closers: BTreeMap<AgentId, oneshot::Sender<()>>,
    opened: mpsc::UnboundedSender<AgentId>,
}

impl Notifier {
    /// A click on a notification sends its agent to `opened`.
    pub fn new(opened: mpsc::UnboundedSender<AgentId>) -> Self {
        Self {
            ledger: Ledger::default(),
            closers: BTreeMap::new(),
            opened,
        }
    }

    /// Shows the alerts of an update, each in place of its agent's notification, and closes the
    /// notifications the state no longer calls for.
    pub fn update(&mut self, alerts: Vec<Alert>, state: &AppState) {
        for command in self.ledger.update(alerts, state) {
            match command {
                Command::Show(alert) => {
                    let (close, closed) = oneshot::channel();
                    if let Some(previous) = self.closers.insert(alert.agent, close) {
                        // The previous notification may be gone already.
                        let _ = previous.send(());
                    }
                    tokio::spawn(present(alert, closed, self.opened.clone()));
                }
                Command::Close(agent) => {
                    if let Some(close) = self.closers.remove(&agent) {
                        let _ = close.send(());
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    /// Shows the alert in place of its agent's notification, if one is on screen.
    Show(Alert),
    Close(AgentId),
}

/// Which agents have a notification on screen, and of which kind.
#[derive(Debug, Default)]
struct Ledger {
    shown: BTreeMap<AgentId, AlertKind>,
}

impl Ledger {
    /// Each alert replaces its agent's notification. A "needs you" notification closes once its
    /// agent no longer needs you; a "finished" one goes away on the service's timeout. Any
    /// notification closes once its agent is gone, or once the levels no longer let it through.
    /// An alert the levels hold back is dropped, and so is any alert while the window has the
    /// focus: you are in the app.
    fn update(&mut self, alerts: Vec<Alert>, state: &AppState) -> Vec<Command> {
        let mut commands = Vec::new();
        for alert in alerts {
            if !state.ui.window_focused
                && notification_levels::shows(state, alert.agent, alert.kind)
            {
                self.shown.insert(alert.agent, alert.kind);
                commands.push(Command::Show(alert));
            }
        }
        let stale: Vec<AgentId> = self
            .shown
            .iter()
            .filter(|&(&agent, &kind)| {
                let status = state.agents.get(&agent).map(|entry| entry.summary.status);
                match (kind, status) {
                    (_, None) => true,
                    _ if !notification_levels::shows(state, agent, kind) => true,
                    (AlertKind::NeedsYou, Some(status)) => status != AgentStatus::NeedsYou,
                    (AlertKind::Finished, Some(_)) => false,
                }
            })
            .map(|(agent, _)| *agent)
            .collect();
        for agent in stale {
            self.shown.remove(&agent);
            commands.push(Command::Close(agent));
        }
        commands
    }
}

/// Shows the alert, and sends its agent to `opened` when the user clicks it. Waits until the
/// notification goes away, or until `close` fires or is dropped, and then closes it.
#[cfg(unix)]
async fn present(
    alert: Alert,
    close: oneshot::Receiver<()>,
    opened: mpsc::UnboundedSender<AgentId>,
) {
    let urgency = match alert.kind {
        AlertKind::NeedsYou => Urgency::Critical,
        AlertKind::Finished => Urgency::Normal,
    };
    let shown = Notification::new()
        .appname(APP_NAME)
        .urgency(urgency)
        .summary(&alert.title)
        .body(&escaped_body(&alert.body))
        .action(OPEN_ACTION, "Open")
        .show_async()
        .await;
    let handle = match shown {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!("cannot show a notification: {error}");
            return;
        }
    };
    let agent = alert.agent;
    let clicked = handle.wait_for_action_async(|response| {
        if *response == NotificationResponse::Default {
            // The window may have closed meanwhile.
            let _ = opened.send(agent);
        }
    });
    tokio::select! {
        () = clicked => {}
        _ = close => handle.close_async().await,
    }
}

/// Shows the alert as a toast, and sends its agent to `opened` when the user clicks it while it
/// is on screen. Windows reports the click or the toast's end, so the thread waits no longer.
/// `close` cannot withdraw the toast.
#[cfg(windows)]
async fn present(
    alert: Alert,
    _close: oneshot::Receiver<()>,
    opened: mpsc::UnboundedSender<AgentId>,
) {
    let agent = alert.agent;
    let toast = tokio::task::spawn_blocking(move || {
        // A toast shows its text as it is: the agent's words cannot add markup.
        let handle = Notification::new()
            .summary(&alert.title)
            .body(&alert.body)
            .show()?;
        handle.wait_for_response(|response: &NotificationResponse| {
            if *response == NotificationResponse::Default {
                // The window may have closed meanwhile.
                let _ = opened.send(agent);
            }
        })
    });
    match toast.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!("cannot show a notification: {error}"),
        Err(error) => tracing::warn!("the notification's thread failed: {error}"),
    }
}

/// The body as text in the notification markup, which most services read: the agent's own words
/// cannot add a link, an image or formatting.
#[cfg(unix)]
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
    use crate::prefs::NotificationLevel;
    use cockpit_protocol::AgentSummary;

    use crate::state::{state_with, summary};

    /// A state whose window is behind another, where alerts show.
    fn away(summaries: Vec<AgentSummary>) -> AppState {
        let mut state = state_with(summaries);
        state.ui.window_focused = false;
        state
    }

    fn alert(id: u32, kind: AlertKind) -> Alert {
        Alert {
            agent: AgentId::new(id),
            kind,
            title: format!("Agent {id}"),
            body: String::new(),
        }
    }

    fn set_status(state: &mut AppState, id: u32, status: AgentStatus) {
        if let Some(entry) = state.agents.get_mut(&AgentId::new(id)) {
            entry.summary.status = status;
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_body_cannot_carry_markup() {
        assert_eq!(
            escaped_body("Running <a href=\"https://example.org\">cargo</a> 2>&1"),
            "Running &lt;a href=\"https://example.org\"&gt;cargo&lt;/a&gt; 2&gt;&amp;1"
        );
    }

    #[test]
    fn a_needs_you_notification_closes_once_the_agent_no_longer_needs_you() {
        let mut state = away(vec![
            summary(1, AgentStatus::NeedsYou),
            summary(2, AgentStatus::NeedsYou),
        ]);
        let mut ledger = Ledger::default();
        let shown = ledger.update(
            vec![alert(1, AlertKind::NeedsYou), alert(2, AlertKind::NeedsYou)],
            &state,
        );

        set_status(&mut state, 1, AgentStatus::Working);
        let answered = ledger.update(Vec::new(), &state);
        let again = ledger.update(Vec::new(), &state);

        assert_eq!(shown.len(), 2);
        assert_eq!(answered, [Command::Close(AgentId::new(1))]);
        assert!(again.is_empty());
    }

    #[test]
    fn a_new_alert_replaces_the_agents_notification() {
        let mut state = away(vec![summary(1, AgentStatus::NeedsYou)]);
        let mut ledger = Ledger::default();
        ledger.update(vec![alert(1, AlertKind::NeedsYou)], &state);

        set_status(&mut state, 1, AgentStatus::Idle);
        let finished = alert(1, AlertKind::Finished);
        let commands = ledger.update(vec![finished.clone()], &state);
        set_status(&mut state, 1, AgentStatus::Working);
        let working = ledger.update(Vec::new(), &state);

        // One Show, which takes the place of the "needs you" notification: no Close.
        assert_eq!(commands, [Command::Show(finished)]);
        assert_eq!(ledger.shown.len(), 1);
        // A "finished" notification waits for the service's timeout.
        assert!(working.is_empty());
    }

    #[test]
    fn a_removed_agent_closes_its_notification() {
        let mut state = away(vec![summary(1, AgentStatus::Idle)]);
        let mut ledger = Ledger::default();
        ledger.update(vec![alert(1, AlertKind::Finished)], &state);

        state.agents.clear();
        let commands = ledger.update(Vec::new(), &state);

        assert_eq!(commands, [Command::Close(AgentId::new(1))]);
    }

    #[test]
    fn nothing_shows_while_the_window_has_the_focus() {
        let mut state = away(vec![
            summary(1, AgentStatus::NeedsYou),
            summary(2, AgentStatus::Idle),
        ]);
        state.ui.window_focused = true;
        let mut ledger = Ledger::default();

        let focused = ledger.update(
            vec![alert(1, AlertKind::NeedsYou), alert(2, AlertKind::Finished)],
            &state,
        );
        state.ui.window_focused = false;
        let away = ledger.update(vec![alert(2, AlertKind::Finished)], &state);

        assert!(focused.is_empty());
        assert_eq!(away, [Command::Show(alert(2, AlertKind::Finished))]);
    }

    #[test]
    fn a_notification_shown_away_closes_once_answered_from_the_focused_window() {
        let mut state = away(vec![summary(1, AgentStatus::NeedsYou)]);
        let mut ledger = Ledger::default();
        ledger.update(vec![alert(1, AlertKind::NeedsYou)], &state);

        state.ui.window_focused = true;
        set_status(&mut state, 1, AgentStatus::Working);
        let answered = ledger.update(Vec::new(), &state);

        assert_eq!(answered, [Command::Close(AgentId::new(1))]);
    }

    #[test]
    fn the_levels_hold_back_an_alert_and_close_what_they_no_longer_let_through() {
        let state_of = |level| {
            let mut state = away(vec![
                summary(1, AgentStatus::Idle),
                summary(2, AgentStatus::NeedsYou),
            ]);
            notification_levels::set_agent(&mut state, AgentId::new(1), level);
            state
        };
        let needs_you_only = state_of(NotificationLevel::NeedsYou);
        let mut ledger = Ledger::default();

        let held_back = ledger.update(vec![alert(1, AlertKind::Finished)], &needs_you_only);
        let shown = ledger.update(vec![alert(2, AlertKind::NeedsYou)], &needs_you_only);
        let mut app_off = state_of(NotificationLevel::NeedsYou);
        notification_levels::set_app(&mut app_off, NotificationLevel::Off);
        let closed = ledger.update(Vec::new(), &app_off);

        assert!(held_back.is_empty());
        assert_eq!(shown.len(), 1);
        assert_eq!(closed, [Command::Close(AgentId::new(2))]);
    }
}

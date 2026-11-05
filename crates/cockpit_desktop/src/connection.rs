//! The connection task: a coroutine that owns the daemon session, applies its updates to the
//! state, and runs the requests the components ask for.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

use cockpit_client::{Backoff, Requester, Session, Transport};
use cockpit_protocol::AgentId;
use dioxus::desktop::DesktopContext;
use dioxus::prelude::*;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::state::{self, AppState, Outgoing, actions};
use crate::{demo, notifications};

/// What the window was started with.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    pub state: AppState,
    pub transport: Transport,
    pub notifications: bool,
    /// Answers requests from the sample state instead of a daemon.
    pub demo: bool,
}

/// What components hold to read and change the state and to reach the daemon.
#[derive(Clone, Copy)]
pub struct Cockpit {
    pub state: Signal<AppState>,
    daemon: Coroutine<Outgoing>,
}

impl Cockpit {
    /// Changes the state, then sends the requests the change returns and the ones the open views
    /// now need.
    pub fn change(&self, change: impl FnOnce(&mut AppState) -> Vec<Outgoing>) {
        let mut signal = self.state;
        let requests = {
            let mut state = signal.write();
            let mut requests = change(&mut state);
            requests.extend(state::needed_requests(&mut state));
            requests
        };
        for request in requests {
            self.daemon.send(request);
        }
    }

    pub fn update(&self, change: impl FnOnce(&mut AppState)) {
        self.change(|state| {
            change(state);
            Vec::new()
        });
    }
}

/// The current time, which ticks every second for the timers on screen.
#[derive(Clone, Copy)]
pub struct Clock(pub Signal<SystemTime>);

const TICK: Duration = Duration::from_secs(1);

pub fn use_clock() -> Clock {
    let mut now = use_signal(SystemTime::now);
    use_future(move || async move {
        loop {
            tokio::time::sleep(TICK).await;
            now.set(SystemTime::now());
        }
    });
    use_context_provider(|| Clock(now))
}

/// Starts the connection, and provides the `Cockpit` to every component.
pub fn use_cockpit(startup: Startup) -> Cockpit {
    let Startup {
        state: initial,
        transport,
        notifications,
        demo,
    } = startup;
    let state = use_signal(move || initial);
    let daemon = use_coroutine(move |requests: UnboundedReceiver<Outgoing>| {
        let transport = transport.clone();
        async move {
            if demo {
                run_demo(state, requests).await;
            } else {
                run(state, transport, notifications, requests).await;
            }
        }
    });
    use_context_provider(|| Cockpit { state, daemon })
}

/// Answers every request from the sample state, in order.
async fn run_demo(mut state: Signal<AppState>, mut requests: UnboundedReceiver<Outgoing>) {
    while let Some(outgoing) = requests.next().await {
        let mut queue = VecDeque::from([outgoing]);
        while let Some(Outgoing { request, purpose }) = queue.pop_front() {
            let result = demo::answer(&state.read(), &request);
            let mut state = state.write();
            state::apply_response(&mut state, request.method(), purpose, result);
            queue.extend(state::needed_requests(&mut state));
        }
    }
}

async fn run(
    mut state: Signal<AppState>,
    transport: Transport,
    notify: bool,
    mut requests: UnboundedReceiver<Outgoing>,
) {
    let mut session = Session::start(transport, Backoff::default());
    let requester = session.requester().clone();
    let (opened_sender, mut opened) = mpsc::unbounded_channel::<AgentId>();
    loop {
        tokio::select! {
            update = session.next_update() => {
                let Some(update) = update else {
                    return;
                };
                let (alerts, follow_up) = {
                    let mut state = state.write();
                    let alerts = state::apply_update(&mut state, update, Instant::now());
                    (alerts, state::needed_requests(&mut state))
                };
                for outgoing in follow_up {
                    send(state, requester.clone(), outgoing);
                }
                if notify {
                    for alert in alerts {
                        notifications::show(alert, opened_sender.clone());
                    }
                }
            }
            outgoing = requests.next() => {
                let Some(outgoing) = outgoing else {
                    return;
                };
                send(state, requester.clone(), outgoing);
            }
            Some(agent) = opened.recv() => {
                focus_window();
                let follow_up = {
                    let mut state = state.write();
                    actions::select_agent(&mut state, agent);
                    state::needed_requests(&mut state)
                };
                for outgoing in follow_up {
                    send(state, requester.clone(), outgoing);
                }
            }
        }
    }
}

/// Sends a request and applies its answer, without holding up the other requests.
fn send(mut state: Signal<AppState>, requester: Requester, outgoing: Outgoing) {
    spawn(async move {
        let Outgoing { request, purpose } = outgoing;
        let method = request.method();
        let result = requester.request(request).await;
        let follow_up = {
            let mut state = state.write();
            state::apply_response(&mut state, method, purpose, result);
            state::needed_requests(&mut state)
        };
        for outgoing in follow_up {
            send(state, requester.clone(), outgoing);
        }
    });
}

fn focus_window() {
    if let Some(desktop) = try_consume_context::<DesktopContext>() {
        desktop.window.set_minimized(false);
        desktop.window.set_focus();
    }
}

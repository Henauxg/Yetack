//! The work of the MCP sign-ins outside the state: a loop moves them on, opens their pages in the
//! browser and starts the listeners for agents on other machines (see `state/mcp_sign_in.rs`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::ReadableExt;

use crate::connection::Cockpit;
use crate::state::mcp_sign_in::{self, Effects, Listen};
use crate::{browser, sign_in_listener};

/// How often the loop looks at the sign-ins.
const STEP: Duration = Duration::from_millis(250);
/// How often a listener checks that its sign-in still waits.
const LISTENER_CHECK: Duration = Duration::from_secs(1);

/// Whether the loop runs: one loop serves every sign-in.
static DRIVING: AtomicBool = AtomicBool::new(false);

/// Runs the loop until no sign-in runs, unless it runs already. Call it after a sign-in starts.
pub fn drive(cockpit: Cockpit) {
    if DRIVING.swap(true, Ordering::SeqCst) {
        return;
    }
    spawn_forever(async move {
        loop {
            tokio::time::sleep(STEP).await;
            let now = Instant::now();
            let (running, due) = {
                let state = cockpit.state.read();
                (
                    mcp_sign_in::any_running(&state),
                    mcp_sign_in::is_due(&state, now),
                )
            };
            if !running {
                DRIVING.store(false, Ordering::SeqCst);
                // A sign-in that started meanwhile found the loop still running.
                let started = mcp_sign_in::any_running(&cockpit.state.read());
                if started && !DRIVING.swap(true, Ordering::SeqCst) {
                    continue;
                }
                return;
            }
            if due {
                let mut effects = Effects::default();
                cockpit.change(|state| {
                    let (requests, found) = mcp_sign_in::tick(state, now);
                    effects = found;
                    requests
                });
                apply(cockpit, effects);
            }
        }
    });
}

fn apply(cockpit: Cockpit, effects: Effects) {
    for url in effects.open {
        spawn_forever(async move {
            if let Err(error) = browser::open(&url).await {
                tracing::warn!("cannot open the sign-in page: {error}");
                cockpit.update(|state| {
                    state
                        .ui
                        .report(format!("Cannot open the sign-in page: {error}"));
                });
            }
        });
    }
    for listen in effects.listen {
        spawn_forever(listen_for_callback(cockpit, listen));
    }
}

/// Listens until the browser comes back, or until the sign-in stops waiting for it.
async fn listen_for_callback(cockpit: Cockpit, listen: Listen) {
    let Listen {
        agent,
        server,
        port,
        since,
        state,
    } = listen;
    let failed = |reason: String| {
        cockpit.update(|state| mcp_sign_in::listen_failed(state, agent, &server, since, reason));
    };
    let listener = match sign_in_listener::bind(port).await {
        Ok(listener) => listener,
        Err(error) => {
            failed(format!(
                "The app cannot listen on port {port} of this computer: {error}."
            ));
            return;
        }
    };
    let callback = sign_in_listener::next_callback(&listener, port, state.as_deref());
    tokio::pin!(callback);
    loop {
        tokio::select! {
            result = &mut callback => {
                match result {
                    Ok(url) => cockpit.change(|state| {
                        mcp_sign_in::received(state, agent, &server, since, url)
                    }),
                    Err(error) => failed(format!("The app stopped listening on port {port}: {error}.")),
                }
                return;
            }
            () = tokio::time::sleep(LISTENER_CHECK) => {
                let waiting = mcp_sign_in::still_listening(&cockpit.state.read(), agent, &server, since);
                if !waiting {
                    return;
                }
            }
        }
    }
}

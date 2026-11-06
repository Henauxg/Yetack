//! The connection task: a coroutine that owns the daemon session, applies its updates to the
//! state, and runs the requests the components ask for.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use cockpit_client::{Backoff, Requester, Session, Transport};
use cockpit_protocol::{AgentId, Request};
use dioxus::desktop::DesktopContext;
use dioxus::prelude::*;
use tokio::sync::mpsc;

use crate::notifications::Notifier;
use crate::state::machine_load::{LOAD_POLL, LoadView};
use crate::state::{
    self, AgentEntry, AppState, Drafts, Outgoing, Purpose, TitlePlace, Utf16Offset, actions,
};
use crate::{bridge, demo};

/// What the window was started with.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    pub state: AppState,
    pub transport: Transport,
    pub notifications: bool,
    /// Answers requests from the sample state instead of a daemon.
    pub demo: bool,
    /// Where the preferences are saved, `None` without a home folder.
    pub prefs_file: Option<PathBuf>,
}

/// What components hold to read and change the state and to reach the daemon. The drafts are a
/// signal of their own: typing changes them alone, so the views that read the state, the
/// transcript first, do not render again. The machine's load is one too: it changes every few
/// seconds, and only the views that show it render again.
#[derive(Clone, Copy)]
pub struct Cockpit {
    pub state: Signal<AppState>,
    pub drafts: Signal<Drafts>,
    pub load: Signal<LoadView>,
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

    /// Changes the state and the drafts, then sends requests as `change` does.
    pub fn change_with_drafts(
        &self,
        change: impl FnOnce(&mut AppState, &mut Drafts) -> Vec<Outgoing>,
    ) {
        let mut drafts = self.drafts;
        self.change(|state| change(state, &mut drafts.write()));
    }

    pub fn update_with_drafts(&self, change: impl FnOnce(&mut AppState, &mut Drafts)) {
        self.change_with_drafts(|state, drafts| {
            change(state, drafts);
            Vec::new()
        });
    }

    /// Changes the drafts alone: the state is read without being written.
    pub fn with_drafts<R>(&self, change: impl FnOnce(&AppState, &mut Drafts) -> R) -> R {
        let mut drafts = self.drafts;
        let state = self.state.peek();
        change(&state, &mut drafts.write())
    }

    /// Typing in the composer changes the drafts, and the state only when the palette opens or
    /// closes.
    pub fn type_in_composer(&self, agent: AgentId, text: String, caret: Utf16Offset) {
        let palette = self.with_drafts(|state, drafts| {
            actions::composer_input(state, drafts, agent, text, caret)
        });
        if let Some(palette) = palette {
            self.change(|state| actions::change_palette(state, agent, palette));
        }
    }

    /// Typing in a title field, the header's or a sidebar row's, changes the drafts alone.
    pub fn type_title(&self, text: String) {
        self.with_drafts(|state, drafts| actions::rename_text(state, drafts, text));
    }

    /// A title field at `place` lost the focus: the edit ends, unless the window lost it.
    pub fn title_field_blurred(self, place: TitlePlace) {
        bridge::save_on_blur(
            move || {
                self.change_with_drafts(|state, drafts| {
                    actions::finish_rename_at(state, drafts, place)
                });
            },
            move || self.update(|state| actions::set_title_kept_open(state, true)),
        );
    }

    /// A title field took the focus, as it opened or when the window got it back.
    pub fn title_field_focused(self) {
        let kept_open = self
            .state
            .peek()
            .ui
            .title_edit
            .as_ref()
            .is_some_and(|edit| edit.kept_open);
        if kept_open {
            self.update(|state| actions::set_title_kept_open(state, false));
        }
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
        prefs_file: _,
    } = startup;
    let state = use_signal(move || initial);
    let drafts = use_signal(Drafts::default);
    let load = use_signal(LoadView::default);
    let daemon = use_coroutine(move |requests: UnboundedReceiver<Outgoing>| {
        let transport = transport.clone();
        async move {
            if demo {
                run_demo(state, drafts, load, requests).await;
            } else {
                run(state, drafts, load, transport, notifications, requests).await;
            }
        }
    });
    use_context_provider(|| Cockpit {
        state,
        drafts,
        load,
        daemon,
    })
}

/// Shows `view`, unless the load already shows it: setting a signal renders its readers again.
fn show_load(mut load: Signal<LoadView>, view: LoadView) {
    if *load.peek() != view {
        load.set(view);
    }
}

/// Answers every request from the sample state, in order, and makes the live agent's
/// conversation grow.
async fn run_demo(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    load: Signal<LoadView>,
    mut requests: UnboundedReceiver<Outgoing>,
) {
    use futures_util::StreamExt;

    let mut ticks = tokio::time::interval(demo::LIVE_TICK);
    let mut load_ticks = tokio::time::interval(LOAD_POLL);
    let mut tick: u32 = 0;
    // The agents closed, which Undo brings back.
    let mut closed: Vec<AgentEntry> = Vec::new();
    loop {
        tokio::select! {
            _ = load_ticks.tick() => {
                let answer = demo::answer(&state.peek(), &Request::MachineLoad);
                show_load(load, LoadView::from_result(answer));
            }
            outgoing = requests.next() => {
                let Some(outgoing) = outgoing else {
                    return;
                };
                let mut queue = VecDeque::from([outgoing]);
                while let Some(Outgoing { request, purpose }) = queue.pop_front() {
                    tokio::time::sleep(demo::latency(&request)).await;
                    if let Purpose::Close { entry } = &purpose {
                        closed.push(entry.as_ref().clone());
                    }
                    let reopened = demo::reopen(&mut state.write(), &mut closed, &request);
                    let result = reopened.unwrap_or_else(|| demo::answer(&state.read(), &request));
                    let mut state = state.write();
                    if result.is_ok() {
                        demo::apply(&mut state, &request);
                    }
                    let method = request.method();
                    state::apply_response(&mut state, &mut drafts.write(), method, purpose, result);
                    queue.extend(state::needed_requests(&mut state));
                }
            }
            _ = ticks.tick() => {
                demo::live_tick(&mut state.write(), tick);
                tick = tick.wrapping_add(1);
            }
        }
    }
}

async fn run(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    load: Signal<LoadView>,
    transport: Transport,
    notify: bool,
    mut requests: UnboundedReceiver<Outgoing>,
) {
    use futures_util::StreamExt;

    let mut session = Session::start(transport, Backoff::default());
    let requester = session.requester().clone();
    let (opened_sender, mut opened) = mpsc::unbounded_channel::<AgentId>();
    let mut notifier = notify.then(|| Notifier::new(opened_sender));
    let mut load_ticks = tokio::time::interval(LOAD_POLL);
    let (load_sender, mut load_answers) = mpsc::unbounded_channel();
    // One question at a time: a slow daemon does not pile them up.
    let mut load_asked = false;
    loop {
        tokio::select! {
            _ = load_ticks.tick() => {
                if !state.peek().is_connected() {
                    show_load(load, LoadView::Waiting);
                } else if !load_asked {
                    load_asked = true;
                    let requester = requester.clone();
                    let answers = load_sender.clone();
                    spawn(async move {
                        let answer = requester.request(Request::MachineLoad).await;
                        // The connection task ended: nobody shows the load any more.
                        let _ = answers.send(answer);
                    });
                }
            }
            Some(answer) = load_answers.recv() => {
                load_asked = false;
                show_load(load, LoadView::from_result(answer));
            }
            update = session.next_update() => {
                let Some(update) = update else {
                    return;
                };
                let (alerts, follow_up) = {
                    let mut state = state.write();
                    let mut drafts = drafts.write();
                    let alerts =
                        state::apply_update(&mut state, &mut drafts, update, Instant::now());
                    (alerts, state::needed_requests(&mut state))
                };
                for outgoing in follow_up {
                    send(state, drafts, requester.clone(), outgoing);
                }
                if let Some(notifier) = &mut notifier {
                    notifier.update(alerts, &state.peek());
                }
            }
            outgoing = requests.next() => {
                let Some(outgoing) = outgoing else {
                    return;
                };
                send(state, drafts, requester.clone(), outgoing);
            }
            Some(agent) = opened.recv() => {
                focus_window();
                let follow_up = {
                    let mut state = state.write();
                    actions::select_agent(&mut state, agent);
                    state::needed_requests(&mut state)
                };
                for outgoing in follow_up {
                    send(state, drafts, requester.clone(), outgoing);
                }
            }
        }
    }
}

/// Sends a request and applies its answer, without holding up the other requests.
fn send(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    requester: Requester,
    outgoing: Outgoing,
) {
    spawn(async move {
        let Outgoing { request, purpose } = outgoing;
        let method = request.method();
        let result = requester.request(request).await;
        let follow_up = {
            let mut state = state.write();
            state::apply_response(&mut state, &mut drafts.write(), method, purpose, result);
            state::needed_requests(&mut state)
        };
        for outgoing in follow_up {
            send(state, drafts, requester.clone(), outgoing);
        }
    });
}

fn focus_window() {
    if let Some(desktop) = try_consume_context::<DesktopContext>() {
        desktop.window.set_minimized(false);
        desktop.window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use cockpit_protocol::AgentStatus;
    use dioxus::dioxus_core::NoOpMutations;

    use super::*;
    use crate::state::actions::Direction;
    use crate::state::{state_with, summary};

    thread_local! {
        static COCKPIT: RefCell<Option<Cockpit>> = const { RefCell::new(None) };
        static TRANSCRIPT_RENDERS: Cell<u32> = const { Cell::new(0) };
    }

    /// The window's signals around a transcript, with a daemon that answers nothing.
    #[component]
    fn Window() -> Element {
        let state = use_signal(|| {
            let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
            state.ui.selected = Some(AgentId::new(1));
            state
        });
        let drafts = use_signal(Drafts::default);
        let load = use_signal(LoadView::default);
        let daemon = use_coroutine(|_: UnboundedReceiver<Outgoing>| async {});
        let cockpit = use_context_provider(|| Cockpit {
            state,
            drafts,
            load,
            daemon,
        });
        COCKPIT.with(|slot| slot.replace(Some(cockpit)));
        rsx! {
            Transcript {}
        }
    }

    /// Reads the state as the transcript view does, and counts its renders.
    #[component]
    fn Transcript() -> Element {
        let cockpit: Cockpit = use_context();
        let state = cockpit.state.read();
        let _items = state
            .selected()
            .map(|entry| entry.transcript.items().count());
        TRANSCRIPT_RENDERS.with(|renders| renders.set(renders.get() + 1));
        rsx! {}
    }

    #[test]
    fn typing_does_not_render_the_transcript_again() {
        let mut dom = VirtualDom::new(Window);
        dom.rebuild_in_place();
        let cockpit = COCKPIT
            .with(|slot| *slot.borrow())
            .expect("the window provides the cockpit");
        let renders = || TRANSCRIPT_RENDERS.with(Cell::get);
        let agent = AgentId::new(1);

        for text in ["R", "Ru", "Run the tests"] {
            dom.in_runtime(|| {
                cockpit.type_in_composer(agent, text.to_owned(), Utf16Offset::end_of(text))
            });
            dom.render_immediate(&mut NoOpMutations);
        }
        dom.in_runtime(|| {
            cockpit.change_with_drafts(|state, drafts| {
                actions::start_rename(state, drafts, agent);
                Vec::new()
            });
        });
        dom.render_immediate(&mut NoOpMutations);
        let after_opening_the_title = renders();
        for text in ["C", "Ca", "Cart total"] {
            dom.in_runtime(|| cockpit.type_title(text.to_owned()));
            dom.render_immediate(&mut NoOpMutations);
        }
        let after_typing = renders();
        // F2 moves the field to the agent's sidebar row, where typing changes the drafts alone too.
        dom.in_runtime(|| {
            cockpit.change_with_drafts(|state, drafts| {
                actions::rename_key(state, drafts);
                Vec::new()
            });
        });
        dom.render_immediate(&mut NoOpMutations);
        let after_moving_the_title = renders();
        for text in ["Cart total!", "Cart totals"] {
            dom.in_runtime(|| cockpit.type_title(text.to_owned()));
            dom.render_immediate(&mut NoOpMutations);
        }
        let after_typing_in_the_row = renders();
        // The dialogs' fields, through the calls their components make.
        dom.in_runtime(|| cockpit.update_with_drafts(actions::open_new_agent));
        dom.render_immediate(&mut NoOpMutations);
        let after_opening_the_dialog = renders();
        for text in ["/work/c", "/work/calc"] {
            dom.in_runtime(|| {
                cockpit.with_drafts(|_, drafts| drafts.new_agent.folder = text.to_owned());
                cockpit.with_drafts(|_, drafts| drafts.new_agent.prompt = text.to_owned());
                cockpit.with_drafts(|_, drafts| actions::session_query(drafts, text.to_owned()));
                cockpit.with_drafts(|state, drafts| {
                    actions::session_move(state, drafts, Direction::Down);
                });
            });
            dom.render_immediate(&mut NoOpMutations);
        }
        dom.in_runtime(|| cockpit.update_with_drafts(actions::toggle_switcher));
        dom.render_immediate(&mut NoOpMutations);
        let after_opening_the_switcher = renders();
        for text in ["a", "ag"] {
            dom.in_runtime(|| {
                cockpit.with_drafts(|_, drafts| actions::switcher_query(drafts, text.to_owned()));
                cockpit.with_drafts(|state, drafts| {
                    actions::switcher_move(state, drafts, Direction::Down);
                });
            });
            dom.render_immediate(&mut NoOpMutations);
        }
        let after_typing_in_dialogs = renders();
        dom.in_runtime(|| cockpit.update(|state| state.ui.window_focused = false));
        dom.render_immediate(&mut NoOpMutations);

        // The first render, then only the ones the changes of the state call for: opening the
        // title field, moving it to the row, the new agent dialog and the switcher, then the
        // focus.
        assert_eq!(
            (
                after_opening_the_title,
                after_typing,
                after_moving_the_title,
                after_typing_in_the_row
            ),
            (2, 2, 3, 3)
        );
        assert_eq!(
            (
                after_opening_the_dialog,
                after_opening_the_switcher,
                after_typing_in_dialogs,
                renders()
            ),
            (4, 5, 5, 6)
        );
        let typed = dom.in_runtime(|| {
            let drafts = cockpit.drafts.peek();
            let draft = drafts.draft(agent).map(|draft| draft.text.clone());
            (draft, drafts.title.clone())
        });
        assert_eq!(
            typed,
            (Some("Run the tests".to_owned()), "Cart totals".to_owned())
        );
        let dialogs = dom.in_runtime(|| {
            let drafts = cockpit.drafts.peek();
            (
                drafts.new_agent.folder.clone(),
                drafts.new_agent.session_query.clone(),
                drafts.switcher.query.clone(),
            )
        });
        assert_eq!(
            dialogs,
            (
                "/work/calc".to_owned(),
                "/work/calc".to_owned(),
                "ag".to_owned()
            )
        );
    }
}

//! The performance info: a box at the bottom right that times each switch of agent, and shows
//! what holds the window's thread. "Performance info" in the slash palette turns it on and off.
//! Off, each hook returns at once.
//!
//! - `+N ms` lines count from the last switch.
//! - The top line sums the last second: daemon events applied, chat rebuilds and chat renders,
//!   with the time each took.
//! - The second line names the agents whose transcripts hold the most items.
//! - "app thread blocked": the app's own code (state updates, renders) held the window's thread.
//! - "page frame took": the web page's layout and paint held one frame.
//! - A switch whose first rows took over half a second to paint stays pinned at the top, in amber.

use std::collections::VecDeque;
use std::fmt::Display;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use cockpit_client::RequestError;
use cockpit_protocol::{Method, ResponseBody};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::AppState;

/// The lines the box keeps, newest last.
const KEPT_LINES: usize = 26;
/// The longest gap between two ticks of the box's timer that is not a stall.
const STALL: Duration = Duration::from_millis(120);
const TICK: Duration = Duration::from_millis(40);
/// A request slower than this shows, whatever it asks.
const SLOW_REQUEST: Duration = Duration::from_millis(300);
/// A switch slower than this to paint stays pinned.
const SLOW_SWITCH: Duration = Duration::from_millis(500);
const PINNED_SWITCHES: usize = 2;
const PINNED_LINES: usize = 12;
/// The agents the box names, by the items they hold.
const NAMED_AGENTS: usize = 3;

struct Log {
    switched: Option<Instant>,
    lines: VecDeque<String>,
    /// The lines since the last switch.
    switch: Vec<String>,
    painted: bool,
    pinned: VecDeque<Vec<String>>,
}

static LOG: Mutex<Log> = Mutex::new(Log {
    switched: None,
    lines: VecDeque::new(),
    switch: Vec::new(),
    painted: false,
    pinned: VecDeque::new(),
});
static ON: AtomicBool = AtomicBool::new(false);
static VERSION: AtomicU64 = AtomicU64::new(0);
static EVENTS: AtomicU64 = AtomicU64::new(0);
static EVENTS_US: AtomicU64 = AtomicU64::new(0);
static BUILDS: AtomicU64 = AtomicU64::new(0);
static BUILDS_US: AtomicU64 = AtomicU64::new(0);
static RENDERS: AtomicU64 = AtomicU64::new(0);
static RENDERS_US: AtomicU64 = AtomicU64::new(0);

/// Turns the measures on or off. Off, the log empties.
pub fn set_on(on: bool) {
    ON.store(on, Ordering::Relaxed);
    if !on && let Ok(mut log) = LOG.lock() {
        *log = Log {
            switched: None,
            lines: VecDeque::new(),
            switch: Vec::new(),
            painted: false,
            pinned: VecDeque::new(),
        };
    }
}

pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

fn push(log: &mut Log, line: String) {
    if log.lines.len() >= KEPT_LINES {
        log.lines.pop_front();
    }
    if log.switched.is_some() && log.switch.len() < PINNED_LINES * 3 {
        log.switch.push(line.clone());
    }
    log.lines.push_back(line);
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// The agent titled `title` was selected.
pub fn switched(title: &str) {
    if !is_on() {
        return;
    }
    if let Ok(mut log) = LOG.lock() {
        log.switched = Some(Instant::now());
        log.switch.clear();
        log.painted = false;
        let clock = chrono::Local::now().format("%H:%M:%S");
        push(&mut log, format!("---- {clock} switch to {title}"));
    }
}

/// A step, timed from the last switch.
pub fn note(text: impl Display) {
    if !is_on() {
        return;
    }
    if let Ok(mut log) = LOG.lock() {
        let line = match log.switched {
            Some(switched) => format!("+{:>6} ms  {text}", switched.elapsed().as_millis()),
            None => format!("           {text}"),
        };
        push(&mut log, line);
    }
}

/// The connection to the daemon changed, at this clock time.
pub fn connection(text: impl Display) {
    let clock = chrono::Local::now().format("%H:%M:%S");
    note(format_args!("{clock} {text}"));
}

/// A request's answer came after `took`. One a switch waits for always shows, with its size;
/// another only when slow.
pub fn answered(method: Method, took: Duration, result: &Result<ResponseBody, RequestError>) {
    if !is_on() {
        return;
    }
    let awaited = matches!(
        method,
        Method::AgentHistory | Method::AgentDetails | Method::AgentReadFile | Method::AgentListDir
    );
    if !awaited && took <= SLOW_REQUEST {
        return;
    }
    let bytes = result
        .as_ref()
        .ok()
        .and_then(|body| serde_json::to_vec(body).ok())
        .map_or(0, |encoded| encoded.len());
    note(format_args!(
        "{method}: answer in {} ms, {} KB",
        took.as_millis(),
        bytes / 1024
    ));
}

/// The time an applied answer held the thread.
pub fn applied(method: impl Display, took: Duration) {
    if took > Duration::from_millis(5) {
        note(format_args!("{method}: applied in {} ms", took.as_millis()));
    }
}

/// A daemon event was applied in `took`.
pub fn event(took: Duration) {
    if !is_on() {
        return;
    }
    EVENTS.fetch_add(1, Ordering::Relaxed);
    EVENTS_US.fetch_add(micros(took), Ordering::Relaxed);
    if took > Duration::from_millis(30) {
        note(format_args!("one event applied in {} ms", took.as_millis()));
    }
}

fn micros(took: Duration) -> u64 {
    u64::try_from(took.as_micros()).unwrap_or(u64::MAX)
}

/// What a [`Timer`] counts.
#[derive(Debug, Clone, Copy)]
pub enum Timed {
    /// The chat built its rows from the state.
    ChatBuild,
    /// The chat's component rendered.
    ChatRender,
}

/// Times one chat build or render, until it drops.
pub struct Timer {
    timed: Timed,
    started: Option<Instant>,
}

impl Timer {
    pub fn start(timed: Timed) -> Self {
        Self {
            timed,
            started: is_on().then(Instant::now),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let Some(started) = self.started else {
            return;
        };
        let (count, total) = match self.timed {
            Timed::ChatBuild => (&BUILDS, &BUILDS_US),
            Timed::ChatRender => (&RENDERS, &RENDERS_US),
        };
        count.fetch_add(1, Ordering::Relaxed);
        total.fetch_add(micros(started.elapsed()), Ordering::Relaxed);
    }
}

/// The chat drew `drawn` of its `rows` rows: its first frame, or the rows it opens with
/// (`opened`). The next painted frame shows too.
pub fn chat_drawn(drawn: usize, rows: usize, opened: bool) {
    if !is_on() {
        return;
    }
    if opened {
        note(format_args!(
            "chat: {drawn} of {rows} rows drawn, the rest on a scroll up"
        ));
    } else {
        note(format_args!("chat: first {drawn} of {rows} rows drawn"));
    }
    spawn(async move {
        let mut painted = document::eval(
            "await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); return 0;",
        );
        let _ = painted.recv::<f64>().await;
        let _ = painted.await;
        note(if opened {
            "chat: its rows painted"
        } else {
            "chat: first rows painted"
        });
        if !opened {
            first_painted();
        }
    });
}

/// The first rows of the switch painted: a slow switch stays pinned.
fn first_painted() {
    let Ok(mut log) = LOG.lock() else {
        return;
    };
    let slow = log
        .switched
        .is_some_and(|switched| switched.elapsed() > SLOW_SWITCH);
    let first = !log.painted;
    log.painted = true;
    if !first || !slow {
        return;
    }
    let mut kept: Vec<String> = log.switch.clone();
    kept.truncate(PINNED_LINES);
    if log.pinned.len() >= PINNED_SWITCHES {
        log.pinned.pop_front();
    }
    log.pinned.push_back(kept);
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// The agents whose transcripts hold the most items, and the total.
fn held(state: &AppState) -> String {
    let mut agents: Vec<(usize, &str)> = state
        .agents
        .values()
        .map(|entry| (entry.transcript.len(), entry.summary.title.as_str()))
        .collect();
    let total: usize = agents.iter().map(|(count, _)| count).sum();
    agents.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
    let named: Vec<String> = agents
        .iter()
        .take(NAMED_AGENTS)
        .map(|(count, title)| format!("{title} {count}"))
        .collect();
    format!(
        "items held: {} ({} agents, {total} items)",
        named.join(", "),
        agents.len()
    )
}

/// Reports the frames of the page that take long, while the box shows.
const PAGE_MONITOR: &str = r"
window.yetackPerf = true;
let last = performance.now();
function frame(now) {
    if (!window.yetackPerf) {
        return;
    }
    const gap = now - last;
    last = now;
    if (gap > 120 && !document.hidden) dioxus.send(gap);
    requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
await new Promise(() => {});
";

/// The box, in the bottom right corner, above everything, and blind to the pointer.
#[component]
pub fn PerfBox() -> Element {
    let cockpit: Cockpit = use_context();
    let mut lines = use_signal(Vec::<String>::new);
    let mut pinned = use_signal(Vec::<Vec<String>>::new);
    let mut rates = use_signal(String::new);
    let mut holding = use_signal(String::new);
    use_future(move || async move {
        let mut seen = u64::MAX;
        let mut last = Instant::now();
        let mut second = Instant::now();
        loop {
            tokio::time::sleep(TICK).await;
            let gap = last.elapsed();
            last = Instant::now();
            if gap > STALL + TICK {
                note(format_args!(
                    "app thread blocked {} ms",
                    gap.saturating_sub(TICK).as_millis()
                ));
            }
            if second.elapsed() >= Duration::from_secs(1) {
                second = Instant::now();
                rates.set(format!(
                    "last 1 s: {} events ({} ms), {} chat builds ({} ms), {} chat renders ({} ms)",
                    EVENTS.swap(0, Ordering::Relaxed),
                    EVENTS_US.swap(0, Ordering::Relaxed) / 1000,
                    BUILDS.swap(0, Ordering::Relaxed),
                    BUILDS_US.swap(0, Ordering::Relaxed) / 1000,
                    RENDERS.swap(0, Ordering::Relaxed),
                    RENDERS_US.swap(0, Ordering::Relaxed) / 1000,
                ));
                holding.set(held(&cockpit.state.peek()));
            }
            let version = VERSION.load(Ordering::Relaxed);
            if version != seen {
                seen = version;
                if let Ok(log) = LOG.lock() {
                    lines.set(log.lines.iter().cloned().collect());
                    pinned.set(log.pinned.iter().cloned().collect());
                }
            }
        }
    });
    use_hook(|| {
        spawn(async {
            let mut monitor = document::eval(PAGE_MONITOR);
            while let Ok(gap) = monitor.recv::<f64>().await {
                note(format_args!("page frame took {gap:.0} ms"));
            }
        });
    });
    use_drop(|| {
        document::eval("window.yetackPerf = false;");
    });
    rsx! {
        div { class: "perf-box", aria_hidden: true,
            for (index, switch) in pinned().iter().enumerate() {
                div { key: "pinned-{index}", class: "perf-pinned",
                    div { "slow switch:" }
                    for (line_index, line) in switch.iter().enumerate() {
                        div { key: "{line_index}", "{line}" }
                    }
                }
            }
            div { class: "perf-rates", "{rates}" }
            div { class: "perf-rates", "{holding}" }
            for (index, line) in lines().iter().enumerate() {
                div { key: "{index}", "{line}" }
            }
        }
    }
}

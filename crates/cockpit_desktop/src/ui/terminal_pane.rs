//! The terminal pane: tabs for the selected agent's terminals over a ghostty-web terminal, in the
//! bottom panel or at the right edge.
//!
//! The page owns the terminals. Its script keeps one ghostty-web terminal per shell, each in a box
//! of its own, and puts the shown one's box in the pane's screen wherever the pane is: moving the
//! pane keeps the screen and its scrollback. Output comes from the connections apart from the
//! state ([`crate::terminal_feed`]), and the keys typed go out as `terminal.input`.

use cockpit_protocol::{AgentId, TerminalId, TerminalParams, TerminalSize};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::connection::Cockpit;
use crate::machine::MachineId;
use crate::prefs::{
    DEFAULT_TERMINAL_HEIGHT, DEFAULT_TERMINAL_WIDTH, MAX_TERMINAL_SIZE, MIN_TERMINAL_SIZE,
    TerminalPlace,
};
use crate::state::terminals;
use crate::terminal_feed::FeedReceiver;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::Presence;

/// ghostty-web 0.4.0, an ES module that holds its WebAssembly inline. See
/// `assets/ghostty-web/README.md`.
const GHOSTTY_WEB: &str = include_str!("../../assets/ghostty-web/ghostty-web.js");

/// The element the shown terminal's box goes in.
const SCREEN_ID: &str = "terminal-screen";

/// Receives ghostty-web's source, then the pane's ids and the size limits, then output and
/// screens. Sends what the user types, the sizes, the titles, and which terminals it needs the
/// kept output of.
const SCRIPT: &str = r##"
const source = await dioxus.recv();
const settings = await dioxus.recv();
const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
const ghostty = await import(url);
URL.revokeObjectURL(url);
await ghostty.init();
const send = (message) => dioxus.send(message);
const css = getComputedStyle(document.documentElement);
const color = (name, fallback) => css.getPropertyValue(name).trim() || fallback;
const terminals = new Map();
const key = (agent, terminal) => `${agent}:${terminal}`;
const bytes = (base64) => Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));

// Writes output that starts at byte `offset`, past what the terminal shows already. Before the
// kept output came, it waits. After a gap, the terminal starts again from the kept output.
function write(entry, offset, data) {
  if (terminals.get(entry.name) !== entry) {
    return;
  }
  if (entry.end === null) {
    entry.pending.push([offset, data]);
    return;
  }
  if (offset > entry.end) {
    forget(entry);
    place();
    return;
  }
  const end = offset + data.length;
  if (end <= entry.end) {
    return;
  }
  entry.term.write(data.subarray(Math.max(0, entry.end - offset)));
  entry.end = end;
}

// Fits the terminal to its box once the box stops changing size. The pane grows as it opens, and
// a fit at each step would give the shell several sizes, each of which makes it redraw.
function fitWhenSettled(entry) {
  clearTimeout(entry.settle);
  entry.settle = setTimeout(() => {
    if (entry.box.isConnected && entry.box.clientWidth > 0) {
      entry.fit.fit();
    }
  }, settings.settle_ms);
}

function terminalFor(agent, terminal) {
  const found = terminals.get(key(agent, terminal));
  if (found !== undefined) {
    return found;
  }
  const box = document.createElement("div");
  box.className = "terminal-box";
  const term = new ghostty.Terminal({
    fontSize: 13,
    fontFamily: color("--mono", "monospace"),
    cursorBlink: true,
    scrollback: 5000,
    theme: {
      background: "#121212",
      foreground: "#d8d8d8",
      cursor: color("--teal", "#2dd4bf"),
      selectionBackground: "rgba(45, 212, 191, 0.35)",
    },
  });
  const fit = new ghostty.FitAddon();
  term.loadAddon(fit);
  term.open(box);
  term.onData((data) => send({ type: "input", agent, terminal, data }));
  term.onResize(({ cols, rows }) => send({ type: "resize", agent, terminal, cols, rows }));
  term.onTitleChange((title) => send({ type: "title", agent, terminal, title }));
  // The pane's shortcut shows and hides it from inside the terminal too. Unlike xterm.js,
  // ghostty-web skips a key the handler returns `true` for.
  term.attachCustomKeyEventHandler((event) => {
    if (event.type === "keydown" && event.ctrlKey && !event.altKey && !event.metaKey && event.key === "`") {
      event.preventDefault();
      send({ type: "toggle" });
      return true;
    }
    return false;
  });
  const name = key(agent, terminal);
  const entry = { name, paused: false, term, fit, box, end: null, pending: [], settle: null, observer: null };
  entry.observer = new ResizeObserver(() => fitWhenSettled(entry));
  entry.observer.observe(box);
  terminals.set(name, entry);
  send({ type: "attach", agent, terminal });
  return entry;
}

// Drops a terminal's screen. ghostty-web draws each terminal on every frame until it is disposed.
function forget(entry) {
  clearTimeout(entry.settle);
  entry.observer.disconnect();
  entry.term.dispose();
  entry.box.remove();
  terminals.delete(entry.name);
  if (shown === entry.name) {
    shown = null;
  }
}

// ghostty-web draws every open terminal on each frame, even one out of the page. Only `drawn`
// keeps drawing. A paused terminal still takes writes.
function drawOnly(drawn) {
  for (const entry of terminals.values()) {
    const term = entry.term;
    if (entry === drawn && entry.paused) {
      entry.paused = false;
      term.startRenderLoop();
    } else if (entry !== drawn && !entry.paused) {
      entry.paused = true;
      cancelAnimationFrame(term.animationFrameId);
    }
  }
}

// Puts the shown terminal's box in the screen, wherever the pane is.
let shown = null;
function place() {
  const screen = document.getElementById(settings.screen_id);
  if (screen === null) {
    // The pane closed: the terminal it shows next takes the focus, even the same one.
    shown = null;
    drawOnly(null);
    return;
  }
  const agent = Number(screen.dataset.agent);
  const terminal = Number(screen.dataset.terminal);
  if (!terminal) {
    screen.replaceChildren();
    shown = null;
    drawOnly(null);
    return;
  }
  const entry = terminalFor(agent, terminal);
  drawOnly(entry);
  if (entry.box.parentElement !== screen) {
    screen.replaceChildren(entry.box);
  }
  const now = key(agent, terminal);
  if (now !== shown) {
    shown = now;
    fitWhenSettled(entry);
    requestAnimationFrame(() => entry.term.focus());
  }
}
new MutationObserver(place).observe(document.body, {
  childList: true,
  subtree: true,
  attributes: true,
  attributeFilter: ["data-terminal"],
});
place();

// The grip at the pane's edge sets its height in the bottom panel, its width at the right.
document.addEventListener("pointerdown", (event) => {
  const grip = event.target.closest?.("[data-terminal-grip]");
  if (!grip) {
    return;
  }
  const pane = grip.parentElement;
  const vertical = grip.dataset.terminalGrip === "bottom";
  const start = vertical ? event.clientY : event.clientX;
  const startSize = vertical ? pane.offsetHeight : pane.offsetWidth;
  const clamp = (size) => Math.round(Math.min(settings.max, Math.max(settings.min, size)));
  let size = startSize;
  grip.setPointerCapture(event.pointerId);
  pane.classList.add("resizing");
  const move = (moved) => {
    size = clamp(startSize - ((vertical ? moved.clientY : moved.clientX) - start));
    pane.style.setProperty(vertical ? "height" : "width", `${size}px`);
  };
  const end = () => {
    grip.removeEventListener("pointermove", move);
    pane.classList.remove("resizing");
    send({ type: "size", size });
  };
  grip.addEventListener("pointermove", move);
  grip.addEventListener("pointerup", end, { once: true });
  grip.addEventListener("pointercancel", end, { once: true });
  event.preventDefault();
});

for (;;) {
  const message = await dioxus.recv();
  try {
  const entry = terminals.get(key(message.agent, message.terminal));
  if (message.type === "output" && entry !== undefined) {
    write(entry, message.offset, bytes(message.data));
  } else if (message.type === "screen" && entry !== undefined && entry.end === null) {
    const data = bytes(message.data);
    // ghostty-web throws on an empty write, as when the shell has written nothing yet.
    if (data.length > 0) {
      entry.term.write(data);
    }
    entry.end = message.offset + data.length;
    for (const [offset, pending] of entry.pending.splice(0)) {
      write(entry, offset, pending);
    }
  } else if (message.type === "focus" && entry !== undefined) {
    entry.term.focus();
  } else if (message.type === "live") {
    const live = new Set(message.terminals.map(({ agent, terminal }) => key(agent, terminal)));
    for (const gone of [...terminals.values()].filter((entry) => !live.has(entry.name))) {
      forget(gone);
    }
  }
  } catch (error) {
    // One message that fails leaves the loop to the next ones.
    send({ type: "problem", text: `${message.type}: ${error}` });
  }
}
"##;

#[derive(Serialize)]
struct Settings {
    screen_id: &'static str,
    /// How long the pane's size stays the same before the terminal fits it: past its opening
    /// animation.
    settle_ms: u32,
    min: u16,
    max: u16,
}

/// What the page's script says.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FromPage {
    /// A message from the app failed in the page.
    Problem {
        text: String,
    },
    Input {
        agent: AgentId,
        terminal: TerminalId,
        data: String,
    },
    Resize {
        agent: AgentId,
        terminal: TerminalId,
        cols: u16,
        rows: u16,
    },
    Title {
        agent: AgentId,
        terminal: TerminalId,
        title: String,
    },
    Attach {
        agent: AgentId,
        terminal: TerminalId,
    },
    Toggle,
    Size {
        size: u16,
    },
}

/// What the page's script receives.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToPage {
    Output {
        agent: AgentId,
        terminal: TerminalId,
        offset: u64,
        data: String,
    },
    Screen {
        agent: AgentId,
        terminal: TerminalId,
        offset: u64,
        data: String,
    },
    /// The terminals the page keeps a screen for.
    Live { terminals: Vec<TerminalParams> },
}

/// Starts the page's terminal script for the window's life, and carries output to it.
pub fn use_terminal_script(cockpit: Cockpit) {
    let feed: FeedReceiver = use_context();
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        let settings = Settings {
            screen_id: SCREEN_ID,
            settle_ms: 180,
            min: MIN_TERMINAL_SIZE,
            max: MAX_TERMINAL_SIZE,
        };
        if let Err(error) = script
            .send(GHOSTTY_WEB)
            .and_then(|()| script.send(settings))
        {
            tracing::warn!("cannot start the terminal script: {error}");
        }
        spawn(async move {
            loop {
                match script.recv::<FromPage>().await {
                    Ok(message) => receive(cockpit, message),
                    Err(error) => {
                        tracing::warn!("the terminal script stopped: {error}");
                        return;
                    }
                }
            }
        });
        if let Some(mut outputs) = feed.take() {
            spawn(async move {
                while let Some(output) = outputs.recv().await {
                    let message = ToPage::Output {
                        agent: output.agent,
                        terminal: output.terminal,
                        offset: output.offset,
                        data: output.data,
                    };
                    if script.send(message).is_err() {
                        return;
                    }
                }
            });
        }
        script
    });
    // A closed terminal's screen goes, and so do those of a daemon that restarted, whose new
    // terminals take the same ids.
    let live = use_memo(move || terminals::live(&cockpit.state.read()));
    use_effect(move || {
        let terminals = live();
        if let Err(error) = script.send(ToPage::Live { terminals }) {
            tracing::warn!("cannot drop the closed terminals: {error}");
        }
    });
    // The output a terminal kept goes to the page once, as it comes.
    use_effect(move || {
        if cockpit.state.read().ui.terminal_screens.is_empty() {
            return;
        }
        let mut state = cockpit.state;
        let screens = std::mem::take(&mut state.write().ui.terminal_screens);
        for screen in screens {
            let message = ToPage::Screen {
                agent: screen.agent,
                terminal: screen.terminal,
                offset: screen.offset,
                data: screen.data,
            };
            if let Err(error) = script.send(message) {
                tracing::warn!("cannot show a terminal's output: {error}");
            }
        }
    });
}

fn receive(cockpit: Cockpit, message: FromPage) {
    match message {
        FromPage::Input {
            agent,
            terminal,
            data,
        } => cockpit.send_terminal_keys(terminals::input(agent, terminal, data)),
        FromPage::Resize {
            agent,
            terminal,
            cols,
            rows,
        } => cockpit.send_terminal_keys(terminals::resize(
            agent,
            terminal,
            TerminalSize { cols, rows },
        )),
        FromPage::Title {
            agent,
            terminal,
            title,
        } => cockpit.update(|state| terminals::set_title(state, agent, terminal, title)),
        FromPage::Attach { agent, terminal } => {
            cockpit.change(|_| vec![terminals::attach(agent, terminal)]);
        }
        FromPage::Problem { text } => tracing::warn!("the terminal script failed on {text}"),
        FromPage::Toggle => cockpit.change(terminals::toggle_pane),
        FromPage::Size { size } => cockpit.update(|state| {
            let size = size.clamp(MIN_TERMINAL_SIZE, MAX_TERMINAL_SIZE);
            match state.prefs.terminal.place {
                TerminalPlace::Bottom => state.prefs.terminal.height = Some(size),
                TerminalPlace::Right => state.prefs.terminal.width = Some(size),
            }
        }),
    }
}

/// The pane, where the prefs put it. `place` is where this one sits in the layout, and `leaving`
/// while it folds away.
#[component]
pub fn TerminalPane(place: TerminalPlace, leaving: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let mut moving = use_signal(|| false);
    let state = cockpit.state.read();
    let Some((agent, terminals)) = terminals::selected(&state) else {
        return rsx! {};
    };
    let machine = state
        .machines
        .get(MachineId::of(agent))
        .map(|machine| machine.daemon.machine.clone())
        .unwrap_or_default();
    let prefs = state.prefs.terminal;
    let (class, grip, style) = match place {
        TerminalPlace::Bottom => (
            if leaving {
                "terminal-pane bottom leaving"
            } else {
                "terminal-pane bottom"
            },
            "bottom",
            format!(
                "height: {}px",
                prefs.height.unwrap_or(DEFAULT_TERMINAL_HEIGHT)
            ),
        ),
        TerminalPlace::Right => (
            if leaving {
                "terminal-pane right leaving"
            } else {
                "terminal-pane right"
            },
            "right",
            format!("width: {}px", prefs.width.unwrap_or(DEFAULT_TERMINAL_WIDTH)),
        ),
    };
    let shown = terminals.shown.map_or(0, TerminalId::get);
    let tabs = terminals.tabs.clone();
    let opening = terminals.opening;
    drop(state);
    let places = [
        (TerminalPlace::Bottom, "Bottom panel"),
        (TerminalPlace::Right, "Right pane"),
    ];
    rsx! {
        section { class, style, aria_label: "Terminal", inert: leaving.then_some(true),
            div { class: "terminal-grip", "data-terminal-grip": grip, aria_hidden: true }
            div { class: "terminal-bar",
                div { class: "terminal-tabs", role: "tablist",
                    for tab in tabs {
                        {
                            let terminal = tab.terminal;
                            let current = terminal.get() == shown;
                            let title = if tab.title.is_empty() { "shell".to_owned() } else { tab.title.clone() };
                            rsx! {
                                div {
                                    key: "{terminal.get()}",
                                    class: if current { "terminal-tab current" } else { "terminal-tab" },
                                    button {
                                        class: "terminal-tab-name",
                                        role: "tab",
                                        aria_selected: current,
                                        onclick: move |_| cockpit.update(|state| terminals::show(state, agent, terminal)),
                                        Icon { kind: IconKind::Terminal, size: 12 }
                                        span { class: "terminal-title", "{title}" }
                                        if let Some(exit) = tab.exited {
                                            span { class: "terminal-exited",
                                                match exit.code {
                                                    Some(code) => format!("exited {code}"),
                                                    None => "ended".to_owned(),
                                                }
                                            }
                                        }
                                    }
                                    button {
                                        class: "terminal-tab-close",
                                        aria_label: "Close this terminal",
                                        title: "Close this terminal: ends its shell",
                                        onclick: move |_| cockpit.change(|state| terminals::close(state, agent, terminal)),
                                        Icon { kind: IconKind::Close, size: 10 }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "terminal-add",
                        aria_label: "New terminal",
                        title: "New terminal",
                        disabled: opening,
                        onclick: move |_| cockpit.change(|state| terminals::open(state, agent)),
                        Icon { kind: IconKind::Plus, size: 12 }
                    }
                }
                div { class: "terminal-controls",
                    if !machine.is_empty() {
                        span { class: "terminal-machine", title: "The shell runs on this machine", "{machine}" }
                    }
                    span { class: "terminal-move-anchor",
                        button {
                            class: if moving() { "icon-button terminal-move open" } else { "icon-button terminal-move" },
                            aria_label: "Move the terminal",
                            aria_expanded: moving(),
                            title: "Move the terminal",
                            onclick: move |_| moving.toggle(),
                            Icon { kind: IconKind::PanelBottom, size: 13 }
                        }
                        Presence { open: moving(),
                            div { class: "terminal-move-menu popover", role: "menu",
                                div { class: "terminal-move-head", "Move the terminal to" }
                                for (target, label) in places {
                                    button {
                                        class: "terminal-move-item",
                                        role: "menuitemradio",
                                        aria_checked: target == prefs.place,
                                        onclick: move |_| {
                                            moving.set(false);
                                            cockpit.update(|state| terminals::move_to(state, target));
                                        },
                                        span { class: "terminal-move-check",
                                            if target == prefs.place { Icon { kind: IconKind::Check, size: 12 } }
                                        }
                                        "{label}"
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "icon-button",
                        aria_label: "Hide the terminal",
                        title: "Hide the terminal: its shells keep running",
                        onclick: move |_| cockpit.change(terminals::toggle_pane),
                        Icon { kind: IconKind::Close, size: 13 }
                    }
                }
            }
            div {
                // A pane that leaves as the other place's opens gives the terminal to that one.
                id: (!leaving).then_some(SCREEN_ID),
                class: "terminal-screen",
                // The page's script leaves the keys typed here to the terminal.
                "data-records-keys": "",
                "data-agent": "{agent.get()}",
                "data-terminal": "{shown}",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The page's script spells these words.
    #[test]
    fn the_messages_to_and_from_the_page_are_pinned() {
        let settings = Settings {
            screen_id: SCREEN_ID,
            settle_ms: 180,
            min: 120,
            max: 2000,
        };
        let agent = AgentId::new(3);
        let terminal = TerminalId::new(2);
        let to_page = [
            ToPage::Output {
                agent,
                terminal,
                offset: 4,
                data: "bHMK".to_owned(),
            },
            ToPage::Screen {
                agent,
                terminal,
                offset: 0,
                data: "bHMK".to_owned(),
            },
            ToPage::Live {
                terminals: vec![TerminalParams { agent, terminal }],
            },
        ];
        let from_page = [
            r#"{"type":"input","agent":3,"terminal":2,"data":"ls\r"}"#,
            r#"{"type":"resize","agent":3,"terminal":2,"cols":100,"rows":30}"#,
            r#"{"type":"title","agent":3,"terminal":2,"title":"vim"}"#,
            r#"{"type":"attach","agent":3,"terminal":2}"#,
            r#"{"type":"toggle"}"#,
            r#"{"type":"size","size":300}"#,
        ];

        let parsed: Vec<FromPage> = from_page
            .iter()
            .map(|message| serde_json::from_str(message).unwrap())
            .collect();

        assert_eq!(
            serde_json::to_value(settings).unwrap(),
            json!({ "screen_id": "terminal-screen", "settle_ms": 180, "min": 120, "max": 2000 })
        );
        assert_eq!(
            serde_json::to_value(to_page).unwrap(),
            json!([
                { "type": "output", "agent": 3, "terminal": 2, "offset": 4, "data": "bHMK" },
                { "type": "screen", "agent": 3, "terminal": 2, "offset": 0, "data": "bHMK" },
                { "type": "live", "terminals": [{ "agent": 3, "terminal": 2 }] },
            ])
        );
        assert!(
            matches!(
                parsed.as_slice(),
                [
                    FromPage::Input { .. },
                    FromPage::Resize {
                        cols: 100,
                        rows: 30,
                        ..
                    },
                    FromPage::Title { .. },
                    FromPage::Attach { .. },
                    FromPage::Toggle,
                    FromPage::Size { size: 300 },
                ]
            ),
            "{parsed:?}"
        );
    }
}

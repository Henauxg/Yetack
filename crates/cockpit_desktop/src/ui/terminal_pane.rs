//! The terminal pane: tabs for the selected agent's terminals over a ghostty-web terminal, in the
//! bottom panel, at the right edge, or as a tab of the file view.
//!
//! The page owns the terminals. Its script keeps one ghostty-web terminal per shell, each in a box
//! of its own, and puts the shown one's box in the pane's screen wherever the pane is: moving the
//! pane keeps the screen and its scrollback. Output comes from the connections apart from the
//! state ([`crate::terminal_feed`]), and the keys typed go out as `terminal.input`.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, TerminalId, TerminalParams, TerminalSize};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bridge;
use crate::connection::Cockpit;
use crate::keymap::{Chord, Command};
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::{
    DEFAULT_TERMINAL_HEIGHT, DEFAULT_TERMINAL_WIDTH, MAX_TERMINAL_SIZE, MIN_TERMINAL_SIZE,
    TerminalPlace,
};
use crate::render::{self, LineRef};
use crate::state::terminal_menu::{self, MenuSpot, TerminalLink};
use crate::state::{Popover, actions, terminals};
use crate::terminal_feed::FeedReceiver;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::Presence;
use crate::ui::shell_menu::ShellButton;

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

// How many times each terminal started again, after a gap or a failure. Past a few, it stays as it
// is, rather than asking the daemon for its output again and again. A screen that draws clears it.
const restarts = new Map();
const MAX_RESTARTS = 3;
// The terminal that starts again, while it did not have the focus.
let unfocused = null;
// The rows whose `path:line` references the app is finding, by the id of the request.
const linkAnswers = new Map();
let linkAsks = 0;

// Pastes `text` through `term.paste`, which brackets it for a shell that asks. Without its
// escapes, the text cannot end the paste early with a bracket's end, after which the shell would
// run the rest as typed.
function pasteInto(term, text) {
  if (text) {
    term.paste(text.replaceAll("\x1b", ""));
  }
}

// The link under the pointer, found as ghostty-web finds the one a click opens. Its hovered
// link would keep one that output or a scroll moved from under the pointer.
async function linkUnder(term, event) {
  const { canvas, renderer, linkDetector, wasmTerm } = term;
  if (!canvas || !renderer || !linkDetector || !wasmTerm) {
    return null;
  }
  const rect = canvas.getBoundingClientRect();
  const col = Math.floor((event.clientX - rect.left) / renderer.charWidth);
  const row = Math.floor((event.clientY - rect.top) / renderer.charHeight);
  const scrollback = wasmTerm.getScrollbackLength();
  const up = Math.max(0, Math.floor(term.getViewportY()));
  const line = up > 0 && row < up ? scrollback - up + row : scrollback + row - up;
  // A scan the pointer's move started marks the row before its links come, which waits on the
  // app: this lookup scans the row again and waits for them.
  linkDetector.scannedRows?.delete(line);
  const link = await linkDetector.getLinkAt(col, line);
  return link?.target ?? null;
}

// Runs a row of the right-click menu that only the page can run, then gives the terminal the
// focus back.
function act(term, action) {
  if (action === "copy" && term.hasSelection()) {
    send({ type: "copy", text: term.getSelection() });
  } else if (action === "select_all") {
    term.selectAll();
  } else if (action === "paste") {
    navigator.clipboard.readText().then(
      (text) => pasteInto(term, text),
      (error) => send({ type: "paste_refused", text: String(error) }),
    );
  }
  term.focus();
}

// A row's text, one code point per cell, so that a character's place is its column. A wide
// character fills its second cell too, so that the path after it still starts after a letter.
// `null` past the buffer.
function rowText(term, y) {
  const row = term.buffer.active.getLine(y);
  if (!row) {
    return null;
  }
  let text = "";
  // The wide character before this cell, whose second cell this is.
  let wide = null;
  for (let x = 0; x < row.length; x++) {
    const cell = row.getCell(x);
    const code = cell?.getCodepoint() ?? 0;
    const shown = code >= 32 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff);
    const character = shown ? String.fromCodePoint(code) : null;
    text += character ?? (cell?.getWidth() === 0 && wide !== null ? wide : " ");
    wide = cell?.getWidth() === 2 ? character : null;
  }
  return text;
}

// Starts a terminal again from the output the daemon kept, unless it did too often.
function restart(entry) {
  const count = restarts.get(entry.name) ?? 0;
  if (count >= MAX_RESTARTS) {
    return;
  }
  restarts.set(entry.name, count + 1);
  // Without the focus, it does not take it from the composer.
  unfocused = entry.box.contains(document.activeElement) ? null : entry.name;
  try {
    forget(entry);
    place();
  } finally {
    unfocused = null;
  }
}

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
    restart(entry);
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

// The pane's shortcut, as the app's settings set it. Ctrl stands for Cmd on macOS too, as in
// the page's other script.
let shortcut = null;
const isShortcut = (event) => shortcut !== null
  && (event.ctrlKey || event.metaKey) === shortcut.ctrl
  && event.altKey === shortcut.alt
  && event.shiftKey === shortcut.shift
  && (event.key.length === 1 ? event.key.toLowerCase() : event.key) === shortcut.key;

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
  // Every paste goes through `pasteInto`.
  // ghostty-web's other paste path, on the box itself, sends it as typed keys: the shell would run
  // each line of a pasted text as it comes.
  box.addEventListener("paste", (event) => {
    event.preventDefault();
    event.stopImmediatePropagation();
    pasteInto(term, event.clipboardData?.getData("text/plain"));
  }, { capture: true });
  // A right-click opens the app's menu, with the link under the pointer, rather than
  // ghostty-web's empty one.
  box.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    event.stopImmediatePropagation();
    const spot = {
      x: Math.round(event.clientX),
      y: Math.round(event.clientY),
      selection: term.hasSelection(),
    };
    linkUnder(term, event)
      .catch(() => null)
      .then((link) => send({ type: "menu", agent, terminal, ...spot, link }));
  }, { capture: true });
  // ghostty-web selects the cell under a click and copies it as the button comes up. A click that
  // gives the terminal the focus would then replace what the user copied with one character. Only
  // a double-click copies one cell: its word, which may hold one character.
  const selection = term.selectionManager;
  if (typeof selection?.copyToClipboard === "function") {
    let clicks = 0;
    box.addEventListener("mousedown", (event) => {
      clicks = event.detail;
    }, { capture: true });
    const copy = selection.copyToClipboard.bind(selection);
    selection.copyToClipboard = (text) => {
      const { selectionStart: start, selectionEnd: end } = selection;
      const oneCell = start && end && start.col === end.col && start.absoluteRow === end.absoluteRow;
      return oneCell && clicks !== 2 ? Promise.resolve() : copy(text);
    };
  } else {
    send({ type: "problem", text: "ghostty-web has no selection copy to keep a click from copying" });
  }
  term.onData((data) => send({ type: "input", agent, terminal, data }));
  term.onResize(({ cols, rows }) => send({ type: "resize", agent, terminal, cols, rows }));
  term.onTitleChange((title) => send({ type: "title", agent, terminal, title }));
  // The pane's shortcut shows and hides it from inside the terminal too. Unlike xterm.js,
  // ghostty-web skips a key the handler returns `true` for.
  term.attachCustomKeyEventHandler((event) => {
    if (event.type === "keydown" && isShortcut(event)) {
      event.preventDefault();
      send({ type: "toggle" });
      return true;
    }
    // Ctrl+Shift+C copies, as in Linux terminals. ghostty-web would send it as Ctrl+C, which
    // stops the command that runs. Its letter is found as ghostty-web finds it, and AltGr, which
    // is Ctrl+Alt on Windows, still types its character.
    const ascii = event.key.length === 1 && event.key.charCodeAt(0) < 128;
    const letterC = ascii ? event.key.toLowerCase() === "c" : event.code === "KeyC";
    if (event.type === "keydown" && event.ctrlKey && event.shiftKey && !event.altKey && letterC) {
      event.preventDefault();
      if (term.hasSelection()) {
        send({ type: "copy", text: term.getSelection() });
      }
      return true;
    }
    return false;
  });
  // ghostty-web opens its web links with `window.open`, which the window ignores: the app opens
  // the web ones in the browser, and the others are no links. The pointer over a link shows
  // where it leads, which a program's own link may hide behind other text.
  const providers = term.linkDetector?.providers;
  if (providers === undefined) {
    send({ type: "problem", text: "ghostty-web has no link providers to open in the browser" });
  }
  for (const provider of providers ?? []) {
    const provide = provider.provideLinks.bind(provider);
    provider.provideLinks = (y, callback) => provide(y, (links) => {
      const web = (links ?? []).filter((link) => /^https?:\/\//i.test(link.text)).map((link) => ({
        ...link,
        target: { web: link.text },
        activate: (event) => {
          if (event.ctrlKey || event.metaKey) {
            send({ type: "open_url", url: link.text });
          }
        },
        hover: (on) => {
          box.title = on ? link.text : "";
        },
      }));
      callback(web.length === 0 ? undefined : web);
    });
  }
  // `path:line` references, which the app finds as in the chat. A Ctrl+click opens the file view.
  term.registerLinkProvider({
    provideLinks(y, callback) {
      const text = rowText(term, y);
      if (text === null || !text.includes(":")) {
        callback(undefined);
        return;
      }
      const id = ++linkAsks;
      linkAnswers.set(id, (links) => {
        // Output that changed the row meanwhile would give the old text's links.
        if (rowText(term, y) !== text) {
          callback(undefined);
          return;
        }
        // A reference that a soft wrap cuts would name the wrong file or line.
        const buffer = term.buffer.active;
        const wrapped = buffer.getLine(y).isWrapped;
        const wrapsOn = buffer.getLine(y + 1)?.isWrapped ?? false;
        const cells = Array.from(text);
        const found = links
          .filter((link) => !(wrapped && link.start === 0) && !(wrapsOn && link.end === cells.length))
          .map((link) => ({
            text: cells.slice(link.start, link.end).join(""),
            range: { start: { x: link.start, y }, end: { x: link.end - 1, y } },
            target: { file: { path: link.path, line: link.line } },
            activate: (event) => {
              if (event.ctrlKey || event.metaKey) {
                send({ type: "open_file", agent, path: link.path, line: link.line });
              }
            },
          }));
        callback(found.length === 0 ? undefined : found);
      });
      send({ type: "links", id, agent, text });
    },
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
// The entry goes first, so a terminal whose dispose throws is not met again.
function forget(entry) {
  terminals.delete(entry.name);
  if (shown === entry.name) {
    shown = null;
  }
  clearTimeout(entry.settle);
  entry.observer.disconnect();
  entry.box.remove();
  try {
    entry.term.dispose();
  } catch (error) {
    send({ type: "problem", text: `dispose: ${error}` });
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
  // A terminal that moves to another place takes the focus, as one that shows anew does.
  const moved = entry.box.parentElement !== screen;
  if (moved) {
    screen.replaceChildren(entry.box);
  }
  const now = key(agent, terminal);
  if (now !== shown || moved) {
    shown = now;
    fitWhenSettled(entry);
    if (now !== unfocused) {
      requestAnimationFrame(() => entry.term.focus());
    }
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

// Acts on one message from the app.
function receive(message) {
  const entry = terminals.get(key(message.agent, message.terminal));
  if (message.type === "output" && entry !== undefined) {
    write(entry, message.offset, bytes(message.data));
  } else if (message.type === "screen" && entry !== undefined && entry.end === null) {
    const data = bytes(message.data);
    entry.end = message.offset + data.length;
    const pending = entry.pending.splice(0);
    let drawn = false;
    try {
      // ghostty-web throws on an empty write, as when the shell has written nothing yet.
      if (data.length > 0) {
        entry.term.write(data);
      }
      drawn = true;
    } finally {
      // The output that waited goes on even if the screen fails to draw.
      for (const [offset, waited] of pending) {
        write(entry, offset, waited);
      }
    }
    // Only a terminal that drew all of it, and did not start again on a gap, starts its count over.
    if (drawn && terminals.get(entry.name) === entry) {
      restarts.delete(entry.name);
    }
  } else if (message.type === "links") {
    const answer = linkAnswers.get(message.id);
    linkAnswers.delete(message.id);
    answer?.(message.links);
  } else if (message.type === "act" && entry !== undefined) {
    act(entry.term, message.action);
  } else if (message.type === "paste" && entry !== undefined) {
    pasteInto(entry.term, message.text);
    entry.term.focus();
  } else if (message.type === "shortcut") {
    shortcut = message;
  } else if (message.type === "focus" && entry !== undefined) {
    entry.term.focus();
  } else if (message.type === "live") {
    const live = new Set(message.terminals.map(({ agent, terminal }) => key(agent, terminal)));
    for (const gone of [...terminals.values()].filter((entry) => !live.has(entry.name))) {
      forget(gone);
    }
    for (const name of [...restarts.keys()].filter((name) => !live.has(name))) {
      restarts.delete(name);
    }
  }
}

for (;;) {
  const message = await dioxus.recv();
  const entry = terminals.get(key(message.agent, message.terminal));
  try {
    receive(message);
  } catch (error) {
    // A failed message must not stop the loop. The terminal it was for starts again from the
    // output the daemon kept, unless a gap already started it again.
    send({ type: "problem", text: `${message.type}: ${error}` });
    try {
      if (entry !== undefined && terminals.get(entry.name) === entry) {
        restart(entry);
      }
    } catch (again) {
      send({ type: "problem", text: `restart: ${again}` });
    }
  }
}
"##;

/// How long the pane's size stays the same before the terminal fits it: past its opening
/// animation.
pub const FIT_SETTLE_MS: u32 = 180;

#[derive(Serialize)]
struct Settings {
    screen_id: &'static str,
    /// [`FIT_SETTLE_MS`].
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
    /// A row of a terminal, one code point per cell, whose `path:line` references the page
    /// asks for.
    Links {
        id: u64,
        agent: AgentId,
        text: String,
    },
    /// A Ctrl+click on a `path:line` reference.
    OpenFile {
        agent: AgentId,
        path: String,
        line: u32,
    },
    /// A Ctrl+click on a web link, which a program may have marked with any address.
    OpenUrl {
        url: String,
    },
    /// Ctrl+Shift+C on a selection.
    Copy {
        text: String,
    },
    /// A right-click, at `x`, `y` in the window's pixels.
    Menu {
        agent: AgentId,
        terminal: TerminalId,
        x: i32,
        y: i32,
        selection: bool,
        link: Option<TerminalLink>,
    },
    /// The menu's Paste could not read the clipboard.
    PasteRefused {
        text: String,
    },
}

/// What the page's script receives.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToPage {
    /// The pane's shortcut, which also works inside a terminal.
    Shortcut {
        ctrl: bool,
        alt: bool,
        shift: bool,
        key: String,
    },
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
    /// The answer to [`FromPage::Links`].
    Links { id: u64, links: Vec<PageLink> },
    /// A row of the right-click menu that only the page can run.
    Act {
        agent: AgentId,
        terminal: TerminalId,
        action: PageAction,
    },
    /// The text the right-click menu's Paste read from the clipboard.
    #[cfg(target_os = "linux")]
    Paste {
        agent: AgentId,
        terminal: TerminalId,
        text: String,
    },
}

/// What the right-click menu asks the page to do in a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAction {
    Copy,
    /// The page reads the clipboard itself, where its web view lets it.
    #[cfg_attr(
        target_os = "linux",
        expect(
            dead_code,
            reason = "on Linux, the app reads the clipboard for the page"
        )
    )]
    Paste,
    SelectAll,
}

/// The page's terminal script, for the right-click menu.
#[derive(Clone, Copy)]
pub struct TerminalScript(document::Eval);

impl TerminalScript {
    pub fn act(self, agent: AgentId, terminal: TerminalId, action: PageAction) {
        let message = ToPage::Act {
            agent,
            terminal,
            action,
        };
        if let Err(error) = self.0.send(message) {
            tracing::warn!("cannot ask the terminal to {action:?}: {error}");
        }
    }

    #[cfg(target_os = "linux")]
    pub fn paste(self, agent: AgentId, terminal: TerminalId, text: String) {
        let message = ToPage::Paste {
            agent,
            terminal,
            text,
        };
        if let Err(error) = self.0.send(message) {
            tracing::warn!("cannot paste in the terminal: {error}");
        }
    }
}

/// Opens a terminal's web link. Only a web address opens: the opener would run any other, such
/// as a `file:` one, which a program's output may hold.
pub fn open_web_link(cockpit: Cockpit, url: String) {
    if render::is_web_url(&url) {
        cockpit.open_in_browser(url);
    } else {
        tracing::warn!("a terminal link is no web address: {url}");
        cockpit.update(|state| {
            state
                .ui
                .report(format!("Cannot open {url}: only web addresses open"));
        });
    }
}

/// A `path:line` reference in a terminal's row, from column `start` to `end`, excluded.
#[derive(Debug, Serialize)]
struct PageLink {
    start: u16,
    end: u16,
    path: String,
    line: u32,
}

impl PageLink {
    /// `found` in `row`, a row's text with one character per cell: its bytes become columns.
    fn in_row(row: &str, found: LineRef) -> Option<Self> {
        let column = |byte: usize| {
            let cells = row.get(..byte)?.chars().count();
            u16::try_from(cells).ok()
        };
        Some(Self {
            start: column(found.start)?,
            end: column(found.end)?,
            path: found.path,
            line: found.line,
        })
    }
}

/// Starts the page's terminal script for the window's life, and carries output to it.
pub fn use_terminal_script(cockpit: Cockpit) {
    let feed: FeedReceiver = use_context();
    let script = use_hook(|| {
        let mut script = document::eval(SCRIPT);
        let settings = Settings {
            screen_id: SCREEN_ID,
            settle_ms: FIT_SETTLE_MS,
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
                    Ok(message) => receive(cockpit, script, message),
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
    use_context_provider(|| TerminalScript(script));
    // A closed terminal's screen goes, and so do those of a daemon that restarted, whose new
    // terminals take the same ids.
    let live = use_memo(move || terminals::live(&cockpit.state.read()));
    use_effect(move || {
        let terminals = live();
        if let Err(error) = script.send(ToPage::Live { terminals }) {
            tracing::warn!("cannot drop the closed terminals: {error}");
        }
    });
    let shortcut = use_memo(move || cockpit.state.read().prefs.keys.chord(Command::Terminal));
    use_effect(move || {
        let Chord {
            ctrl,
            alt,
            shift,
            key,
        } = shortcut();
        let message = ToPage::Shortcut {
            ctrl,
            alt,
            shift,
            key,
        };
        if let Err(error) = script.send(message) {
            tracing::warn!("cannot give the terminal script its shortcut: {error}");
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

fn receive(cockpit: Cockpit, script: document::Eval, message: FromPage) {
    match message {
        FromPage::Links { id, agent, text } => {
            let style = cockpit
                .state
                .read()
                .agents
                .get(&agent)
                .map_or(PathStyle::Unix, |entry| {
                    PathStyle::of(&entry.summary.folder)
                });
            let links = render::line_refs(&text, style)
                .into_iter()
                .filter_map(|found| PageLink::in_row(&text, found))
                .collect();
            if let Err(error) = script.send(ToPage::Links { id, links }) {
                tracing::warn!("cannot give the terminal its links: {error}");
            }
        }
        FromPage::OpenFile { agent, path, line } => cockpit.update(|state| {
            actions::open_file(state, agent, PathBuf::from(path), Some(line));
        }),
        FromPage::OpenUrl { url } => open_web_link(cockpit, url),
        FromPage::Copy { text } => bridge::copy_to_clipboard(text),
        FromPage::Menu {
            agent,
            terminal,
            x,
            y,
            selection,
            link,
        } => cockpit.update(|state| {
            terminal_menu::open(
                state,
                MenuSpot {
                    agent,
                    terminal,
                    x,
                    y,
                    selection,
                    link,
                },
            );
        }),
        FromPage::PasteRefused { text } => {
            tracing::warn!("the terminal cannot read the clipboard: {text}");
            let keys = if cfg!(target_os = "macos") {
                "Cmd+V"
            } else {
                "Ctrl+V or Ctrl+Shift+V"
            };
            cockpit.update(|state| {
                state.ui.report(format!(
                    "The menu cannot read the clipboard here: paste with {keys}."
                ));
            });
        }
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
                // The file view's splitter sizes it.
                TerminalPlace::FileView => {}
            }
        }),
    }
}

/// The pane, where the prefs put it. `place` is where this one sits in the layout, `leaving`
/// while it folds away, and `instant` when it draws without its opening animation.
#[component]
pub fn TerminalPane(
    place: TerminalPlace,
    leaving: bool,
    #[props(default)] instant: bool,
) -> Element {
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
            Some("bottom"),
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
            Some("right"),
            format!("width: {}px", prefs.width.unwrap_or(DEFAULT_TERMINAL_WIDTH)),
        ),
        TerminalPlace::FileView => (
            if instant {
                "terminal-pane in-file-view instant"
            } else {
                "terminal-pane in-file-view"
            },
            None,
            String::new(),
        ),
    };
    let shown = terminals.shown.map_or(0, TerminalId::get);
    let tabs = terminals.tabs.clone();
    let opening = terminals.opening;
    let offers_shells = terminals.offers_shells();
    let add_title = terminals.default_shell().map_or_else(
        || "New terminal".to_owned(),
        |shell| format!("New {} terminal", shell.name),
    );
    let shell_menu_open = matches!(
        state.ui.popover,
        Some(Popover::ShellMenu { agent: shown, .. }) if shown == agent
    );
    drop(state);
    let places = [
        (TerminalPlace::Bottom, "Bottom panel"),
        (TerminalPlace::Right, "Right pane"),
        (TerminalPlace::FileView, "Tab of the file view"),
    ];
    rsx! {
        section { class, style, aria_label: "Terminal", inert: leaving.then_some(true),
            if let Some(grip) = grip {
                div { class: "terminal-grip", "data-terminal-grip": grip, aria_hidden: true }
            }
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
                    div { class: "terminal-new",
                        button {
                            class: "terminal-add",
                            aria_label: "New terminal",
                            title: "{add_title}",
                            disabled: opening,
                            onclick: move |_| cockpit.change(|state| terminals::open(state, agent, None)),
                            Icon { kind: IconKind::Plus, size: 12 }
                        }
                        ShellButton { agent, offers: offers_shells, open: shell_menu_open, disabled: opening }
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
                    // In the file view, the terminal's tab hides it.
                    if place != TerminalPlace::FileView {
                        button {
                            class: "icon-button",
                            aria_label: "Hide the terminal",
                            title: "Hide the terminal: its shells keep running",
                            onclick: move |_| cockpit.update(|state| terminals::hide(state, agent)),
                            Icon { kind: IconKind::Close, size: 13 }
                        }
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

    #[test]
    fn a_reference_after_characters_past_ascii_keeps_its_columns() {
        // Several bytes each, one cell each. The page repeats the wide "界" in its second cell.
        let row = "é 界界 src/a.rs:3 项目/src/b.rs:4";

        let links: Vec<(u16, u16)> = render::line_refs(row, PathStyle::Unix)
            .into_iter()
            .filter_map(|found| PageLink::in_row(row, found))
            .map(|link| (link.start, link.end))
            .collect();

        assert_eq!(links, [(5, 15)]);
    }

    /// The page's script spells these words.
    #[test]
    fn the_messages_to_and_from_the_page_are_pinned() {
        let settings = Settings {
            screen_id: SCREEN_ID,
            settle_ms: FIT_SETTLE_MS,
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
            ToPage::Links {
                id: 5,
                links: vec![PageLink {
                    start: 0,
                    end: 6,
                    path: "a.rs".to_owned(),
                    line: 2,
                }],
            },
            ToPage::Act {
                agent,
                terminal,
                action: PageAction::SelectAll,
            },
        ];
        let from_page = [
            r#"{"type":"input","agent":3,"terminal":2,"data":"ls\r"}"#,
            r#"{"type":"resize","agent":3,"terminal":2,"cols":100,"rows":30}"#,
            r#"{"type":"title","agent":3,"terminal":2,"title":"vim"}"#,
            r#"{"type":"attach","agent":3,"terminal":2}"#,
            r#"{"type":"toggle"}"#,
            r#"{"type":"size","size":300}"#,
            r#"{"type":"links","id":5,"agent":3,"text":"a.rs:2"}"#,
            r#"{"type":"open_file","agent":3,"path":"a.rs","line":2}"#,
            r#"{"type":"open_url","url":"https://example.org/login"}"#,
            r#"{"type":"copy","text":"ls"}"#,
            r#"{"type":"menu","agent":3,"terminal":2,"x":40,"y":60,"selection":false,"link":{"file":{"path":"a.rs","line":2}}}"#,
            r#"{"type":"menu","agent":3,"terminal":2,"x":40,"y":60,"selection":true,"link":{"web":"https://example.org"}}"#,
            r#"{"type":"menu","agent":3,"terminal":2,"x":40,"y":60,"selection":true,"link":null}"#,
            r#"{"type":"paste_refused","text":"NotAllowedError"}"#,
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
                { "type": "links", "id": 5, "links": [{ "start": 0, "end": 6, "path": "a.rs", "line": 2 }] },
                { "type": "act", "agent": 3, "terminal": 2, "action": "select_all" },
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
                    FromPage::Links { id: 5, .. },
                    FromPage::OpenFile { line: 2, .. },
                    FromPage::OpenUrl { .. },
                    FromPage::Copy { .. },
                    FromPage::Menu {
                        link: Some(TerminalLink::File { line: 2, .. }),
                        ..
                    },
                    FromPage::Menu {
                        link: Some(TerminalLink::Web(_)),
                        selection: true,
                        ..
                    },
                    FromPage::Menu { link: None, .. },
                    FromPage::PasteRefused { .. },
                ]
            ),
            "{parsed:?}"
        );
    }
}

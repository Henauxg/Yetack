//! The terminals: their tabs, in the bottom dock or among the side dock's files, over a ghostty-web
//! terminal.
//!
//! The page owns the terminals. Its script keeps one ghostty-web terminal per shell, each in a box
//! of its own, and puts each dock's shown one's box in that dock's screen: moving a terminal to
//! the other dock keeps its screen and its scrollback. Output comes from the connections apart
//! from the state ([`crate::terminal_feed`]), and the keys typed go out as `terminal.input`.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, TerminalId, TerminalParams, TerminalSize, TerminalSizeAt};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::bridge;
use crate::connection::Cockpit;
use crate::keymap::{Chord, Command};
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::{DEFAULT_TERMINAL_HEIGHT, MAX_TERMINAL_SIZE, MIN_TERMINAL_SIZE};
use crate::render::{self, LineRef};
use crate::state::docks::{self, Dock, TabKey};
use crate::state::terminal_menu::{self, MenuSpot, TerminalLink};
use crate::state::terminals::AgentTerminals;
use crate::state::{AppState, Popover, actions, terminal_tab_menu, terminals};
use crate::terminal_feed::FeedReceiver;
use crate::ui::file_view::{DockTabRow, FileShown, close_by_key, focus_files};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::shell_menu::ShellButton;
use crate::ui::splitter::BOTTOM_FILE_VIEW_ID;

/// ghostty-web 0.4.0, an ES module that holds its WebAssembly inline. See
/// `assets/ghostty-web/README.md`.
const GHOSTTY_WEB: &str = include_str!("../../assets/ghostty-web/ghostty-web.js");

// The elements that hold the terminal each dock shows: the bottom dock's, then the side dock's.
const SCREEN_ID: &str = "terminal-screen";
const SIDE_SCREEN_ID: &str = "terminal-screen-side";

/// Receives ghostty-web's source, then the pane's ids and the size limits, then output and
/// screens. Sends what the user types, the sizes, the titles, and which terminals it needs the
/// kept output of.
const SCRIPT: &str = r##"
const source = await dioxus.recv();
const settings = await dioxus.recv();
const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
const ghostty = await import(url);
URL.revokeObjectURL(url);
// Each terminal has a ghostty-web instance of its own: the library gives a new terminal a closed
// one's memory without zeroing it, and the closed one's text would show in it
// (coder/ghostty-web#141). An instance takes a few milliseconds to load, so the next one loads
// ahead.
let spare = await ghostty.Ghostty.load();
let loading = false;
// Loads that failed in a row. Past a few, the page loads no more instances.
let loadFailures = 0;
const MAX_LOAD_FAILURES = 3;
// Whether `place` waits for an instance, and the terminal it then shows without the focus.
let placeOnLoad = false;
let unfocusedOnLoad = null;
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
// The output the daemon keeps of each terminal, in bytes, as `terminal.attach` answers it.
const KEPT_BYTES = 512 * 1024;
// A new terminal's history: bytes, not lines (coder/ghostty-web#140).
let scrollbackBytes = settings.scrollback_bytes;
// The largest size the kept output replays at. Any client can give the PTY a size, and
// ghostty-web would hold a cell for each of a 65535 by 65535 screen. A real pane is far smaller.
const MAX_COLS = 1000;
const MAX_ROWS = 500;
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
    term.scrollToBottom();
  }
}

// Writes output without moving a view the user scrolled up. ghostty-web goes to the bottom at
// each write, so this moves the view back up by the lines the write pushed into the scrollback.
// At the bottom, or less than a line above it, which looks the same, the view follows the output.
function writeKeepingView(term, data) {
  const up = term.getViewportY();
  if (up < 1) {
    term.write(data);
    return;
  }
  const before = term.getScrollbackLength();
  const top = before - Math.floor(up);
  const shown = viewText(term, top, up);
  term.write(data);
  const after = term.getScrollbackLength();
  const pushed = after - before + droppedLines(term, top, up, shown, Math.max(0, before - after));
  pushedLines.set(term, (pushedLines.get(term) ?? 0) + pushed);
  term.scrollToLine(up + pushed);
  // A wheel scroll still under way aims at lines counted from the old bottom: its target moves
  // too, unless it heads for the bottom.
  if (term.scrollAnimationFrame !== undefined && term.targetViewportY > 0) {
    term.targetViewportY += pushed;
  }
}

// The lines output pushed into each terminal's scrollback under a view kept up, for a scrollbar
// drag to keep its lines too.
const pushedLines = new WeakMap();

// How many lines to compare to find the view's lines again.
const VIEW_TEXT_LINES = 3;

// The text of scrollback line `index`, with the cells `rowText` would show as spaces.
function lineText(term, index) {
  const cells = term.getScrollbackLine(index) ?? [];
  const text = cells.map(({ codepoint: code }) => {
    const shown = code >= 32 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff);
    return shown ? String.fromCodePoint(code) : " ";
  });
  return text.join("").trimEnd();
}

// The text of the first lines the view shows, from scrollback line `top`: `VIEW_TEXT_LINES` of
// them, or fewer when the view is fewer lines up.
function viewText(term, top, up) {
  const count = Math.min(VIEW_TEXT_LINES, Math.floor(up));
  return Array.from({ length: count }, (_, line) => lineText(term, top + line));
}

// How many of the oldest lines the write dropped. ghostty-web drops them a page at a time once
// the scrollback is full, so the view's lines are that many lines earlier. At least `least` lines
// went: the lines the scrollback lost. Starting there, blank or repeated lines can match a wrong
// place only within the lines the write pushed. When the view's own lines are gone too, this
// returns `top`, all the lines before the view.
function droppedLines(term, top, up, shown, least) {
  for (let dropped = least; dropped <= top; dropped++) {
    // Comparing the first line alone rules out most places, at a third of the cost.
    if (lineText(term, top - dropped) !== shown[0]) {
      continue;
    }
    const found = viewText(term, top - dropped, up);
    if (found.every((line, index) => line === shown[index])) {
      return dropped;
    }
  }
  return top;
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
  } else if (action === "clear") {
    term.scrollToBottom();
    // Ctrl+L.
    term.input("\x0c", true);
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
    if (entry.lost) {
      return;
    }
    entry.pending.push([offset, data]);
    entry.waiting += data.length;
    // Before its first fit, as while its box keeps changing size, a terminal would keep a busy
    // shell's output without end. Past what the daemon keeps, a new attach at that fit gives as
    // much.
    if (!entry.fitted && entry.waiting > KEPT_BYTES) {
      lose(entry);
    }
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
  writeKeepingView(entry.term, data.subarray(Math.max(0, entry.end - offset)));
  entry.end = end;
  entry.scrollbar();
}

// Drops the kept output and the output that waits with it, before the terminal's first fit, and
// has the page attach again at that fit. The output that comes meanwhile is dropped too. A fresh
// screen dropped here keeps the next one fresh, since nothing answered its queries.
function lose(entry) {
  entry.lost = true;
  entry.freshLost ||= entry.screen?.fresh === true;
  entry.screen = null;
  entry.pending = [];
  entry.waiting = 0;
}

// Fits the terminal to its box once the box stops changing size. The pane grows as it opens, and
// a fit at each step would give the shell several sizes, each of which makes it redraw.
function fitWhenSettled(entry) {
  clearTimeout(entry.settle);
  entry.settle = setTimeout(() => {
    if (!entry.box.isConnected || entry.box.clientWidth === 0) {
      // Hidden before its first fit, it could show the output that waits much later, as live:
      // the shell would get answers to queries that stopped waiting. It attaches again instead.
      if (!entry.fitted) {
        lose(entry);
      }
      return;
    }
    entry.fit.fit();
    entry.fitted = true;
    if (entry.lost) {
      entry.lost = false;
      entry.attach();
    }
    try {
      showWaitingScreen(entry);
    } catch (error) {
      send({ type: "problem", text: `screen: ${error}` });
      if (terminals.get(entry.name) === entry) {
        restart(entry);
      }
    }
  }, settings.settle_ms);
}

// Shows the kept output that came, once the terminal has the pane's size: before its first fit,
// it has ghostty-web's 80 by 24, and the output would wrap there.
function showWaitingScreen(entry) {
  if (!entry.fitted || entry.screen === null) {
    return;
  }
  const message = entry.screen;
  entry.screen = null;
  const fresh = message.fresh || entry.freshLost;
  entry.freshLost = false;
  const data = bytes(message.data);
  entry.end = message.offset + data.length;
  const pending = entry.pending.splice(0);
  entry.waiting = 0;
  let drawn = false;
  try {
    replay(entry, message.offset, data, message.sizes, fresh);
    entry.scrollbar();
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
}

// Gives the terminal `size`, unless it has it or it is out of bounds.
function resizeTo(term, size) {
  const fits = size.cols >= 1 && size.rows >= 1 && size.cols <= MAX_COLS && size.rows <= MAX_ROWS;
  if (fits && (size.cols !== term.cols || size.rows !== term.rows)) {
    term.resize(size.cols, size.rows);
  }
}

// Takes the answers to queries that ghostty-web still holds: a write emits at most 256 bytes of
// them, and the next write would send the rest to the shell. With `keep`, they go to the shell
// now, after the ones the writes emitted; without it, they go nowhere.
function takeAnswers(entry, keep) {
  const wasm = entry.term.wasmTerm;
  if (typeof wasm?.readResponse !== "function") {
    send({ type: "problem", text: "ghostty-web has no response reader to take a replay's answers" });
    return;
  }
  let answer = wasm.readResponse();
  while (answer !== null) {
    if (keep) {
      entry.sendInput(answer);
    }
    answer = wasm.readResponse();
  }
}

// Writes the kept output `data`, which starts at byte `start`, at the sizes the shell wrote each
// part for, then gives the terminal the pane's size again: each cursor move lands, and each line
// wraps, where it did. Without `sizes`, from a daemon that predates them, it all goes in at the
// pane's size. The PTY learns of no size the replay takes, and the shell gets the answers to the
// output's queries only for a `fresh` terminal.
function replay(entry, start, data, sizes, fresh) {
  const term = entry.term;
  const pane = { cols: term.cols, rows: term.rows };
  const at = (offset) => Math.min(data.length, Math.max(0, offset - start));
  const parts = sizes.length === 0
    ? [{ from: 0, size: pane }]
    : sizes.map(({ offset, size }, index) => ({ from: index === 0 ? 0 : at(offset), size }));
  entry.replaying.on = true;
  entry.replaying.answers = fresh;
  try {
    for (const [index, part] of parts.entries()) {
      const to = parts[index + 1]?.from ?? data.length;
      resizeTo(term, part.size);
      // ghostty-web throws on an empty write, as when the shell has written nothing yet.
      if (to > part.from) {
        term.write(data.subarray(part.from, to));
      }
    }
  } finally {
    // A terminal that fails here and may not start again still takes the keys typed.
    try {
      resizeTo(term, pane);
      takeAnswers(entry, fresh);
    } finally {
      entry.replaying.on = false;
      entry.replaying.answers = false;
    }
  }
  // The PTY keeps the size another client gave it, or the one it opened with, until told.
  const last = sizes.at(-1)?.size;
  if (last !== undefined && (last.cols !== pane.cols || last.rows !== pane.rows)) {
    entry.sendSize(pane);
  }
}

// The scrollbar's thumb is never shorter than this, in pixels.
const MIN_THUMB = 20;

// ghostty-web paints its scrollbar on its canvas, over the last column, where the pointer keeps
// the text's shape, and its fit leaves the strip at the canvas's right empty. The pane draws a
// scrollbar of its own in that strip instead, styled as the app's other scrollbars. Returns a
// function that redraws the thumb, at most once per frame.
function scrollbarFor(term, box) {
  const bar = document.createElement("div");
  bar.className = "terminal-scrollbar";
  bar.hidden = true;
  const thumb = document.createElement("div");
  thumb.className = "terminal-scrollbar-thumb";
  bar.append(thumb);
  box.append(bar);
  if (typeof term.renderer?.renderScrollbar === "function" && typeof term.handleMouseDown === "function") {
    term.renderer.renderScrollbar = () => {};
    // Its click handler takes the presses on the last column, where its scrollbar no longer shows.
    box.removeEventListener("mousedown", term.handleMouseDown, { capture: true });
  } else {
    send({ type: "problem", text: "ghostty-web has no painted scrollbar or click handler to turn off" });
  }
  let frame = 0;
  const update = () => {
    frame = 0;
    const scrollback = term.getScrollbackLength();
    bar.hidden = scrollback === 0;
    if (scrollback === 0) {
      return;
    }
    const track = bar.clientHeight;
    const size = Math.min(track, Math.max(MIN_THUMB, (track * term.rows) / (scrollback + term.rows)));
    // How many lines the view is above the end.
    const up = Math.min(scrollback, Math.max(0, term.getViewportY()));
    thumb.style.height = `${size}px`;
    thumb.style.transform = `translateY(${(track - size) * (1 - up / scrollback)}px)`;
  };
  const later = () => {
    if (frame === 0) {
      frame = requestAnimationFrame(update);
    }
  };
  // The terminal keeps the focus.
  bar.addEventListener("mousedown", (event) => event.preventDefault());
  // ghostty-web clears the selection on a click outside its canvas.
  bar.addEventListener("click", (event) => event.stopPropagation());
  // A press on the track brings the thumb's middle under the pointer.
  bar.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || event.target === thumb) {
      return;
    }
    const rect = bar.getBoundingClientRect();
    const room = rect.height - thumb.offsetHeight;
    if (room <= 0) {
      return;
    }
    const along = (event.clientY - rect.top - thumb.offsetHeight / 2) / room;
    term.scrollToLine(Math.round((1 - Math.min(1, Math.max(0, along))) * term.getScrollbackLength()));
  });
  thumb.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) {
      return;
    }
    const startY = event.clientY;
    const startUp = term.getViewportY();
    const startPushed = pushedLines.get(term) ?? 0;
    thumb.setPointerCapture(event.pointerId);
    bar.classList.add("dragging");
    const end = () => {
      thumb.removeEventListener("pointermove", move);
      thumb.removeEventListener("lostpointercapture", end);
      bar.classList.remove("dragging");
    };
    const move = (moved) => {
      // A box taken out of the page mid-drag loses the capture without telling the thumb.
      if (!thumb.hasPointerCapture(moved.pointerId)) {
        end();
        return;
      }
      const room = bar.clientHeight - thumb.offsetHeight;
      if (room > 0) {
        // The lines output pushed in since the drag began stay where they are.
        const pushed = (pushedLines.get(term) ?? 0) - startPushed;
        const lines = ((moved.clientY - startY) * term.getScrollbackLength()) / room;
        term.scrollToLine(Math.round(startUp + pushed - lines));
      }
    };
    thumb.addEventListener("pointermove", move);
    thumb.addEventListener("lostpointercapture", end);
  });
  term.onScroll(later);
  term.onResize(later);
  later();
  return later;
}

// The pane's shortcut, as the app's settings set it. Ctrl stands for Cmd on macOS too, as in
// the page's other script.
let shortcut = null;
const isShortcut = (event) => shortcut !== null
  && (event.ctrlKey || event.metaKey) === shortcut.ctrl
  && event.altKey === shortcut.alt
  && event.shiftKey === shortcut.shift
  && (event.key.length === 1 ? event.key.toLowerCase() : event.key) === shortcut.key;

// Takes the instance loaded ahead, and loads the next one. `null` while none is ready.
function takeInstance() {
  const instance = spare;
  spare = null;
  loadSpare();
  return instance;
}

// Loads the next instance, unless one is ready or loading, or the last loads failed. A `place`
// that waited for it runs again once it ends.
function loadSpare() {
  if (spare !== null || loading || loadFailures >= MAX_LOAD_FAILURES) {
    return;
  }
  loading = true;
  ghostty.Ghostty.load().then(
    (instance) => {
      spare = instance;
      loadFailures = 0;
    },
    (error) => {
      loadFailures += 1;
      send({ type: "problem", text: `loading a ghostty-web instance: ${error}` });
    },
  ).finally(() => {
    loading = false;
    if (placeOnLoad) {
      placeOnLoad = false;
      unfocused = unfocusedOnLoad;
      unfocusedOnLoad = null;
      try {
        place();
      } finally {
        unfocused = null;
      }
    }
  });
}

// Opens `term` in `box`, and returns the listeners ghostty-web put on the document meanwhile:
// `dispose` leaves two of them, which would keep the closed terminal and its memory alive.
// `open` would also focus the terminal, on a timer that fires once the box shows, even for a
// terminal that starts again while the user types elsewhere. `place` gives the focus instead.
function openIn(term, box) {
  const added = [];
  const add = document.addEventListener;
  document.addEventListener = function (type, listener, options) {
    added.push([type, listener, options]);
    return add.call(this, type, listener, options);
  };
  const focus = term.focus;
  term.focus = () => {};
  try {
    term.open(box);
  } finally {
    document.addEventListener = add;
    term.focus = focus;
  }
  return added;
}

// The terminal's entry, made on first use. `null` while no instance is ready for a new one.
function terminalFor(agent, terminal) {
  const found = terminals.get(key(agent, terminal));
  if (found !== undefined) {
    return found;
  }
  const instance = takeInstance();
  if (instance === null) {
    return null;
  }
  const box = document.createElement("div");
  box.className = "terminal-box";
  const term = new ghostty.Terminal({
    ghostty: instance,
    fontSize: 13,
    fontFamily: color("--mono", "monospace"),
    cursorBlink: true,
    scrollback: scrollbackBytes,
    theme: {
      background: "#121212",
      foreground: "#d8d8d8",
      cursor: color("--teal", "#2dd4bf"),
      selectionBackground: "rgba(45, 212, 191, 0.35)",
    },
  });
  const fit = new ghostty.FitAddon();
  term.loadAddon(fit);
  const documentListeners = openIn(term, box);
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
  // What the keyboard sends the shell, keys and an input method's text, brings the view back to
  // the bottom, where it shows what was typed. Keys that send nothing, as Cmd+C, leave the view
  // where it is. So do the answers to the shell's queries, which ghostty-web sends another way.
  const input = term.inputHandler;
  if (typeof input?.onDataCallback === "function") {
    const typed = input.onDataCallback;
    input.onDataCallback = (data) => {
      term.scrollToBottom();
      typed(data);
    };
  } else {
    send({ type: "problem", text: "ghostty-web has no input callback to bring the view down on" });
  }
  // While the kept output replays, the PTY keeps the pane's size. ghostty-web answers the queries
  // the output holds, such as the cursor's place, and the shell reads those answers as typed: only
  // the answers to a fresh terminal's queries reach it, since older ones were answered long ago.
  const replaying = { on: false, answers: false };
  const sendInput = (data) => send({ type: "input", agent, terminal, data });
  term.onData((data) => {
    if (!replaying.on || replaying.answers) {
      sendInput(data);
    }
  });
  const sendSize = ({ cols, rows }) => send({ type: "resize", agent, terminal, cols, rows });
  term.onResize((size) => {
    if (!replaying.on) {
      sendSize(size);
    }
  });
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
  const scrollbar = scrollbarFor(term, box);
  const name = key(agent, terminal);
  const attach = () => send({ type: "attach", agent, terminal });
  const entry = {
    name,
    paused: false,
    term,
    fit,
    box,
    scrollbar,
    sendSize,
    sendInput,
    replaying,
    attach,
    documentListeners,
    // Whether the terminal took the pane's size once, and the kept output that waits for it.
    fitted: false,
    screen: null,
    end: null,
    // The output that waits for the kept output to show, and its size in bytes.
    pending: [],
    waiting: 0,
    // Whether output was dropped before the first fit: the terminal attaches again then. And
    // whether a fresh screen went with it, so that the next one counts as fresh.
    lost: false,
    freshLost: false,
    settle: null,
    observer: null,
  };
  entry.observer = new ResizeObserver(() => {
    fitWhenSettled(entry);
    scrollbar();
  });
  entry.observer.observe(box);
  terminals.set(name, entry);
  attach();
  return entry;
}

// Drops a terminal's screen. ghostty-web draws each terminal on every frame until it is disposed.
// The entry goes first, so a terminal whose dispose throws is not met again.
function forget(entry) {
  terminals.delete(entry.name);
  for (const [screen, name] of shownIn) {
    if (name === entry.name) {
      shownIn.delete(screen);
    }
  }
  clearTimeout(entry.settle);
  entry.observer.disconnect();
  entry.box.remove();
  try {
    entry.term.dispose();
  } catch (error) {
    send({ type: "problem", text: `dispose: ${error}` });
  }
  for (const [type, listener, options] of entry.documentListeners) {
    document.removeEventListener(type, listener, options);
  }
  // A WebView2 page holds 124 instances. The closed one's memory frees, so loads may work again.
  loadFailures = 0;
  loadSpare();
}

// ghostty-web draws every open terminal on each frame, even one out of the page. Only the ones in
// `drawn` keep drawing. A paused terminal still takes writes.
function drawOnly(drawn) {
  for (const entry of terminals.values()) {
    const term = entry.term;
    if (drawn.has(entry) && entry.paused) {
      entry.paused = false;
      term.startRenderLoop();
    } else if (!drawn.has(entry) && !entry.paused) {
      entry.paused = true;
      cancelAnimationFrame(term.animationFrameId);
    }
  }
}

// The terminal each screen showed last, by the screen's id.
const shownIn = new Map();
// Puts the terminal each dock shows in that dock's screen. Only these terminals keep drawing.
function place() {
  const drawn = new Set();
  for (const id of settings.screen_ids) {
    const entry = placeIn(id);
    if (entry !== null) {
      drawn.add(entry);
    }
  }
  drawOnly(drawn);
}
// Puts the terminal that the screen `id` names into that screen. A terminal that shows there anew
// takes the focus.
function placeIn(id) {
  const screen = document.getElementById(id);
  if (screen === null) {
    // The dock closed: the terminal it shows next takes the focus, even the same one.
    shownIn.delete(id);
    return null;
  }
  const agent = Number(screen.dataset.agent);
  const terminal = Number(screen.dataset.terminal);
  if (!terminal) {
    screen.replaceChildren();
    shownIn.delete(id);
    return null;
  }
  const entry = terminalFor(agent, terminal);
  if (entry === null) {
    // Another terminal left on screen meanwhile would take the keys typed.
    screen.replaceChildren();
    shownIn.delete(id);
    placeOnLoad = true;
    unfocusedOnLoad = unfocused ?? unfocusedOnLoad;
    return null;
  }
  // A terminal that moves to another dock takes the focus, as one that shows anew does.
  const moved = entry.box.parentElement !== screen;
  if (moved) {
    screen.replaceChildren(entry.box);
  }
  const now = key(agent, terminal);
  if (now !== shownIn.get(id) || moved) {
    shownIn.set(id, now);
    fitWhenSettled(entry);
    if (now !== unfocused) {
      requestAnimationFrame(() => entry.term.focus());
    }
  }
  return entry;
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
    if (entry.lost) {
      entry.freshLost ||= message.fresh === true;
    } else {
      entry.screen = message;
      showWaitingScreen(entry);
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
  } else if (message.type === "scrollback") {
    scrollbackBytes = message.bytes;
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
    /// The screens of the bottom dock and of the side dock.
    screen_ids: [&'static str; 2],
    /// [`FIT_SETTLE_MS`].
    settle_ms: u32,
    min: u16,
    max: u16,
    /// [`crate::prefs::Scrollback::bytes`] as the script starts: the pane may open a terminal
    /// before the script reads its first message.
    scrollback_bytes: u32,
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
    /// The history the terminals opened from now on keep: [`crate::prefs::Scrollback::bytes`].
    Scrollback { bytes: u32 },
    Output {
        agent: AgentId,
        terminal: TerminalId,
        offset: u64,
        data: String,
    },
    /// The output a terminal keeps, the sizes it was written at, and whether it is all recent
    /// ([`terminals::Screen::fresh`]).
    Screen {
        agent: AgentId,
        terminal: TerminalId,
        offset: u64,
        data: String,
        sizes: Vec<TerminalSizeAt>,
        fresh: bool,
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
    /// Ctrl+L to the shell, which clears the screen, with the view at the bottom to show it.
    Clear,
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
            screen_ids: [SCREEN_ID, SIDE_SCREEN_ID],
            settle_ms: FIT_SETTLE_MS,
            min: MIN_TERMINAL_SIZE,
            max: MAX_TERMINAL_SIZE,
            scrollback_bytes: cockpit.state.peek().prefs.terminal.scrollback.bytes(),
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
    let scrollback = use_memo(move || cockpit.state.read().prefs.terminal.scrollback);
    use_effect(move || {
        let bytes = scrollback().bytes();
        if let Err(error) = script.send(ToPage::Scrollback { bytes }) {
            tracing::warn!("cannot give the terminal script its scrollback: {error}");
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
                sizes: screen.sizes,
                fresh: screen.fresh,
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
        // The grip is on the bottom dock: the file view's splitter sizes the side dock.
        FromPage::Size { size } => cockpit.update(|state| {
            state.prefs.terminal.height = Some(size.clamp(MIN_TERMINAL_SIZE, MAX_TERMINAL_SIZE));
        }),
    }
}

/// The bottom dock: the tabs of the terminals that sit there, and the shown one's screen.
/// `leaving` while it folds away.
#[component]
pub fn TerminalPane(leaving: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some((agent, _)) = terminals::selected(&state) else {
        return rsx! {};
    };
    let machine = state
        .machines
        .get(MachineId::of(agent))
        .map(|machine| machine.daemon.machine.clone())
        .unwrap_or_default();
    let height = state.prefs.terminal.height;
    let class = if leaving {
        "terminal-pane bottom leaving"
    } else {
        "terminal-pane bottom"
    };
    let style = format!("height: {}px", height.unwrap_or(DEFAULT_TERMINAL_HEIGHT));
    let shown = terminals::bottom_terminal(&state, agent);
    let shows_file = terminals::bottom_shows_file(&state, agent);
    let shown_file = state
        .ui
        .file_view_in(agent, Dock::Bottom)
        .filter(|_| shows_file)
        .map(|view| view.path.clone());
    let active = state.ui.active_files_dock(agent) == Dock::Bottom;
    drop(state);
    rsx! {
        section {
            class,
            style,
            aria_label: "Bottom dock",
            inert: leaving.then_some(true),
            div { class: "terminal-grip", "data-terminal-grip": "bottom", aria_hidden: true }
            div { class: "terminal-bar",
                DockTabRow { agent, dock: Dock::Bottom }
                NewTerminal { agent, dock: Dock::Bottom }
                div { class: "terminal-controls",
                    if !machine.is_empty() {
                        span { class: "terminal-machine", title: "The shell runs on this machine", "{machine}" }
                    }
                    button {
                        class: "icon-button",
                        aria_label: "Close the bottom panel",
                        title: "Close the files here, and hide the terminals: their shells keep running",
                        onclick: move |_| cockpit.update(|state| actions::close_bottom_dock(state, agent)),
                        Icon { kind: IconKind::Close, size: 13 }
                    }
                }
            }
            if shows_file {
                section {
                    id: (!leaving).then_some(BOTTOM_FILE_VIEW_ID),
                    class: "file-view in-bottom",
                    aria_label: "File view",
                    "data-file-view": "",
                    "data-active": active.then_some(""),
                    tabindex: -1,
                    onpointerdown: move |_| focus_files(cockpit, agent, Dock::Bottom, active),
                    onkeydown: move |event| close_by_key(cockpit, agent, shown_file.clone(), &event),
                    FileShown { agent, dock: Dock::Bottom }
                }
            } else {
                TerminalScreen { agent, terminal: shown, dock: Dock::Bottom, leaving }
            }
        }
    }
}

/// What the tab of `terminal` shows.
pub fn tab_view(
    state: &AppState,
    agent: AgentId,
    terminals: &AgentTerminals,
    terminal: TerminalId,
    shown: Option<TerminalId>,
) -> Option<TerminalTabViewProps> {
    let tab = terminals.tab(terminal)?;
    let title = if tab.title.is_empty() {
        "shell".to_owned()
    } else {
        tab.title.clone()
    };
    let exited = tab.exited.map(|exit| match exit.code {
        Some(code) => format!("exit {code}"),
        None => "ended".to_owned(),
    });
    Some(TerminalTabViewProps {
        agent,
        terminal,
        title,
        exited,
        selected: shown == Some(terminal),
        pinned: docks::is_pinned(state, agent, &TabKey::Terminal(terminal)),
    })
}

/// A terminal's tab, which looks as a file's: its "×" ends the shell, and a pinned tab shows a
/// pin, which unpins it, in its place.
#[component]
pub fn TerminalTabView(
    agent: AgentId,
    terminal: TerminalId,
    title: String,
    exited: Option<String>,
    selected: bool,
    pinned: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let key = TabKey::Terminal(terminal);
    let page_key = key.to_page();
    rsx! {
        div {
            class: if selected { "file-tab terminal-tab selected" } else { "file-tab terminal-tab" },
            role: "tab",
            aria_selected: selected,
            "data-tab": "{page_key}",
            "data-pinned": pinned.then_some(""),
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                cockpit.update(|state| terminal_tab_menu::open(state, agent, terminal, x, y));
            },
            button {
                class: "file-tab-name",
                onclick: move |_| cockpit.update(|state| terminals::show(state, agent, terminal)),
                span { class: "terminal-tab-icon", Icon { kind: IconKind::Terminal, size: 12 } }
                span { class: "terminal-title", "{title}" }
                if let Some(exited) = exited {
                    span { class: "terminal-exited", "{exited}" }
                }
            }
            if pinned {
                button {
                    class: "file-tab-close file-tab-pin",
                    aria_label: "Unpin {title}",
                    title: "Unpin",
                    onclick: move |_| {
                        let key = key.clone();
                        cockpit.update(|state| docks::toggle_pin(state, agent, &key));
                    },
                    Icon { kind: IconKind::Pin, size: 12 }
                }
            } else {
                button {
                    class: "file-tab-close",
                    aria_label: "Close this terminal",
                    title: "Close this terminal: ends its shell",
                    onclick: move |_| cockpit.change(|state| terminals::close(state, agent, terminal)),
                    Icon { kind: IconKind::Close, size: 12 }
                }
            }
        }
    }
}

/// A dock's "+" and the button beside it that picks the shell: the new terminal opens in `dock`.
#[component]
pub fn NewTerminal(agent: AgentId, dock: Dock) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(entry) = state.agents.get(&agent) else {
        return rsx! {};
    };
    let terminals = &entry.terminals;
    let opening = terminals.opening;
    let offers_shells = terminals.offers_shells();
    let add_title = terminals.default_shell().map_or_else(
        || "New terminal".to_owned(),
        |shell| format!("New {} terminal", shell.name),
    );
    let shell_menu_open = matches!(
        state.ui.popover,
        Some(Popover::ShellMenu { agent: shown, dock: from, .. }) if shown == agent && from == dock
    );
    drop(state);
    rsx! {
        div { class: "terminal-new",
            button {
                class: "terminal-add",
                aria_label: "New terminal",
                title: "{add_title}",
                disabled: opening,
                onclick: move |_| cockpit.change(|state| terminals::open(state, agent, None, dock)),
                Icon { kind: IconKind::Plus, size: 12 }
            }
            ShellButton { agent, dock, offers: offers_shells, open: shell_menu_open, disabled: opening }
        }
    }
}

/// Where `dock` shows `terminal`: the page's script puts the terminal's box in it. A dock that
/// leaves gives the terminal to the one that opens.
#[component]
pub fn TerminalScreen(
    agent: AgentId,
    terminal: Option<TerminalId>,
    dock: Dock,
    #[props(default)] leaving: bool,
) -> Element {
    let id = match dock {
        Dock::Bottom => SCREEN_ID,
        Dock::Side => SIDE_SCREEN_ID,
    };
    let shown = terminal.map_or(0, TerminalId::get);
    rsx! {
        div {
            id: (!leaving).then_some(id),
            class: "terminal-screen",
            // The page's script leaves the keys typed here to the terminal.
            "data-records-keys": "",
            "data-agent": "{agent.get()}",
            "data-terminal": "{shown}",
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
            screen_ids: [SCREEN_ID, SIDE_SCREEN_ID],
            settle_ms: FIT_SETTLE_MS,
            min: 120,
            max: 2000,
            scrollback_bytes: 10_000_000,
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
                sizes: vec![TerminalSizeAt {
                    offset: 0,
                    size: TerminalSize { cols: 80, rows: 24 },
                }],
                fresh: true,
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
                action: PageAction::Clear,
            },
            ToPage::Scrollback { bytes: 25_000_000 },
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
            json!({
                "screen_ids": ["terminal-screen", "terminal-screen-side"], "settle_ms": 180,
                "min": 120, "max": 2000,
                "scrollback_bytes": 10_000_000,
            })
        );
        assert_eq!(
            serde_json::to_value(to_page).unwrap(),
            json!([
                { "type": "output", "agent": 3, "terminal": 2, "offset": 4, "data": "bHMK" },
                {
                    "type": "screen", "agent": 3, "terminal": 2, "offset": 0, "data": "bHMK",
                    "sizes": [{ "offset": 0, "size": { "cols": 80, "rows": 24 } }],
                    "fresh": true,
                },
                { "type": "live", "terminals": [{ "agent": 3, "terminal": 2 }] },
                { "type": "links", "id": 5, "links": [{ "start": 0, "end": 6, "path": "a.rs", "line": 2 }] },
                { "type": "act", "agent": 3, "terminal": 2, "action": "clear" },
                { "type": "scrollback", "bytes": 25_000_000 },
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

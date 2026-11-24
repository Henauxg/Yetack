# ghostty-web

The terminal emulator of the terminal pane: [coder/ghostty-web](https://github.com/coder/ghostty-web) 0.4.0, MIT (see LICENSE).

`ghostty-web.js` is the package's `dist/ghostty-web.js`, unchanged. It holds Ghostty's parser as an inline WebAssembly module, so it needs no other file. The npm tarball's sha512 integrity was `0puDBik2qapbD/QQBW9o5ZHfXnZBqZWx/ctBiVtKZ6ZLds4NYb+wZuw1cRLXZk9zYovIQ908z3rvFhexAvc5Hg==`.

The pane's script uses these private members:

- `Terminal.startRenderLoop()` and `Terminal.animationFrameId`, to stop drawing the terminals not on screen.
- `Terminal.linkDetector.providers`, to open the library's own web links in the browser: their `activate` calls `window.open`, which the window ignores.
- `Terminal.selectionManager`: its `copyToClipboard`, `selectionStart` and `selectionEnd`, so that a click, which selects the cell under it, copies nothing.
- `Terminal.renderer.renderScrollbar()` and `Terminal.handleMouseDown`, which the pane's own scrollbar replaces: ghostty-web paints its scrollbar on the canvas, over the last column, and its click handler on the terminal's box takes the clicks there. The pane's scrollbar reads `getScrollbackLength()`, `getViewportY()` and `rows`, and moves the view with `scrollToLine()`, which counts lines up from the end.
- `Terminal.wasmTerm.readResponse()`, which returns `null` once no answer is left, to send or drop the answers to a replay's queries past the 256 bytes of them that a write emits. The replay also counts on `write()` emitting its answers through `onData` before it returns.
- `Terminal.canvas`, `renderer.charWidth`, `renderer.charHeight`, `wasmTerm.getScrollbackLength()`, `getViewportY()`, `linkDetector.getLinkAt()` and `linkDetector.scannedRows`, to find the link under a right-click as ghostty-web finds the one a click opens.

Check them when updating the library.

Each terminal has an instance of its own from `Ghostty.load()`, passed as the `ghostty` option, rather than the one instance `init()` loads for all. ghostty-web 0.4.0 gives a new terminal a closed one's memory without zeroing it, and the closed one's text shows in it ([coder/ghostty-web#141](https://github.com/coder/ghostty-web/issues/141)). Once a release fixes that, the terminals can share one instance again.

The `scrollback` option is a count of bytes for Ghostty, though ghostty-web documents it as lines ([coder/ghostty-web#140](https://github.com/coder/ghostty-web/issues/140)). The script passes 10 MB, Ghostty's own default. A release that turns lines into bytes itself needs the script's value changed. On a shared instance, a history past about 1 MB can also crash a terminal made after one of another width ([coder/ghostty-web#210](https://github.com/coder/ghostty-web/issues/210)), which the instance of each terminal avoids.

`Terminal.open()` puts five listeners on the document, and `dispose()` removes three. The terminal's `mouseup` stays, since `dispose()` clears `isOpen` before it checks it, and so does the selection's `mousedown`, an anonymous function. Each keeps the closed terminal alive, and with it its instance's memory. The script records the listeners `open()` adds, and removes them after `dispose()`.

`Terminal.open()` also calls `focus()`, which focuses the terminal again on a timer. The script makes `focus()` do nothing while `open()` runs, and focuses the terminal itself, except one that starts again while the user types elsewhere.

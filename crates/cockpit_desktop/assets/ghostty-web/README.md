# ghostty-web

The terminal emulator of the terminal pane: [coder/ghostty-web](https://github.com/coder/ghostty-web) 0.4.0, MIT (see LICENSE).

`ghostty-web.js` is the package's `dist/ghostty-web.js`, unchanged. It holds Ghostty's parser as an inline WebAssembly module, so it needs no other file. The npm tarball's sha512 integrity was `0puDBik2qapbD/QQBW9o5ZHfXnZBqZWx/ctBiVtKZ6ZLds4NYb+wZuw1cRLXZk9zYovIQ908z3rvFhexAvc5Hg==`.

The pane's script uses these private members:

- `Terminal.startRenderLoop()` and `Terminal.animationFrameId`, to stop drawing the terminals not on screen.
- `Terminal.linkDetector.providers`, to open the library's own web links in the browser: their `activate` calls `window.open`, which the window ignores.
- `Terminal.selectionManager`: its `copyToClipboard`, `selectionStart` and `selectionEnd`, so that a click, which selects the cell under it, copies nothing.

Check them when updating the library.

# Ghostty's terminal core

**Decided 2026-10-24: the human wants to move the terminal grid from
`alacritty_terminal` to libghostty, once a trial shows it works.** Until
the trial passes, nothing is switched.

## Why

libghostty-vt is Ghostty's terminal emulation as a library, depending on
nothing but libc, with a C API
(https://mitchellh.com/writing/libghostty-is-coming, 2025-09-22). It is
the parser and screen state only; Horadric keeps its own Direct2D
renderer, ConPTY and session hosts. What it would bring:

- The Kitty keyboard protocol in full, closing the gaps listed under
  "Keys" in the loose ends.
- Kitty graphics, OSC 133 prompt marks, OSC 8 links and synchronized
  output, handled and kept up to date by people whose project it is.
- A core under heavy use, so the odd sequences agents' TUIs send are
  more likely to be handled right.

## What stands in the way

- **No tagged release yet.** The C API landed in 2026 commits.
- **Windows.** libghostty builds for Windows, but the Ghostty team says
  Windows work may start late 2026 and promises nothing
  (https://github.com/ghostty-org/ghostty/discussions/12290,
  2026-04-15). Horadric needs only the VT core, which matters less here
  than for the app, but the trial has to show it.
- **Zig.** Building it needs a Zig toolchain beside cargo, on this
  machine, CI and the macOS build. Or a prebuilt static library,
  vendored like `conpty.dll`.
- **A dependency decision.** It replaces one listed in CLAUDE.md, so
  CLAUDE.md changes with it.

## The trial

1. Build libghostty-vt for Windows x64 and link it from a small Rust
   test behind a feature flag, not in the app.
2. Feed it recorded output from real sessions (Claude Code, Codex, a
   `cargo build`, `vim`) and compare its screen with alacritty_terminal's
   cell by cell.
3. Measure parsing throughput on a large dump against alacritty_terminal.
4. Map what the renderer reads from the grid today (cells, colours,
   cursor, scrollback, selection, search over 10,000 lines) onto the C
   API and list what is missing.
5. Only then: put it behind a setting in a dev instance, as a second grid
   the reader thread can fill, and use it for a while.

The trial's result goes in the lore. If it fails, note why and what
would have to change upstream, and recheck at Ghostty's next release.

Touches: `crates/horadric-ui/src/console.rs` and the nine other files
that use `alacritty_terminal`, and the macOS port.

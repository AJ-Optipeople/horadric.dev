# Step 3: the terminal window

Clicking a tile expands it into a real terminal running the real `claude`.
Verified end to end against Claude Code 2.1 with Haiku: the trust dialog,
arrow keys, a prompt and its answer, resize, collapse and expand, `/exit`.

- **Starting a session.** `horadric new [--name] [--cwd] [-- claude args]`
  posts to `/horadric/new` on the running app, which starts the agent in a
  console and opens its terminal. `horadric run` is unchanged: a tagged
  `claude` in your own terminal, whose tile can not expand because Horadric
  does not own it. The friendlier ways in are under Launchers below.
- **`/horadric/new` is guarded.** A web page can reach localhost, so starting a
  process needs an `X-Horadric-Command: new` header, which a browser can not
  send cross origin without a preflight we never answer, and any request
  with an `Origin` header is refused.
- **ConPTY on the `windows` crate directly**, in `horadric-pty`, not
  `portable-pty`. Five calls did not justify its crates. The traps, both
  handled: the output pipe never reaches end of file on its own, so the
  waiter closes the console after the exit; and a parent with redirected
  stdio leaks those handles into the child unless `STARTF_USESTDHANDLES` is
  set with invalid handles. Resize is `ResizePseudoConsole`, not
  `SetConsoleScreenBufferSize`.
- **`alacritty_terminal` 0.26** with default features off. Only `Term` and
  the vte `Processor` are used, fed by our own reader thread; its event loop
  and tty module are compiled but unused. A synchronized update the program
  never ends is flushed when its 150 ms run out, from the paint path.
- **Glyph runs with forced advances.** Every glyph in a run gets the cell
  width as its advance, so text sits on the grid whatever the font says.
  Cell sizes are snapped to device pixels or neighbouring backgrounds leave
  seams. Characters the font lacks (Claude Code's `⏺`, `⎿`, `✻`), wide
  characters and combining sequences are drawn one at a time with
  DirectWrite fallback, pinned to their cells. Colour fonts only for wide
  characters, so a one cell symbol keeps the colour the program gave it.
  Cascadia Mono, falling back to Consolas, 14 DIPs. "Terminal font" in
  the tray menu picks any installed monospaced family instead, for every
  pane at once, kept in state.json as picked so a family that is
  uninstalled for a while comes back. The cursor blinks at the Windows
  caret rate in the pane with the keyboard, unless caret blinking is off
  in Settings or the program asked for a steady cursor. It is lit while it
  moves and stays lit 15 seconds after it last did, so an idle stage does
  not repaint.
- **Keyboard.** Characters come from `WM_CHAR` after the layout has done its
  work, so dead keys and AltGr on a Danish keyboard need nothing special.
  Keys without characters come from `WM_KEYDOWN` in xterm encoding.
  Shift+Enter sends Meta+Enter, Claude Code's newline. Ctrl+C copies when there is
  a selection. Ctrl+V pastes text with bracketed paste and escape characters
  stripped. Alt+F4 still closes. An input method composes at the cursor
  in the terminal's font, placed through IMM32 on each paint the cursor
  moved, with its candidate list kept off the cursor's cell. A hidden
  cursor counts, since agents hide it and park it where the user types.
  Checked on screen by reading the placement back against a screenshot;
  no IME is installed here, so a real composition is not seen yet.
- **Kitty keyboard protocol.** A program that pushes flags with `CSI > n u`
  gets keys in the kitty encoding. alacritty_terminal already parses the
  push, pop, set and query and keeps the flags in `TermMode`, once its
  `kitty_keyboard` config is on; `keys.rs` does the encoding. All five
  flags are honoured: disambiguate (Esc, Ctrl and Alt chords and
  Shift+Enter become `CSI code ; mods u`, plain Enter, Tab and Backspace
  stay as they were), event types (repeats from bit 30 of the key
  message, releases from `WM_KEYUP`), alternate keys, all keys as escape
  codes, and associated text. The key's code comes from `MapVirtualKey`
  and the character Windows made of the press is read off the queue
  before it is dropped, which is how AltGr typing stays typing. Paste,
  copy and Ctrl+Shift+T stay the stage's; Ctrl+C with nothing selected is
  `CSI 99 ; 5 u`. A release is only sent for a key whose press was, so the
  stage's own chords never leak one. F3 is `CSI 13 ~`, as kitty has it.
  Not done: the modifier keys themselves under "all keys", the lock
  modifiers, and the keypad's own codes. Checked on screen with a script
  that pushed flag 3 and logged its raw input, and with Claude Code,
  where Shift+Enter and Ctrl+Enter still make a new line.
- **Images.** Passing Ctrl+V on for Claude Code to read the clipboard did
  not work, so Horadric does it: a clipboard image is saved as a PNG in
  `%TEMP%\Horadric` and its path is pasted, which Claude Code turns into an
  attachment. The clipboard's own PNG is used when a browser or the Snipping
  Tool offers one, otherwise the Windows Imaging Component encodes the
  bitmap. Text beats an image, because Excel puts a picture beside copied
  cells, unless the text is only the image's URL. Files copied in Explorer
  paste as their paths. Files dropped on the window paste as their paths,
  quoted when they hold a space, as in Windows Terminal. Pasted images are
  deleted after a day.
- **Mouse.** Drag selects and copies on release, double click selects a
  word, right click copies a selection or pastes. Dropped files paste as
  paths. The wheel scrolls history, or sends arrows to a full screen
  program. A program that asks for the mouse (modes 1000, 1002 and 1003,
  encoded as 1006, 1005 or the old bytes) gets clicks, releases, the
  wheel and moves instead; Shift keeps a click for selecting, as in
  xterm.
- **Links.** Ctrl+click opens what is under the mouse, and while Ctrl is
  held that link is underlined and the cursor is a hand (`links.rs` finds
  it, pure; `watch::open_link` opens it). A web address goes to the
  browser, a folder to Explorer, a text file to VS Code with `-g` at the
  line the text gave (`a.rs:12:5`, `a.rs(12,5)`), in the window that has
  the project, anything else to what Windows opens it with, so a program
  runs. A path counts only when it is on disk, relative ones against the
  folder the session started in, which the console keeps for that. Quoted
  text is tried whole first, for paths with spaces, and a line wrapped by
  the terminal is read as one. Links a program made with OSC 8 are always
  underlined and win over the text, but open only web pages and files
  that are not programs: their text can say anything, and a click should
  not run what it hid. Claude Code wraps long lines itself, with real
  line breaks, so an address it splits over two rows is not rejoined.
- **Lifetime.** Closing the window collapses: window and renderer go, the
  console and its grid stay, and the window comes back where it was. A
  clean exit closes the window; a failed one keeps it open so the error can
  be read. An exit Claude Code could not report (a crash, a kill) becomes a
  `SessionEnd` of our own.
- **Environment.** The child gets `HORADRIC_SESSION`, `HORADRIC_OWNER_PORT` and `COLORTERM`, and
  loses the variables Claude Code sets to name a parent session. When Horadric
  was started from inside Claude Code those made the tile's agent believe it
  was nested, and it stopped saving its transcript.
- `HORADRIC_AGENT` runs something other than `claude.exe` in a terminal.
  `HORADRIC_AGENT=cmd.exe` is how to test the terminal without spending
  tokens.

Measured: one process, 51 MB (debug build) with a cluster and one open
terminal. History is 10,000 rows per session (see Quality of life), at
about 24 bytes a cell: under 30 MB per session when full at 120 columns,
1.1 GB for forty full ones.

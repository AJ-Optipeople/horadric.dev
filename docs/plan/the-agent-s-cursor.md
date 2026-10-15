# The agent's cursor

Asked for on 2026-10-01. Proposed, not built. When an agent tests a dev
instance it clicks with the human's own mouse: a PowerShell script calls
`SetCursorPos` and `SendInput`, the pointer jumps across the screen, and
the human has to keep their hands off until it is done. A human who
moves mid script sends the click somewhere else, which is why every
scripted click today first checks the window under the point is the dev
build's. The ask: the agent gets a cursor of its own that it uses as it
does today, the human keeps theirs, and the agent's is drawn on screen
whenever it is in use. It does not have to be a real pointer, only as
usable as one.

**Why it is faked.** Windows has one pointer per desktop. A second mouse,
real or a virtual driver, moves the same one. So the agent's cursor is a
position Horadric keeps, a picture of an arrow drawn there, and mouse
messages sent to the window under it. No `SendInput`, no `SetCursorPos`:
the human's pointer never moves and their clicks keep going where they
point.

**Why that works fully for Horadric's own windows.** A posted
`WM_LBUTTONDOWN` carries its point in `lParam`, and most handlers read it
from there. Not all: a drag reads the screen point with `GetCursorPos`
(a cluster moved, a tile lifted into the cube, the stash, the usage
window's slider, a terminal selection), hover asks `GetCursorPos` after
the fact, `SetCapture` only follows the real mouse, and the browser pane
asks `GetAsyncKeyState` whether a button is down. About forty such reads
in thirteen files of `horadric-ui`. Faked messages alone would click but
not drag. Since the code is ours, those reads go through one place that
knows about the agent's cursor, and then the fake is as good as the real
thing. Someone else's app reads the real pointer and cannot be taught,
so other apps get less (step 5).

**The shape.**

- **`pointer.rs` in `horadric-ui`.** The one place that answers where the
  mouse is and what is held: `pointer::at()` for `GetCursorPos`,
  `pointer::held(vk)` for `GetAsyncKeyState` and `GetKeyState`. With no
  agent gesture under way they pass straight through to Windows. During
  one they answer the agent's point and the agent's buttons and keys.
  Every read in the UI moves to them, and clippy's
  `disallowed_methods` keeps a new `GetCursorPos` from creeping back.
- **A gesture is one call.** Move, press, drag, release, scroll, a key or
  some text. It runs on the UI thread, between messages, so it never
  interleaves with a real one. It finds the Horadric window at the point
  from Horadric's own windows in z order (not `WindowFromPoint`, which
  would find whatever the human has on top), sends it `WM_MOUSEMOVE`,
  the button messages and `WM_MOUSEWHEEL` with the point in `lParam`,
  and keeps an emulated capture: after a press, the moves and the
  release go to the window pressed, as `SetCapture` would make them.
  Leaving a window sends it `WM_MOUSELEAVE`, so hover clears. Text is
  `WM_KEYDOWN`, `WM_CHAR` and `WM_KEYUP` to the window that has the
  keyboard in Horadric, without `SetForegroundWindow`, so the human's
  focus stays in their own app.
- **The human wins a tie.** While a gesture runs, real mouse messages to
  the window it acts on are held back and replayed after it, so a
  human's twitch does not break the agent's drag. A gesture is
  milliseconds long, so nobody feels the wait. After it the agent's
  hover stays until the human's pointer comes back to that window.
- **The arrow, `ghost.rs`.** A layered window, topmost, click through
  (`WS_EX_TRANSPARENT`), never activated and not on the taskbar, drawn
  with Direct2D like the rest: an arrow in a colour no Horadric state
  uses, with the session's name on a small tag beside it. It glides to
  each new point over about 150 ms, so the human can follow it, rings
  out on a press, draws a line while dragging, and fades three seconds
  after the last gesture. Several sessions acting at once each get
  their own arrow and tag. `SetWindowDisplayAffinity` with
  `WDA_EXCLUDEFROMCAPTURE` keeps it out of screenshots, so the arrow
  never covers what the agent is trying to read, while the human still
  sees it.
- **How an agent drives it.** Tools on the MCP server every session
  already gets (`mcp.rs`), beside the browser ones: `desktop_click`,
  `desktop_drag`, `desktop_scroll`, `desktop_type`, `desktop_press` and
  `desktop_screenshot`. Points are screen pixels, the same as a
  screenshot's. Key names reuse the browser tools' `key_events`
  parsing. The screenshot is `PrintWindow` of Horadric's windows
  composed in place, so it shows them even when the human's windows are
  on top, and replaces the `CopyFromScreen` scripts. For a script or a
  shell, `horadric pointer click 1820 64` and friends do the same.
  Both go to the listener at a new `/horadric/pointer`, with the
  session's header, so the tag knows whose arrow it is.
- **Dev instances only, at first.** The installed Horadric refuses
  pointer calls; the MCP tools go to the dev instance's port. An agent
  testing a build cannot click the human's real tiles by a wrong
  coordinate, and a point that lands outside the dev build's windows is
  refused with what is there instead. That refusal replaces the check
  each script does today.

**Steps**, each landing on its own:

1. `pointer.rs` with every cursor and button read moved to it, passing
   through. No behaviour change; the clippy rule turned on.
2. Gestures and `/horadric/pointer`, with `horadric pointer` on the
   command line. Verified on a dev instance with the human's pointer
   parked in another corner: a tile click switches the stage, a cluster
   dragged and dropped, a tile lifted into the cube, the usage slider
   dragged, a terminal selection, a list picked, text typed into a
   pane. After each, the real `GetCursorPos` is where it was.
3. The arrow: glide, press ring, drag line, fade, tag, kept out of
   captures. Checked by eye, and by a `CopyFromScreen` that must not
   show it.
4. The MCP tools and `desktop_screenshot`, and this file's "Verifying
   Windows code" and `CLAUDE.md` rewritten to use them instead of
   scripts.
5. Other apps, best effort: messages posted to the child window under
   the point and UI Automation's invoke for a named button, the arrow
   drawn the same. Clicks and typing work in most plain Win32 apps.
   Drags, and apps that read the real pointer (games, some Electron and
   DirectX apps), do not, and the tool says so instead of falling back
   to the real mouse.

Pure parts get tests: finding the window at a point from a z ordered
list, the emulated capture's routing, the glide's path, the held back
messages replayed in order.

**Open questions.**

- Whether the installed Horadric should ever take pointer calls, for an
  agent helping the human in their real tiles. Off until asked for.
- Whether step 5 is worth it, or Horadric's windows and the browser pane
  cover what agents actually click.

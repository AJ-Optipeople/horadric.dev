# Implementation plan

Where Horadric is, what comes next, and what was decided along the way. Update
this file when a step lands or a decision changes. It is the handover
document: someone picking the project up cold should need nothing else.

Last updated 2026-09-25, after step 3, the launchers, persistence, install,
the stage, reload, the project grid, browser windows, the look, plain
terminals, a pass of quality of life, the columns, the task list and
session hosts.

## Shape of the thing

Five crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `horadric-core` | Session, Phase, Registry, the state machine | any |
| `horadric-hooks` | The localhost listener, its client, the settings installer | any |
| `horadric-pty` | ConPTY pseudo consoles, the session host and its pipe protocol | Windows |
| `horadric-ui` | Cluster and terminal windows, layout, drawing | Windows |
| `horadric` | The command line, `horadricw` for Explorer, the wiring | Windows |

`horadric-core` and the pure halves of `horadric-ui` (`layout`, `columns`, `theme`,
`palette`, `keys`, `frame`, `files`, `viewer`, `highlight`, `shell`, `board`) have no
I/O and
are tested. `horadric-pty` has a
test that runs `cmd.exe` in a real pseudo console. Everything else is
verified on screen.

Threads: the listener thread owns the socket, a feeder thread applies events
to the registry, the UI thread owns every window and never blocks. Each
console adds two: a reader that parses what its session host sends into
the grid and notices the exit, and a writer that owns the pipe's sending
side so typing never blocks the UI. The host process has its own, see
"Sessions that outlive Horadric". Everything off the UI thread reaches it as a message to one
hidden message-only window, never a thread message: thread messages are
dropped while Windows runs a modal loop, and dragging a terminal's edge is
one. The registry is an `Arc<Mutex<Registry>>`, each console's grid an
`Arc<Console>` with the terminal behind a mutex.

## Done

### Step 1: hook receiver and state machine

Claude Code `http` hooks post every lifecycle event to
`127.0.0.1:43117/horadric/hook`. The hook config carries `X-Horadric-Session:
$HORADRIC_SESSION`, so an event arrives already tagged with the session it
belongs to. A `claude` started outside Horadric sends an empty header and the
event is dropped, which is what keeps Horadric from instrumenting the whole
machine. Superset got this wrong and wrote it up in their
`HOOKS_INVESTIGATION.md`; the guard is the lesson.

The state machine is in `horadric-core/src/session.rs`. Prompt and tool events
mean working, permission and notification events mean waiting, `Stop` means
done, `SessionEnd` means ended. Subagent events and compaction restarts
change nothing.

The tag alone is not enough. Claude Code's background sessions (`claude
--bg`, and the left arrow in a session, which opens its agent view) are
started by one shared `claude daemon`, which copies the environment of
the session that started it. Every background session after that posts
under that session's `HORADRIC_SESSION`, whoever asked for it. Seen with
a probe on 2026-09-26: a `--bg` started with its own tag arrived under
the tag of the Horadric session that had spawned the daemon. So a tile
keeps the Claude `session_id` it first hears and ignores events, status
lines included, that carry another. A `SessionEnd` from `/clear` or
`/resume` lets go of the id, so the next conversation can take the tile.
Those background sessions get tiles of their own, see Background
sessions. `horadric run` posts a `HoradricRegister` event of its own before
starting Claude, because Claude Code sends no hook until the first prompt and
a fresh session would otherwise be invisible.

### Step 2: cluster windows

One frameless window per project, standing in columns down the left edge
of the work area (see The columns), drawn with Direct2D and DirectWrite. The Win32 rules that make it feel
native rather than like an app window:

- `WS_EX_NOACTIVATE` so clicking a tile never takes focus from the editor.
- `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar.
- `SWP_NOACTIVATE` on every move, resize and raise.
- Dragging handled by hand, because the system move loop activates.
- A dragged cluster takes the place in the columns it is let go over.
  Placing it freely, with snapping and Shift, was dropped for the columns.
- DWM rounded corners and a suppressed border, so it matches Windows 11.
- Not topmost. It stacks like a normal window, so an editor can cover it.
  A click raises it by going topmost and straight back, since a plain
  `HWND_TOP` from a background process stays below the foreground window.
  "Bring tiles to front" in the tray menu raises them all. It used to be
  topmost and raised on every phase change, which put it over everything.

Measured: one process, about 45 MB with two clusters and five tiles, no CPU
between events.

### Step 3: the terminal window

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

### Launchers

Typing `horadric new --cwd` was too much friction for starting a session in a
new project, so three ways in that need no terminal:

- **Tray icon.** Always there, even with no tiles. Its menu has "New
  session..." (the folder picker, opening in the last project), up to eight
  recent projects one click away, and Quit, which warns when sessions in
  Horadric terminals would end. Recent projects live in
  `%APPDATA%\Horadric\recent.json`. The tooltip counts sessions and waiting
  ones. The icon is drawn in code (`icon.rs`), so there is no file to ship.
  Pulled forward from step 5.
- **The full width `+` below a cluster's last tile** starts another session
  in that project at once, no picker. It is outlined, not filled, so it
  reads as the slot where the next tile goes rather than as a session.
- **The `+` in a cluster header** is for a new project. It opens the folder
  picker in the folder that holds this project, since projects tend to sit
  side by side. It used to open in the project itself, which made another
  session there the easy path; the bottom button is that path now.
- **Explorer.** `horadric explorer install` adds "Open in Horadric" when right
  clicking a folder or the empty space inside one, under
  `HKEY_CURRENT_USER`, so no admin rights. On Windows 11 it is under "Show
  more options"; the short menu only takes packaged apps. It runs
  `horadricw.exe`, a second, windowless binary, so a right click never flashes
  a console. `horadricw` starts the app when it is not running (hidden, no
  console window) and then asks it for the session. With no folder it only
  starts the app.

- **The start window** (`start.rs`). With no project open the desktop
  used to show only the usage window and a tray icon, and nothing said how
  to begin. Now a ghost cluster stands where the first cluster will go,
  below the usage window: "No project open", a dashed tile "Open a
  project" that opens the folder picker (beside the most recent project),
  and up to five recent projects that start a session with one click. A
  folder dropped on it from Explorer opens as the project, and a dropped
  file opens its folder. It exists only while there are no clusters, so it
  is gone the moment the first one appears. Tested on screen with a dev
  instance: the empty and the recent versions drawn, and a click on a
  recent project started one `cmd.exe`, replaced the ghost with its
  cluster and opened the stage. The picker and a drop not yet tried by
  hand.

Two Win32 details that decided the shape. The app's hidden window is now a
top level tool window, not message-only, because only top level windows
hear Explorer's `TaskbarCreated` broadcast, after which the tray icon must be
added again. And menus and dialogs run modal loops that dispatch our own
messages, so they run with the app state not borrowed, from the window
procedure itself.

### Persistence

Horadric picks up where it left off, after a quit, a crash or a restart. The
processes can not survive that, but the conversations can: Claude Code
resumes one with `claude --resume <id>`, and every hook carries the id.

- `%APPDATA%\Horadric\state.json` (`horadric_core::saved`) holds the sessions
  Horadric owned, with name, folder, original arguments, Claude's session id,
  whether a prompt was ever sent, the last line and the window position;
  the cluster positions; the recent projects; whether autostart was offered.
  Written from the one second tick when it changed, beside and renamed.
- On start every saved session is a **paused** tile, a new phase. Clicking
  one runs `claude --resume <id>` with its original arguments in the same
  folder and window. Nothing starts until clicked, so forty saved sessions
  cost nothing at boot. A session never prompted starts fresh, since Claude
  wrote no transcript to resume.
- A clean exit (`/exit`) ends a session for good. Any other exit, a crash or
  a kill, pauses it instead. Right click a tile for End session.
- Right click a project's header or its bottom plus for the whole project:
  New session, Start 4 sessions, Start over with 4 sessions (the new ones
  start before the old ones end, so the cluster keeps its place), and End
  all sessions. The tray has End all sessions for every project. Ending
  asks first when a session is running and says how many are mid turn.
- On `WM_QUERYENDSESSION` and on Quit the state is written once more and
  then frozen, so sessions dying on the way out are not saved as gone.
- Claude's session id is the latest non empty one from any hook. Horadric's
  own events carry none; the first version stored that empty id and never
  learned the real one.
- **The runaway, and the guard.** The first resume cleared the paused mark
  after launching, and launching opens the window through `expand`, which
  saw a paused tile and resumed again: 167 `claude` processes in a minute.
  Now the mark is cleared first, and `launch` refuses any session that
  already has a live console, whatever path led there.
- A second Horadric refuses to start when the port answers. Autostart plus a
  manual start would otherwise show every saved session twice.

### History

A session ended for good (`/exit`, End session, Start over) used to be
gone from Horadric, with no way back to its conversation. Asked for so a
closed session can be picked up where it was left.

- **Claude Code already keeps them.** Every conversation is a transcript in
  `~/.claude/projects/<folder>/<id>.jsonl` (under `CLAUDE_CONFIG_DIR` when
  set), the folder being the working directory with every character that
  is not a letter or digit made a hyphen. The drive letter comes in both
  cases, so both folders are read. Horadric keeps no history of its own:
  this one also has the conversations started outside Horadric.
- **History in the project menu** (right click a header or the bottom `+`)
  lists the newest ten, by the title Claude Code gave them and how long ago
  each was touched (`history::label`). Conversations a tile holds, live or
  paused, are left out, and so are ones with no title, since Claude Code
  writes one after the first prompt. Only the last 256 KB of each file is
  read and at most 40 files are looked at, since the menu is opening on
  the UI thread (`transcript::history`).
- **A click** starts `claude --resume <id>` as a new tile in the project,
  with the title and id set at once, so a pause before its next prompt
  still resumes that conversation. "All conversations..." at the bottom
  starts `claude --resume` with no id, and Claude Code's own picker lists
  the rest in the pane.
- **A project with no tiles left** has no cluster and so no project menu.
  Two more ways in: History in the tray menu, with a submenu for each
  recent project, and a right click on a recent project in the start
  window, which offers New session and that project's History. Menu ids
  are spans of `history::SPAN` per list (`history::pick`,
  `history::pick_nested`), so one tray menu holds every project's.
- `tray::Item::Submenu` is new for it.

Tested on screen with a dev instance and `cmd.exe`: the project menu's
submenu listed this project's ten newest conversations with titles and
ages, and a click opened a pane titled with the conversation, its process
given `--resume` and the id. With no project open, a right click on the
start window's recent project and a pick from it replaced the start window
with the project's cluster and the resumed tile. The tray's History showed
the project's submenu without the conversation just resumed, and a pick
there started the right one. A dev instance lists the installed one's live
sessions too, since its registry does not hold them.

### Install

`horadric install` makes Horadric a normal per user app, no admin rights:

- Copies `horadric.exe` and `horadricw.exe` to `%LOCALAPPDATA%\Programs\Horadric`.
- Adds that folder to the user `PATH` (registry, then `WM_SETTINGCHANGE`).
- A Start menu shortcut to `horadricw.exe`, so Windows search finds Horadric.
- Points Explorer's "Open in Horadric" and Start with Windows at the copy.
- Installs the Claude Code hooks, then starts Horadric.

`horadric uninstall` reverses it and leaves the saved state. The executables
carry the icon and version information, baked in by `crates/horadric/build.rs`,
which writes the `.res` file itself from the tray icon's drawing code, so
Explorer, Start, the taskbar and Task Manager all say Horadric.

`horadric` with no arguments now starts the app hidden and returns. Running
the app inside a terminal meant closing the terminal ended every session.
`horadric app` is the old behaviour, for the log.

### Reload

Shipping a build used to be four steps from a terminal outside Horadric and a
click on every tile. Now it is `target\release\horadric.exe reload`, run by
the agent when the human says ship.

- **The request.** `reload` posts `/horadric/reload` with the path of the
  binary it was run from, guarded like `/horadric/new` (its own
  `X-Horadric-Command` value, no `Origin`). `--now` skips the wait.
- **The wait.** The app hands over once no session in a Horadric terminal is
  mid turn (`Phase::mid_turn`, only `Working`). A session waiting on you
  is not mid turn; it resumes to the same question. The agent that ran
  `reload` finishes its turn first, which is why the command returns at
  once instead of waiting. The tray tooltip says a reload is pending.
- **The handover.** The app saves with `running` marked on every session
  whose process is alive and `on_stage` naming the session with the
  keyboard on the stage,
  starts `<new build> swap --pid <its pid>` with no window, and quits.
- **`swap`** waits for that process to exit, moves each installed binary
  aside as `horadric.old.exe` and `horadricw.old.exe` (a running binary can be
  renamed, not overwritten), copies the build in, rewrites the hooks as
  `install` would, and starts `horadric.exe app --reload`. It counts as up
  when the port answers and the process is still alive 3 seconds later,
  since the app listens before it builds its windows.
- **Rollback.** A build that is not up within 20 seconds is killed, the
  moved binaries go back, and the old build is started the same way. Only
  what was moved goes back, so a copy failing half way restores exactly
  that. `%APPDATA%\Horadric\reload.log` has the last reload.
- **`app --reload`** resumes the sessions saved as `running`, once each,
  without opening a window for each, then puts the stage back.
- **After a crash** the same happens without `--reload`. Every save writes
  `live: true`; only tray Quit writes it false, so a start that finds it
  true follows a crash, a kill or a logoff, and resumes the sessions saved
  as `running`. A start after Quit still leaves every session paused. The
  fuses: once each, never one already heard from since the start (its agent
  outlived the old Horadric), and never twice in a row. A start that
  resumes after a crash writes `recovering: true` for its first 60 seconds,
  and a start that finds both resumes nothing. Tested with a dev instance
  and two `cmd.exe` sessions: killed, both came back, one process each;
  killed again within the minute, both stayed paused.
- **A dev instance** restarts from its own build and copies nothing, which
  is how reloading itself gets tested.

Tested with a dev instance and `cmd.exe` sessions: two running sessions
came back as two, in about three seconds, with the stage showing the same
one; a faked working session held the reload until its `Stop`. The install
path and the rollback were tested against a fake install folder with its
own `APPDATA`, `LOCALAPPDATA`, home and port, the rollback by squatting the
port so the new build could not listen.

The installed Horadric has to know `/horadric/reload`, so the first build with
it is installed by hand. Since session hosts (below) the sessions no longer
end with the process: the new build attaches to them, and the wait for
idle sessions is gone. What this section says about resuming still holds
for a session whose host is gone.

### Dev instances

Horadric is developed from a session inside the installed Horadric, so a test
build must never disturb the one hosting the agent. `HORADRIC_DEV=1` makes
a dev instance:

- Port 43118 unless `HORADRIC_PORT` says otherwise, so both can listen.
- State in `%APPDATA%\Horadric-dev`. Sharing `state.json` would load the real
  sessions as paused tiles, and a click would resume a conversation that is
  already live in the installed Horadric.
- No autostart offer and no switch for it in the tray. The first dev run
  used to point the `Run` key at `target\debug\horadricw.exe`.
- A red tray icon and "Horadric dev" in the tooltip.
- `install`, `uninstall` and the hook and Explorer installers refuse to run.

The hook URL is fixed at install time, so a `claude` in a dev terminal
still posts to the installed Horadric. Every Horadric-started `claude` now gets
`HORADRIC_OWNER_PORT`, the hook sends it as `X-Horadric-Port`, and a listener
that is not the owner passes the event on to the one that is. Without the
header the installed Horadric would adopt dev sessions as ghost tiles.

Tested on screen: a dev instance beside no installed one, a `cmd.exe` tile,
state written to `Horadric-dev` only. The hand off between two running
instances is covered by a listener test, not yet seen with a real `claude`.

### The stage

Ten sessions open used to mean ten terminal windows piled on each other.
Now they share one, the stage, which shows one project at a time.

The first version showed one session and swapped it on a click, with a
"pop out" for sessions wanted side by side. Replaced, see The project
grid below: switching sessions one at a time hid the rest of a project,
and popped windows were the freedom that made the desktop a pile again.

- **The project on stage is lit**: its cluster has an edge in the project's
  colour, the same colour as the stage's border (see The look).
- **Where the stage goes.** A new session docks it against the tiles as a
  square filling top to bottom: its top and bottom at the margin, its edge
  a gap from the box around every cluster on the primary screen, on the
  side with more room, narrower only when that side is (`layout::beside`,
  `layout::square`). The first time, it opens the same way. A resume, a
  tile click or the hotkey leaves it wherever it was left (`stage` in
  `state.json`).
- **Next waiting session.** Ctrl+Alt+Space, from anywhere, shows the session
  that has waited on you longest. Pressed again while that one is in front,
  it moves on to the next (`inbox::next`), so one you are not ready for does
  not block the rest. A dev instance uses Ctrl+Alt+Shift+Space so both can
  hold one. Also in the tray menu, with the shortcut beside it.
- **Fit terminal beside tiles** in the tray menu fills that same space
  beside the clusters with the stage, by its visible edges: a Windows 11 window
  has an invisible resize border that would otherwise double every gap.
- **Terminals snap** to the work area edges and the other Horadric
  windows, with the margin and gap the columns use: when moved
  (`WM_MOVING`, `layout::snap`) and when an edge is dragged (`WM_SIZING`,
  `layout::snap_edges`). Both measure visible edges (`snapping::others`),
  and Shift places it freely. The system loops build each rect from the one returned
  last, so a snapped rect snapped again on every small step and the window
  never let go. The free rect comes from the cursor instead, at the
  distance from each edge it had when the drag began.
- The windows to snap to are found by class name, so a dev instance also
  snaps to the installed Horadric's windows. Harmless, and useful: that is
  where they are on screen.

Found while testing it: a click on a tile made the cluster the foreground
window, `WS_EX_NOACTIVATE` or not. So "is this terminal in front?" was
always no at click time, and the second click that should collapse a
terminal only brought it forward, before the stage too. The cluster now
answers `WM_MOUSEACTIVATE` with `MA_NOACTIVATE` itself.

A test trap, not a bug: an app started with `Start-Process -WindowStyle
Hidden` has its first `ShowWindow(SW_SHOWNORMAL)` turned into a hide by
Windows, so the first terminal never appears. Start dev builds with
`-NoNewWindow` instead. `horadricw` and the `Run` key are not affected.

### The project grid

The stage shows every session of one project at once, as panes in a grid
(`layout::grid`): one fills the window, two sit side by side, three are two
above one wide, four are two by two. Asked for so a project reads as one
piece of work, with less freedom and more structure.

- **A pane is a child window** (`pane.rs`) with its own render target,
  keyboard and mouse, the old terminal window's insides. The stage
  (`terminal.rs`) is the frame: title, snapping, layout, and the drag.
  Child windows rather than one window drawing every grid, because focus,
  capture, dropped files and the caret then belong to the right session by
  themselves.
- **A tile click switches the project** and gives that session the
  keyboard. A click on the tile of the session typing, with the stage in
  front, collapses it. The hotkey for the next waiting session does the
  same switch.
- **Which sessions are panes**: every console of the project except a
  clean exit. A crash keeps its pane, showing what went wrong, until the
  tile resumes it into the same place. Paused sessions have no pane.
- **The stage follows the registry.** `reconcile` ends in `sync_stage`, so
  a session started in the project on stage joins it, an ended one leaves,
  and an empty stage closes. `TerminalWindow::show` keeps the panes it
  already has, with their selection and scroll, and lays out again only
  when the set changed.
- **Headers.** With more than one pane, each has a header: the session
  name and what the agent says it is doing, with a line of the phase's
  colour along its top. No dot before the name: every app has one, and
  the line already says it. The one with the keyboard is underlined in
  the project's colour and the others step back. One pane alone has no
  header; the title bar says it all.
- **Drag to swap.** Pressing a header gives the stage the mouse
  (`WM_PANE_GRAB`, then `SetCapture` on the stage). Past 4 pixels the pane
  lifts, blue, and the pane under the cursor is outlined; letting go swaps
  the two. The swap goes through the app (`Input::Swap`), which owns the
  order.
- **The order is kept** per project in `grids` in `state.json`. A paused
  session keeps its place, so a restart puts each back where it was as it
  resumes (`layout::grid_order`). A reload brought a swapped grid back
  swapped.
- **Tiles and panes share one order.** It lives in `Shared::orders`, so
  the cluster sorts its tiles by it (`layout::rank`) and the stage lays
  out its panes by it. Every session gets a place as the registry
  changes, new ones last in the order they started, so a tile and its
  pane agree from the first. A pane swap moves the two tiles too.
- **Drag a tile to move it.** In a cluster of more than one tile, pressing
  a tile and moving past 4 pixels lifts it: it follows the cursor up and
  down, solid and outlined blue, and the others slide out of its way.
  Letting go puts it in the place it is over (`layout::tile_slot`) and
  the stage's grid follows (`Input::Reorder`). A lone tile still drags its
  window, and the header drags any cluster. Asked for so the tiles read
  in the same order as the panes, and either can be arranged.

Tested on screen with a dev instance and three `cmd.exe` sessions: tiles
and panes started in the same order; dragging the bottom tile to the top
moved its pane to the first place; the order came back after a restart
with every session paused; a plain click on a tile still resumed it; a
pane swap swapped the two tiles.
- **Pop out is gone**, with `popped` and `placement` in `state.json`. An
  older file still reads; those fields are ignored.
- The tray's "Arrange terminals" became "Fit terminal beside tiles".

Tested on screen with a dev instance and `cmd.exe` sessions: three in one
project laid out two above one; typing reached the pane clicked; `exit` in
one left the other two side by side with the keyboard moved over; a tile in
another project switched the stage, and a second click collapsed it;
Alt+F4 inside a pane closed the stage; the drag swapped two panes and the
reload kept them swapped.

### The files tile

Every cluster whose project is in git ends in a files tile: the project's
tree as VS Code's explorer shows it, with the git changes coloured in.
Asked for so the changes in a project can be seen at a glance, without an
editor.

- **What it shows.** Every file git knows (tracked and untracked, ignored
  ones hidden) plus deleted ones, folders first, names sorted without
  regard to case. Names take VS Code's dark theme colours and letters: M
  modified, A added, U untracked, D deleted, R renamed, C conflict. A
  folder takes the most important change inside it and a dot. A chain of
  folders with one child each shares a row (`crates/horadric-ui/src`). The
  header counts changed files.
- **Folders.** One holding a change opens by itself, so a new change is in
  view without a click. A click opens or closes any folder and is
  remembered by path, so it survives the tree being rebuilt.
- **Files.** A click shows the file on the stage (see The file viewer).
  Ctrl+click opens it in VS Code instead, in the window with the project
  (`Code.exe <project> <file>`, found beside `bin\code.cmd` on `PATH`, so
  no console flashes). Without VS Code, whatever Windows opens it with.
- **Size.** Its column decides (see The columns): the files tiles in a
  column share the height the rest leaves, and a longer tree scrolls with
  the wheel, with a thin bar. Room left under the last row stays empty.
  The header folds it to one row, kept in `state.json` as
  `files_collapsed` per cluster. It used to be 14 rows, or as tall as its
  bottom edge was dragged, which made every cluster a different height
  and let a change in one move the rest. It sits below the `+`, which
  stays the slot where the next session tile goes.
- **Where the data comes from** (`watch.rs`). One thread per project runs
  `git ls-files` and `git status` and hands the tree (`files.rs`, pure and
  tested) to the cluster with a window message. Then it sleeps in
  `ReadDirectoryChangesW` until a file changes: nothing polls, so a quiet
  project costs no CPU. Changes in ignored folders are dropped, from `git
  ls-files --ignored --directory`, or every `cargo build` would rerun git;
  inside `.git` only `index` and `HEAD` count. A burst is waited out (300
  ms quiet, at least 1 s between scans, at most 3 s of waiting).
- **A git folder elsewhere.** A session in a subfolder of a repository,
  or in a worktree whose `.git` is a file, has its index outside the
  project folder. The thread then also watches the top of `git rev-parse
  --absolute-git-dir`, not its subfolders, and names what changes there as
  if it were in `.git`, so the same rule applies and commits are heard.
- `git --no-optional-locks` is required: a plain `git status` may rewrite
  the index, which the watcher would see, and scan again forever.
- A folder outside git gets no tile and no thread.

Tested on screen with a dev instance on this repo: 43 changes shown
coloured, a new file at the root appeared as U within a second and went
when deleted, five writes to `target\` caused no scan, folding, the wheel
and the header fold worked, and a click opened `main.rs` in VS Code. A scan
takes about 100 ms in a debug build, most of it starting git. A session
started in a subfolder of a scratch repository showed a new file, rescanned
on a commit made at the repository root, and did not rescan on `git
status`, `git log` or `git gc`.

### The file viewer

A click on a file in the files tile shows it on the stage, read only, beside
the project's sessions: line numbers, VS Code's Dark+ colours, long lines
wrapped at a space and indented under their text. Asked for so a file an
agent is working on can be read without leaving Horadric. A viewer, not an
editor, on purpose.

- **A pane with no program.** A file view is a `Console` without a pseudo
  console, its grid fed escape sequences that paint the file
  (`viewer.rs`, pure and tested). The pane draws it like a session, so
  selection, the wheel, glyph fallback and wide characters cost nothing.
  Control characters in the file become their Unicode pictures, so a file
  can never send the terminal a command.
- **One per project.** A click on another file replaces it, like VS Code's
  preview tab, so browsing never piles up panes. It comes last in the
  grid and is never saved in the order. Closing the stage drops it.
- **Colours** (`highlight.rs`). `syntect` with the Sublime grammars it
  bundles, which are TextMate grammars like VS Code's, and Dark+ written
  out as scope rules, so no theme file ships. Pure Rust regexes
  (`regex-fancy`), no C build. Highlighting runs on a thread; the file shows
  plain first. The bundle has no TypeScript or TOML: `.ts` and `.tsx` borrow
  JavaScript, TOML stays plain.
- **Keys.** Arrows, Page Up and Down, Ctrl+Home and Ctrl+End scroll.
  Ctrl+C copies, and a drag copies on release, as in a session. Copying
  leaves out the line numbers and gives a wrapped line back whole
  (`viewer::copy` maps grid cells to file bytes). Esc or the cross at the
  end of its header closes it. Nothing typed reaches anything.
- **Live.** When the project's watcher rescans, the view reads its file
  again if its size or time changed, keeping the same line at the top. So
  an agent's edit shows within a second or so. A resize lays the file out
  again for the new width, also keeping the top line.
- **Limits.** Files over 4 MB are not read, binary files (a NUL in the
  first 8000 bytes) are not shown, and past 10,000 lines or 20,000 grid
  rows the rest is left out and counted. Each row is a full width of
  cells, so the rows limit is what bounds memory, about 60 MB at 120
  columns.
- **Changed lines.** Between a line's number and its text, a bar in VS
  Code's gutter colours says how it differs from the last commit: green
  for added, blue for changed, and a red underline on the line a removal
  follows. It comes from the hunk headers of `git diff -U0 HEAD` for the
  one file (`viewer::marks`, pure), run on the colouring thread. Every
  rescan asks again even when the file did not change, so a commit clears
  the marks. An untracked file, or one outside a repository, has none.
- **Search.** Ctrl+Shift+F opens the terminal's search bar, but a view
  searches its file, not its grid (`viewer::find` and `viewer::cells`), so
  a line number never matches and a match can cross a wrap. It starts at
  the top line on screen, and Enter and F3 go down the file, as in an
  editor, where a terminal goes up its history. Shift goes back, and both
  wrap round. Case is ignored unless the query has a capital.

Tested on screen with a dev instance on this repo: `main.rs` opened beside
a `cmd.exe` pane with the colours right, the wheel scrolled it, a click on
another file replaced it, an edit to the shown file appeared within a few
seconds, a drag across the gutter copied only the text, and Esc and the
cross both closed it with the keyboard back in the session. The changed
lines and the search were checked on a scratch repository with a line
changed, two removed and two added: each mark in its place, Enter stepping
to the next match and round to the first, "12" (only a line number there)
not found, and the marks gone a few seconds after a commit.

Not done yet: horizontal scrolling instead of wrapping.

### Browser windows

An agent testing a web page starts a browser of its own, through Playwright
or the Chrome DevTools MCP. Its window used to land anywhere, with nothing
saying which session opened it. Embedding a browser in Horadric was
considered and dropped: the agent drives its browser over a debug protocol
and never needs a window Horadric owns, and it would be the web view the
settled decisions rule out. So Horadric manages the browser's window instead.

- **Which session.** Every agent starts inside a job object of its own
  (`horadric-pty`, `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so it is in the job from
  its first instruction), and everything it starts joins the job. A window
  belongs to the session whose job holds its process (`IsProcessInJob`).
  The job limits nothing and closing it ends nothing.
- **Which windows** (`browsers.rs`). A WinEvent hook, out of context, hears
  every top level window shown and every tracked one destroyed, and posts
  them to the app window. Only main windows count (no owner, a title bar,
  not a tool window) and only browsers, by executable name: Chrome, Edge,
  Firefox, Brave, Chromium, Vivaldi, Opera and Playwright's WebKit. An editor
  or an app under test is left alone, and so is a dev Horadric started inside
  a session, whose windows are in that session's job too.
- **Where it goes.** When it first appears, against the stage in the space
  beside the tiles and the stage together (`layout::beside_stage`), its own
  size where that fits and shrunk where it does not. Narrower than 360 DIPs,
  or opened minimised or maximised, it stays where it opened. After that it
  is the user's.
- **It follows its project.** When the stage switches project, that
  project's browsers come back just below the stage in the stacking order,
  and every other project's are minimised. One the user minimised stays
  minimised.
- **The tile mark.** A tile whose session has a browser open shows a small
  window glyph at the end of its second line. It is a button: a click
  restores that session's browsers and gives them the foreground.
- **Never block.** Every call on a browser window is asynchronous
  (`SWP_ASYNCWINDOWPOS`, `ShowWindowAsync`). The window belongs to another
  process, and a hung browser would otherwise freeze the UI thread.
- A URL opened in the user's own browser (`start https://...`) goes to the
  browser that is already running, outside every job, so it is not caught.
- Shrinking a browser changes the page's layout when it has no fixed
  viewport. With a fixed one, the Playwright library's default, the window
  size changes only what you see, not what the agent tests. Not yet checked
  against the MCP servers' own defaults.

Tested on screen with a dev instance on its own port and `cmd.exe` sessions
in two projects, each running `start msedge` with a profile of its own: each
Edge landed beside the stage, starting the second project minimised the
first one's, a tile click switched them back, the tile's button brought its
Edge to the front, and closing an Edge took the mark off its tile. The user's
own Chrome was never touched. The hook costs no CPU to speak of: 16 ms in 5
idle seconds.

### Plain terminals

A project needs terminals that are not an agent: a dev server, a log, a
deploy, a quick `git log`. Asked for so they are at hand beside the
sessions instead of in another terminal app.

- **A shell is a session** with `shell` set (`Session::shell`, saved). So
  it gets a tile, a pane in the project's grid, a place in the saved order,
  the tile menu, and a cluster when it is the project's only session, all
  from the code sessions already had. The alternative, panes with no tile
  like the file viewer, left a dev server with nothing to click once the
  stage showed another project.
- **Ways in.** A small button with a terminal glyph beside the bottom `+`
  (`Hit::Shell`), Ctrl+Shift+T in any pane (`CharAction::NewShell`, as a new
  tab in Windows Terminal; plain Ctrl+T still reaches the program), and
  "New terminal" in the project menu. It opens in the project folder, joins
  the stage without moving it, and gets the keyboard. Named Terminal,
  Terminal 2 and so on.
- **Which shell** (`shell::program`): `HORADRIC_SHELL` by path or name,
  otherwise `pwsh`, otherwise Windows PowerShell, otherwise `COMSPEC`.
- **Not a session to the hooks.** The shell gets no `HORADRIC_SESSION` or
  `HORADRIC_OWNER_PORT`, and loses them when Horadric itself inherited them, so
  a `claude` typed into it is nobody's and stays off the tiles, as it
  would outside Horadric. Found on screen: a dev instance started from a
  session handed its tag down.
- **The tile.** No hook reports on a shell, so its phase stays idle and the
  tile reads the terminal instead: the terminal glyph, the title the program
  set as its second line (`shell::title` drops a title that only names the
  executable, and takes the command from `cmd.exe - npm run dev`), and its
  output as the activity trace, at most one mark a second
  (`Session::touch`). The pane header and the stage's title use the same
  cleaned title.
- **Lifetime.** Any exit closes it, tile and pane, whatever the code: `exit`
  means done, and there is nothing to resume. Closing the stage leaves it
  running. "Start over" leaves a project's terminals alone. Quitting Horadric
  says terminals close, apart from the sessions that resume. After a restart
  a terminal comes back as a paused tile in its place, and a click opens a
  fresh shell there; a reload reopens the ones that were open. What ran in
  it is gone either way: the process question below covers shells too.

Tested on screen with a dev instance: the button opened Windows PowerShell
(no `pwsh` on this machine) as a second pane beside a `cmd.exe` session
with the keyboard, setting the window title from inside it showed on the
tile, the pane header and the stage, `exit` removed tile and pane, a
restart brought it back paused and a click reopened it, one process each
time and no `claude`. Not tested on screen: Ctrl+Shift+T, since a scripted
key would go to whatever window has the focus; the mapping is unit tested.

### The columns

On a laptop taken off a desktop setup, and as projects were added, the
tiles became a mess, with things moving about by themselves. There were
four causes. Every cluster was as tall as its content, the files tile up
to 14 rows, and the stack started a new column wherever one overflowed,
so one new tile or one changed file could move every project below it.
Nothing listened for a screen change, and `arrange` only read the
primary work area, so after undocking the windows stayed wherever Windows
put them. A dragged cluster was saved in pixels, which meant nothing on a
smaller screen. And new clusters were made in `HashMap` order, so the
order changed from one start to the next.

- **Columns as tall as the screen** (`columns.rs`, pure and tested). A
  project keeps its column and its place in it until it is dragged. A new
  project, a new session or a changed file never moves another project to
  another column.
- **Only the files tiles flex.** A cluster's header, tiles and plus keep
  their height, and the files tiles in a column share what is left
  equally (`columns::fill`). One project alone gets a files tile to the
  bottom of the screen, five get short ones. The shares are equal rather
  than by content, so a file that changes never moves anything.
- **When a column is full.** Files tiles that would be under 3 rows fold
  to their header, the one opened longest ago first, and among equals the
  lowest. A click on a folded header opens it again and another folds
  instead. Past that the column scrolls: the wheel over anything that
  does not scroll by itself moves it a tile per notch (`Input::Scroll`).
  Nothing jumps to another column by itself. That was asked for, since
  things moving on their own was what made it a mess.
- **Where a new project goes.** The column with the most room, when its
  cluster fits there with a files tile of 3 rows (`Cluster::need_px`,
  which counts a folder in git before git has answered). Otherwise a new
  column at the right, while one fits on the screen. Several at once, as
  on the first start, go in name order.
- **Dragging.** A cluster or the usage window follows the cursor, and
  when let go it takes the place under it (`Input::Drop`,
  `columns::drop_column`, `columns::drop_slot`): another column, another
  place in its own, or a new column right of the last. Free placement,
  snapping and `pinned` are gone, and so is the files tile's drag handle,
  since the column decides its height now. Asked for: columns only.
- **An order, not pixels.** `columns` in `state.json` is a list of keys
  per column, the usage window's among them (`columns::USAGE`). The pixel
  fields an older file has are read and ignored. A project with no tiles
  left keeps its place while its column has others and it is in the
  recent list.
- **Any screen.** A screen too narrow for every column shows the ones
  that do not fit at the bottom of the last one that does
  (`Columns::shown`). The model keeps them apart, so docking again splits
  them back. A drag on the narrow screen makes the merge real first,
  since the user moves against what they see. `WM_DISPLAYCHANGE` and a
  work area change (`WM_SETTINGCHANGE` with `SPI_SETWORKAREA`) lay
  everything out again, and once more a second later, after Windows has
  moved the windows off a screen that went away. A stage left off every
  screen, or lying over the tiles, docks beside them again.
  `WM_DPICHANGED` on a tile lays its column out again too.
- "Tidy up tiles" scrolls every column back to the top.
- **Which screen.** The columns stand on the primary screen unless "Tiles
  on screen" in the tray menu, shown with more than one screen, picks
  another (`screens.rs`, pure and tested). `screen` in `state.json` keeps
  its device name, `\\.\DISPLAY2`, and picking the primary one clears it,
  so undocking a laptop still brings the columns to its own screen. A
  chosen screen that is unplugged falls back to the primary one and is
  not forgotten, so plugging it in again brings them back. The stage stays
  where it was left, since tiles on a small screen beside a terminal on
  the big one is a reason to move them, unless the tiles now cover it.

Tested on screen with a dev instance on its own port and `APPDATA`, with
the old state file of four paused projects. They came up as three small
clusters under the usage window in the first column, and the one with
fifteen tiles in a second, its files tile folded for want of 6 pixels. A
session in a new git project opened a third column with its files tile to
the bottom of the screen. Dragging it onto the second column put it on
top and pushed the other past the bottom; the wheel scrolled that column
to its end and no further; dragging it right of the last column gave it
its own again. A second git project dragged under it split the column
into two equal files tiles. Clusters, the usage window and the stage,
moved out of place by hand, all came back on a posted `WM_DISPLAYCHANGE`,
the stage docked again since it lay over the tiles. The order came back
after a restart. Not tested on screen: a real undock, a narrow screen
merging columns (unit tested), and a real mouse drag rather than a
scripted one.

### The usage window

A window of its own at the top of the stack: how much of the account's
Claude limits is used, and the model, effort and permission mode every
session Horadric starts gets. Asked for so the limits are in view without
typing `/usage`, and so a model can be picked once instead of per session.

- **Where the numbers come from.** No hook carries usage, and there is no
  public API for a subscription's limits. Claude Code gives them only to the
  status line command, as JSON on stdin after each reply: the model, how
  full the context is, and the five hour, weekly and spend limits with when
  each resets. So Horadric hands every `claude` it starts `--settings
  %APPDATA%\Horadric\claude-settings.json`, written at each start, which makes
  `horadric status` its status line. That passes the JSON to
  `/horadric/status` on the owner's port, tagged like a hook, and prints
  `Haiku 4.5 · context 21% · session 28%` for the terminal. A `claude`
  started anywhere else is not touched, the same rule the hooks keep. The
  path goes without quotes unless it has a space, since PowerShell takes a
  quoted first word as a string.
- **What it shows.** One row per limit Claude Code sent, each with a bar,
  the percent and how long until it resets, blue, amber from 75 %, red
  from 90 %. A limit whose reset has passed reads as empty. There is no
  header: the window belongs to no project, "Claude" on top said nothing,
  and the limits say what it is. The numbers are the account's, so the
  latest from any session wins, and they are saved, so the window is not
  empty after a restart. Before the first reply it says so.
- **Folded** it is the session's budget alone, the five hour limit that
  runs out first, on one row. A click on the limits folds and unfolds it,
  with a chevron beside "Session" as a cluster has beside its name.
- **Settings.** Model and Permissions drop a list, Effort is a slider.
  Each has Default first, which passes nothing and leaves it to Claude
  Code. Models are listed by version (Fable 5.1, Opus 5.5, Opus 5.5 1M,
  Sonnet 5, Haiku 4.5) and passed by full name, so an alias moving on
  never changes the model behind your back. Bypass permissions sits apart
  at the bottom of its list. They are passed as `--model`, `--effort` and
  `--permission-mode` when a session starts or resumes (`Defaults::flags`),
  unless its own arguments already say, and never saved in its arguments,
  so a resume takes the defaults as they are then. Saved as `defaults` in
  `state.json`. Nothing goes to a shell put in with `HORADRIC_AGENT`.
- **Running sessions switch too.** Asked for: picking a model and seeing
  nothing change read as broken. Claude Code has `/model` and `/effort`
  for a running session (`Setting::command`), so Horadric types them in,
  one a second, once each session is free (`Session::free_for_command`):
  at its prompt, its status line heard, and nothing typed into its
  terminal since its last prompt went in, which could be a draft the
  command would run into. A session that chose the setting in its own
  arguments keeps it. The permission mode has no command, so it waits for
  the next start or resume, and its list says so. A settings file change
  does not reach a running session, tried first.
- **Your own defaults stay yours.** Typed in, `/model` and `/effort` also
  save the pick as the default for every new `claude`, anywhere
  (`model`, `effortLevel`, and effort per model under `modelSettings` in
  `~/.claude/settings.json`). Only print mode does not. So Horadric reads
  those keys before the first command and puts them back five seconds
  after the last (`install::SWITCHED`). The running sessions keep what
  they switched to.
- **Context on tiles.** Each session keeps the last status it heard
  (`Session::status`, not saved), which the tile draws, see The look.
- **The window** (`usage.rs`) behaves like a cluster: no focus, dragged
  to a place in the columns the same way, folded by its limits, raised
  from the tray with the rest. It starts at the top of the first column.
  It counts as a tile when the
  stage docks. Kept as `usage_window` in `state.json`.
- **It looks like a cluster too** (see The look): the same faceplate, the
  limits as segmented meters on a screen sunk into it, the settings in a
  grooved section, no accent, since an accent would claim a project, and
  Fluent chevrons for folding and for each list. A list drops in a window
  of its own (`dropdown.rs`) on the same faceplate, a lit lamp by the value
  in use, and it is the one window that takes the focus: it holds the
  mouse so a click anywhere else closes it, takes the arrow keys, Enter
  and Escape, and hands the focus back. The effort slider is a fader: a
  slot with a notch per stop, lit up to a small key that rides in it.

Tested with a dev instance: the empty window, fake limits at 41 % and 82 %
(blue and amber bars, the window growing to fit), a tile's context at
85 %, and a real `claude` started with Haiku and low effort as defaults. It
ran as `claude --model haiku --effort low --settings ...`, one process,
and after its reply the window showed 28 % and 21 % with reset times and
the terminal the status line.

The lists and slider were tested with a second dev instance
(`HORADRIC_PORT=43119` and its own `APPDATA`, since another was running),
its window moved clear of the others and each scripted click checked to
land on it: folding to 88 pixels, picking from both lists, a click
outside closing one, the slider set by dragging, all saved. With two and
then one real `claude` running, picking Haiku 4.5 typed
`/model claude-haiku-4-5-20251001` into each and sliding to Low typed
`/effort low`, their status lines followed, one process each, and
`~/.claude/settings.json` was the same as before five seconds later. Not
tested on screen: a draft holding a command back, and the keys on a list,
since a scripted key goes to whatever has the focus. The draft rule is
unit tested.

### The look

The first look was a default dev tool: flat boxes, one type size, a phase
as a tinted fill, nothing moving. Asked for something with its own
identity. The rule behind it: colour has two jobs and they never share a
place. A phase is light, in a tile's lamp and icon. A project is an
accent, on its mark and on the stage's edge. So a project's colour can never
be read as a session needing you.

The look went through three versions. First deep dark glass: acrylic
behind every cluster, tiles a breath of white. Then clay (commit
`d0354b5`): soft moulded surfaces, raised or pressed in, which made the app
distinct but never sat right beside the terminals. Now a hardware control
panel in the dark, the metaphor that fits: sessions are keys with lamps,
and a terminal is a screen set into the panel.

- **Metal, keys, screens.** Every window is a matte faceplate, a shade
  lighter at the top where the light falls, with a groove cut round it
  and another under the header (`Painter::plate`, `Painter::engrave`). No
  texture: the reality is in bevels, shadows and light. A session is a key
  (`Painter::key`): a face lit from above, a bevel along its top edge, its
  side showing below the face, its shadow on the plate. The plus bays are
  sunk into the plate with a dashed edge, and a key rises into one under
  the cursor. Anything that scrolls sits on a screen sunk into the plate:
  the files, the usage limits. The header counts are small LEDs, the plus a
  round push button, limits and context segmented meters. The stage's
  faceplate is painted with the same light (`TerminalWindow::paint_plate`,
  a GDI gradient and seam), and each pane is a screen set into it: a bezel
  of plate that continues the stage's light from where the pane sits, the
  glass sunk in with rounded corners and shade under its top edge, the
  name printed on the plate above it beside the session's lamp, and the
  glass rimmed in the project's colour on the pane with the keyboard
  (`glyphs::grid_origin`, tested). The terminal's black is the screens'
  glass and its red, green, yellow and blue are the lamps' colours. No
  acrylic any more, so every window is opaque and its text ClearType.
- **Soft shadows without blur.** Direct2D's hwnd targets have no effects,
  so a soft edge is eight copies of a shape, each a little bigger and
  fainter (`blur_steps`, tested). A shadow inside a shape is the outside of
  a moved copy, drawn as a wide stroke under a layer clipped to the shape
  (`Painter::inner`, `Painter::hollow`).
- **Phase as light.** Each key has a lamp down its left edge
  (`theme::lamp`). Working is blue with a hot spot running up and down it.
  Waiting breathes amber and backlights its whole key, light spilling out
  under it, and sends a ring off itself the moment it starts waiting. Done
  is green and flashes once. Idle, paused and ended lamps are dark glass,
  and paused and ended keys are latched down (`theme::depth`) and fade back
  (`theme::presence`).
- **The selected key is held in.** The session whose pane on the stage has
  the keyboard has its key latched in level with the plate, like the one
  button held down on a tape deck: out of the light, shade falling in over
  its top edge, the plate's lit lip under it (`theme::key_depth`,
  `Painter::latched`). It is fully lit, so it never reads as paused. The
  stage keeps `Shared::active` and a change sends `Input::Spotlight`, which
  redraws the clusters, so the latch follows a tile click, a pane click
  and a keyboard move between panes.
- **Project accents.** Eight colours chosen away from every phase colour,
  picked by an FNV hash of the project key (`theme::accent`), so a project
  keeps its colour across runs. It marks the cluster's name, washes faintly
  down from its top edge, rings the cluster whose project is on the stage,
  tints the stage window's border (`DWMWA_BORDER_COLOR`, the accent sunk
  two thirds into the dark) and underlines the pane with the keyboard. All
  of them muted: an accent says which project, it does not outline one.
- **A live tile.** The icon is the tool the turn is in (Segoe Fluent Icons,
  `theme::tool_icon`), or the phase when there is none. The session keeps
  the tool and when it last did something (`Session::tool`,
  `Session::activity`, neither saved), and the tile draws the last ten
  minutes as a trace of twenty bars. How full the context is, from the
  status line, is a short bar under the icon, drawn like the
  usage window's limits; from 75 % the number shows as
  well, amber then red, in the trace's place. The top line says only how
  long, except for waiting, where the verb decides what you do.
- **Motion** (`motion.rs`, `anim.rs`, pure and tested). A new tile rises
  into place, tiles slide when the list changes, hover fades in 120 ms.
  On the stage the panes without the keyboard step back behind a veil of
  the background, and panes fade in when the stage switches project.
  Windows' animation setting (`SPI_GETCLIENTAREAANIMATION`) off holds all of
  it still.
- **Type.** Project names in Segoe UI Variable Display, session names
  semibold, ages with tabular digits so they do not shuffle each second,
  FILES and RECENT letter spaced like legends printed on the plate.

What it costs, measured on a release build with two clusters of fake
sessions: 1.1 % of one core while two tiles work and two wait, 0.8 % with
only waiting ones breathing, 0.2 % with nothing moving. The first cut cost
11 %. Two changes got it down. Everything that holds still is drawn once
into a compatible render target and copied each frame, with only the light
drawn fresh (`Painter::still` and `Painter::light`); the layer is redrawn
when anything else changes. And gradient brushes are kept by their stops
and moved or faded by their properties, since each new one is a texture on
the GPU. A cluster asks for frames by timer only while something moves: 40
ms while a light goes round, 66 ms while only a breath does, none at rest.

Tested on screen with a dev instance on its own port and state folder: the
acrylic behind clusters with Chrome underneath, the orbit, the breath and
the ring leaving a tile that starts waiting, the done flash, three `cmd.exe`
panes on the stage with the unfocused two dimmed, the stage's border
matching its cluster's mark, context rings at 38 % and 82 %. After the first
look proved too light and its borders too bright, the darker pass was
checked the same way beside the installed build.

Not done: the tray menu is still light, and the stage keeps Windows' own
title bar, plate coloured, since drawing our own means taking over dragging,
snapping and the caption buttons. Panes are still square child windows; the
glass inside them is what is rounded.

### Quality of life

Seven things found by reading the app for what daily use would miss first.

- **A notification when a session needs you.** The tiles are not topmost,
  so an amber tile under an editor went unseen. When a session starts
  waiting (`Phase::Waiting`, never idle after a finished turn), the tray
  icon sends a notification: "fix-login needs permission" over the tool it
  asks about (`inbox::alert`). Several at once are one, "3 sessions need
  you". Nothing is sent for a session you are looking at, the stage in
  front with it on it. A click shows that session, or for several the one
  that waited longest (`NIN_BALLOONUSERCLICK`). "Notify when a session
  needs you" in the tray menu switches it off, saved as `quiet`. It is a
  `Shell_NotifyIcon` balloon, which Windows 11 shows as a toast and keeps
  in the notification centre, so Do not disturb holds it back by itself.
- **Zoom a pane.** Its header has a button at the end, and a double click
  on the header or Ctrl+Shift+Enter does the same: the pane fills the stage
  alone, the rest hidden at their size so their programs are not told of a
  resize. Zoom follows the keyboard: a tile click or Ctrl+Alt+arrow shows
  that pane instead, so zoomed reads as one pane at a time. Switching
  project ends it.
- **The keyboard between panes.** Ctrl+Alt+arrow moves it to the pane in
  that direction (`layout::neighbour`, the nearest one past the edge, most
  in line). Ctrl+Alt, not Alt, because Alt+arrow is word movement in a
  shell. The stage chords are one pure table (`keys::chord`), and a chord's
  own `WM_CHAR` is taken off the queue so it never reaches the program.
- **Font size.** Ctrl and plus or minus, Ctrl+0 for the default, or Ctrl and
  the wheel, one DIP a step from 9 to 32 (`keys::font_size`). One size for
  every pane, saved as `font_size`. The `Font` makes new text formats for
  the size; the glyph cache keeps, since glyph indices do not depend on it.
- **Rename a session.** "Rename..." in a tile's menu asks in a plain Win32
  dialog built from a template in memory (`ask.rs`), so no resource file
  ships. The name beats Claude's title (`Session::renamed`, saved) until a
  newer `/rename`, and an empty one gives the naming back to Claude.
- **Search the history, and more of it.** Ctrl+Shift+F opens a bar over the
  pane's top right corner. Typing searches up from the bottom, literally
  (`find::pattern` escapes what a regex would read), ignoring case unless
  the query has a capital; the match is selected and scrolled into view, so
  Ctrl+C copies it. Enter or Up goes further up, Shift+Enter or Down comes
  back down, Esc closes it. It is `alacritty_terminal`'s own regex search,
  and works in a file view too. History went from 2000 rows to 10,000, what
  Windows Terminal keeps: 2000 was gone in an afternoon of Claude Code.
- **Open the project** in VS Code or in Explorer, from the project menu.
  VS Code is found as for the files tile; without it the line is greyed.

Tested on screen with a dev instance on its own port and `APPDATA`, running
`cmd.exe`: the zoom button zoomed and unzoomed, a double click on a header
unzoomed, moving right while zoomed showed the next pane, Ctrl+Alt+Left and
Ctrl+Shift+Enter did the same from the keyboard, Ctrl+Shift+F then "needle"
selected the last match and Enter the one above with nothing reaching the
shell, Ctrl+= twice made the font 17 and saved it, and Rename through the
tile menu renamed the tile, the stage's title and `state.json`. The keys
were sent to the dev instance's thread with its key state set through
`AttachThreadInput`, not as real input, so nothing reached another window.
The notification was accepted by Windows (logged with `HORADRIC_DEBUG`) but
not seen, since Do not disturb was on; how it looks and a click on it are
not tested yet. The rename dialog opened behind other windows, as anything
opened from a synthetic click does.

### The task list

A list of work per project that agents take items from, one at a time or
all the way down by themselves. Asked for because every feature or fix
meant starting a terminal by hand and pasting the story in. Linear and the
like were ruled out as too much: this is a text file and a tile, not a
tracker.

- **The list is a file in the repo**, `.horadric/tasks.md`, beside
  `.horadric/config.json`, which holds the mode (`{"tasks": {"mode":
  "auto"}}`) and will hold step 4's settings and the SSH hosts. The human
  edits the list in VS Code, agents read and write it like any other file,
  and git keeps its history. Nothing about it is in `state.json` but
  whether the tile is folded. Committing it is the project's choice.
- **The format** is a Markdown checklist, and the file order is the work
  order. Moving a line is how priority changes. Indented lines under an
  item are its notes and go to the agent with it.

  ```
  - [x] Rename Glance to Horadric
  - [/] Fix the login redirect @fix-the-login-redirect-51234
    Happens only after a session expires. Repro in #12.
  - [?] Add dark mode to the settings page @add-dark-mode-51300
  - [!] Migrate to the new API @migrate-api-51400: needs a key I do not have
  - [ ] Show the build time in the footer
  ```

  `[ ]` open, `[/]` being worked on, `[?]` done by the agent and waiting
  for review, `[!]` blocked with a reason, `[x]` done. `@id` is the
  Horadric id of the session that holds it, so the file alone says who has
  what and a restart loses nothing. Only lines starting at the left edge
  with `- [` or `* [` are items; headings, prose and nested lists are left
  out. An `@` inside the title stays in the title: only the last ` @id` at
  the end, or before a colon, is a holder. No dependencies, labels,
  estimates or assignees.
- **Who writes what** (`horadric_core::tasks`, pure and tested). Horadric
  changes the marker and the `@id`, one line at a time: read the file,
  change that line, write it back through a temporary file and a rename
  (`horadric_hooks::tasks::update`), every other byte as it was, CRLF
  included. A change finds its item by line and title together, or by
  holder, and does nothing when the file moved under it. Everything else
  belongs to the human and the agents.
- **Taking an item** (`runner.rs`, `App::take_task`). A click on an open
  row writes `[/] @id` into the file first, then starts the session, named
  after the item, with the item, its notes and one line on how to report
  as its first prompt. The file is the lock: a list read a moment later
  already says the item is taken. `--append-system-prompt` tells the agent
  it works one item of the list, to commit when finished, how to report
  (`task done`, `task blocked "why"`), to file other work with `task add`
  rather than do it, and the file's format, so a planning item can write
  its own items below itself. It goes on every start and resume while the
  session holds an item; the first prompt only on the first start, never
  saved with the session's arguments, or a resume would send it again.
  A failed start puts the item back as open.
- **The first prompt says how to report too.** Tested with Haiku: given
  only the system prompt it answered the item and never ran `task done`.
  With one line at the end of the prompt, `(An item from
  .horadric/tasks.md. When it is finished, run `.../horadric.exe task
  done`.)`, it ran it. The human sees that line in the terminal, which is
  no loss.
- **`horadric task done|blocked WHY|add TITLE|list`** (`task.rs`). The
  command changes the file itself rather than asking the app, so the agent
  hears at once whether it worked: it walks up from its folder to the
  list holding an item for `HORADRIC_SESSION`, marks it `[?]`, or `[x]` in
  auto mode, or `[!]` with the reason, then posts to `/horadric/tasks` on
  `HORADRIC_OWNER_PORT` so the app looks at once. The agent runs this
  build by its full path with forward slashes (`runner::command_for`),
  since the `horadric` on `PATH` may be another build and Git Bash and
  PowerShell both take it.
- **Why the agent reports done.** `Stop` only means a turn ended. It is
  just as often a question as a finished job, so the hooks cannot tell the
  two apart. The agent can.
- **Three modes per project**, from the button in the tile's header:
  - *Manual.* Nothing starts by itself. Click items to take them. Done
    goes to review.
  - *Review.* Horadric takes the first open item. When the agent reports
    done, the item goes `[?]`, the row shows an approve button and a
    notification says so. Approve and the item goes `[x]`, the session
    closes and the next item starts. Or type into the session what is
    wrong; the agent goes on and reports done again.
  - *Auto.* The same without the gate: done goes straight to `[x]`, and
    the list is worked until it is empty. Then a notification says so.
- **One fresh session per item.** Carrying one session down the list
  would fill its context with the items before. The commit the agent made
  is what the next one builds on. A session whose item is `[x]` closes
  once it is not mid turn, since the agent reports from inside its turn.
- **When the runner stops.** At a blocked item, since the order is the
  order and the next may need this one; an item added meanwhile waits
  behind it. At an item whose session is gone. At a paused session, after
  a restart: the runner never resumes one by itself, a click on its row
  or tile does. At a permission prompt, like any session, so how far auto
  mode gets alone depends on the permission mode in the usage window.
- **When a usage limit runs out** the runner waits instead of stopping
  (`Limits::out_until`, pure and tested). While the numbers the status
  line last gave say a limit is at 100 %, nothing starts until a minute
  past its reset, the latest reset if several are full. A session on an
  item that the limit refuses mid turn (a `StopFailure` with
  `rate_limit`) gets no nudge; the runner notes when the fullest limit
  resets, holds every list until then, and a minute after types into it
  once that the limit has reset and to go on with the item
  (`tasks::go_on`). With no reset known it stays for the human. A
  notification says when the list goes on. Only typed into a running
  terminal, so it never starts an agent. Tested with a dev instance and
  faked limits: a full limit resetting in 20 s held the first start for
  80 s, and a refused session was told to go on 70 s after a limit at
  97 % resetting in 10 s, once.
- **The nudge.** A session in review or auto mode whose turn ends without
  a report is asked once, typed into its terminal, whether it is finished,
  with the command spelled out. The Enter follows 400 ms later, so the
  input box takes the text as typed and not as a paste with a newline in
  it. If it stops again after that without a report, the row reads "asks
  you" and a notification says it needs you. Asked for by the user once
  the plan left it open.
- **Fuses.** The runner is the one part of Horadric that starts agents by
  itself, which is what ran away once. It holds one item per project, or
  as many as `parallel` says and never more than 8, it starts at most one
  session per project every 10 seconds, it never resumes a paused session
  and starts nothing beside one, a start writes the file before it launches
  and checks the item is still open in a fresh read, and `launch` still
  refuses a second process for a session.
- **Notifications**, once each while they hold: an item ready for review,
  one blocked (with the reason), one whose session stopped after the
  nudge, and a list finished by the runner.
- **The tasks tile** (`board.rs` for what a row says, pure and tested;
  `layout::TasksLayout`; `Painter::tasks`). It sits between the plus and
  the files tile, on a screen like the files: a chevron and TASKS, a
  summary ("2 of 5 done"), the mode as a small key that opens a menu, and
  a plus that asks for a new item with the rename dialog. A row per item
  not done, eight before the wheel scrolls: a glyph and a word in the
  colour its session's lamp burns in (working blue, asks you and review
  amber, blocked red, paused and session gone dim). A click on an open row
  takes it, on a row whose session is gone starts it again, on any other
  shows its session. Right click: start, show session, approve or mark
  done, put back in the list (which ends its session), edit the list. Its
  height is fixed, counted like the plus row; only the files tile flexes.
  The fold is kept as `tasks_collapsed` per cluster.
- **A list keeps its project up.** A project in the recent list whose
  list has an item not done gets its cluster with no session in it, so
  the list shows and its runner runs (`tasks::unfinished`,
  `App::listed`). Once every item is done and no session is left, the
  cluster goes as it did before. Only recent projects, the eight the
  tray offers, since those are the folders Horadric knows. Tested on
  screen: a recent folder with two open items came up as a cluster with
  only the tasks tile, marking both done took it away and an added item
  brought it back within a second, auto mode started one `cmd.exe` on
  it, and `task done` closed that session and the cluster.
- **Reading the list.** Every second the app compares each project's list
  and config with when they last changed (`fs::metadata`, two calls a
  project) and reads again only what changed, so an edit in VS Code shows
  within a second. Not the files tile's `ReadDirectoryChangesW` watcher:
  that one only runs in git projects, and ignores what git ignores, which
  a `.horadric` folder may well be. Horadric's own writes and `horadric
  task` read at once.

Tested on screen with a dev instance on its own port and `APPDATA`, first
with `HORADRIC_AGENT=cmd.exe` and `horadric task` typed by hand: a click on
a row wrote `[/] @id` and started one `cmd.exe`; `task done` from a
subfolder marked it `[?]` with a notification; the approve button marked it
`[x]` and closed the session. Auto mode, switched by writing the config,
started the next item within a second, closed each finished session and
started the next, never more than one agent for the list. `task blocked`
showed a red row and stopped the runner, with an item added behind it left
waiting; `task done` on it went on. A fake `Stop` got the nudge typed and
entered in the terminal, and only the stop after it the "needs you"
notification. After a restart the held item read "paused", nothing
started, and a click on the row resumed it. The plus asked for a title and
review mode started the new item at once. Then with a real `claude` on
Haiku in review mode: the runner started it, it ran `task done` after its
permission prompt, the row went to review with a notification, and
approving closed the session and said the list was done. Not tested on
screen: the right click menu and the mode menu, which a synthetic click
opens behind other windows.

### Background sessions

Claude Code 2.1 runs sessions in the background: `claude --bg`, or the
left arrow on an empty prompt, which sends the session there and opens
the agent view that lists them. A shared `claude daemon` runs them, so no
Horadric terminal holds them, and they show as tiles anyway.

- **Finding them.** `Registry::route` sends an event to the tile that
  holds its conversation's `session_id`, whatever its tag says, then by
  the tag. A conversation no tile holds is a stranger, and the feeder asks
  `claude agents --json` whether it is a background session: at most once
  a minute for the same conversation, since every plain `claude` on the
  machine posts its hooks here untagged, and the listener now passes
  those on without a tag instead of dropping them. One that is gets a tile
  `bg-<short id>`, named from the list, in the cluster of its folder.
  Nothing else untagged ever shows. At start the feeder asks once, so
  those already running show before their next hook. The phase comes from
  hooks as for any tile; until the first, from the list's `state`.
- **A session sent back from a Horadric pane** keeps its id, so it stays
  on its own tile, and the pane is left with Claude Code's agent view.
- **A click attaches.** The tile opens a pane running `claude attach
  <short id>` (`Run::Attach`), started like a shell: untagged, no
  Horadric flags, no register, so the tile keeps its phase. Detaching or
  closing the pane ends only the attach; the tile stays and is checked
  against the list, and ends if the session is gone. "End session" runs
  `claude stop`, which sends a `SessionEnd`.
- **Never saved.** The daemon keeps the session, and a resume would start
  a second copy of it. An attached pane's host is stopped when the app
  quits or reloads, since nothing would bring it back.

Tested on screen with a dev instance on its own port and `APPDATA`: at
start it showed this session's own background session; a `--bg` probe
posting untagged to the dev port got its tile from its first hook and
went done with its reply; a click attached in a pane, which showed the
conversation; ending the attach closed the pane and kept the tile;
`claude stop` ended the tile. The tile menu's entries were not clicked,
the installed column stood over the dev one. A background tile looks like
any other; a mark of its own would help.

## Next

### Step 4: worktrees and the git glance

- `git worktree add` per session, branch named from the session name.
- `.horadric/config.json` per repo with `setup` commands, copied into each new
  worktree because gitignored files do not come along. Superset learned this
  the hard way; their worktrees break without it. Allocate a port range per
  worktree too, so dev servers do not collide.
- `git diff --numstat` for the changed file list with plus and minus counts,
  uncommitted versus committed.
- A button that opens the worktree in VS Code (`code <path>`).
- **The project key has to change.** It is the working directory today, so
  every worktree would become its own cluster. It must resolve to the parent
  repository, via `git rev-parse --git-common-dir`.
- Worktrees must be optional per project. A session that wants the shared
  working tree, or is not in a repo at all, has to keep working.

Built so far: the project key resolves to the main working tree, each new
session gets a worktree of its own, and its tile shows what it changed.

- **Where.** A new session started from a repository's main working tree
  (the plus, the project menu, the start window, `horadric new`) runs `git
  worktree add -b <branch>` from what the main tree has checked out. The
  branch is the session's name made into a slug, `session` when it has
  none, with 2, 3 and on after it when taken. The folder goes beside the
  main tree, `app.fix-login` for `app`, so the main tree's watchers never
  see it. A session started in a subfolder starts in the same subfolder of
  the worktree. Carrying on a past conversation (`--resume`, `--continue`)
  keeps the folder it was held in, since Claude Code finds it by that. A
  folder already in a linked worktree, outside a repository, or a git that
  refuses, keeps the shared tree.
- **Config.** `.horadric/config.json`, read from the main tree since it may
  be kept out of git: `"worktrees": {"setup": [...], "ports": 10}`. On by
  default. `"worktrees": false` or `"enabled": false` keeps the shared tree,
  and "A worktree for each new session" in the project menu switches it.
- **Setup.** The commands run in the session's own pane before the agent,
  through `horadric setup <program> <args>`, which runs each with `cmd /d /s
  /c` and then starts the agent with its arguments exactly as given (a
  batch file in between would have had to quote them for cmd). A command
  that fails says so and the agent starts anyway, since it can read the
  error and put it right. Only on the first start: a resume goes straight
  to the agent.
- **Ports.** Each worktree gets the lowest free range of `ports` ports on a
  step from 4100, above the defaults dev servers pick (3000, 5173, 8080),
  which the main tree keeps. The session gets `PORT` and
  `HORADRIC_PORT_FIRST` and `HORADRIC_PORT_LAST`, and is told the range.
  The worktree and its range are saved with the session, so a resume gets
  the same ones.
- **Telling the agent.** Its system prompt says where its worktree is, its
  branch, where the main tree is and not to edit there, to commit on its
  branch, and its ports. Joined with the task list's and the hosts' prompt.
- **Ending a session** removes its worktree with plain `git worktree remove`
  and then `git branch -d`. Git refuses the first while the tree has
  changed or untracked files and the second while the branch has commits
  the main tree lacks, so nothing is lost: what refuses stays for the human.
  Quitting pauses and removes nothing.
- **Sweeping merged worktrees.** Ending a session cannot clean up a
  worktree whose branch was merged later, nor one an agent added by hand,
  and those piled up: eight of them beside this repository after one
  evening of parallel branches. So the app sweeps each project with a
  session at startup and after any of its sessions is heard from, at most
  every 10 s a project, and only once the main tree's HEAD has moved, since
  nothing is merged while it stays put. A linked worktree goes, with `git
  worktree remove` and then `git branch -d`, when its branch is merged into
  what the main tree has checked out, `git status` in it says nothing, no
  session (running or paused, or about to start) has its folder there, and
  the branch's reflog shows at least one commit on it. The last keeps a
  worktree just added and not yet worked in, which counts as merged. A
  locked or detached worktree is left alone. Gitignored files in a swept
  worktree go with it. Tested with a dev instance on a scratch repository
  of five worktrees: the merged one went with its branch, the busy, dirty,
  fresh and unmerged ones stayed, and merging the unmerged one got it swept
  at the next hook. That dev instance still held sessions in this
  repository, so its first sweep also removed the eight leftovers here,
  and kept the four worktrees in use.
- **The task runner** holds one item at a time in the shared tree, unless
  `"tasks": {"parallel": 3}` in the config lets it hold several. Then each
  item it takes (or a click takes) gets a worktree of its own, its branch
  named after the item, and the runner fills a free place with the next
  open item, one start every 10 seconds. A blocked item or one whose
  session is gone still stops the list, and a paused one holds it until a
  click resumes it. With worktrees off, or outside a repository, `parallel`
  counts as 1, since items side by side in one tree edit the same files.
  The list lives only in the main working tree: the app reads it there,
  the agent is told its path and that it is the one file in the main tree
  it may change, and `horadric task` finds it through `HORADRIC_TASKS`,
  which a session in a worktree gets, since the worktree has no list or an
  old copy. A finished item's session ends as always; its branch stays
  until the human merges it, so an item no longer builds on the one before
  unless that one was merged first. Tested with a dev instance and
  `HORADRIC_AGENT=cmd.exe`: with `parallel` 2 in auto mode, item one
  started in `par.item-one`, item two 10 s later in `par.item-two`, and
  item three waited. `task done` run from inside `par.item-one` with its
  session's environment marked the main tree's list, the session closed,
  its clean worktree went away and item three started in its own. Three
  `cmd.exe` the whole time (two items and one plain session). Not checked
  on screen: that `HORADRIC_TASKS` reaches the agent's shell, which goes
  the same way as the ports.
- **Merging a finished item.** When a session ends and `git branch -d`
  refuses its branch, the app hears of it, and if the branch is a done
  item's (its title's slug, or that with 2, 3 and on after it:
  `worktree::finished`, pure and tested) a notification says "Finished:
  <item>, click to merge <branch> into main". The click asks yes or no,
  and yes runs `git merge --no-ff --no-edit` in the main tree, then `git
  branch -d`. A merge that stops (a conflict, or local changes in the way)
  is undone with `git merge --abort`, so main is never left half merged,
  and the notification says why and that the branch stays. The project
  menu lists "Merge <branch> into main" for every done item's branch not
  merged yet, from `git branch --no-merged HEAD`, so a missed notification
  or a restart loses nothing. Tested on screen with a dev instance and
  `HORADRIC_AGENT=cmd.exe`: an item with a commit on its branch got the
  notification, the click asked, yes gave a merge commit on main and the
  branch went; a second item whose change conflicted with one on main left
  main clean and kept its branch. The project menu's entries were not
  clicked, the installed Horadric's column stood over the dev one. The
  lists are now read from the project's folder rather than a session's:
  a project whose sessions were all in worktrees showed an empty list,
  since a worktree has none.
- **What it changed.** A tile whose session has a worktree shows the lines
  added and removed, `+9 −1` in the files tile's green and red, beside the
  last line, and a `</>` button that opens the worktree in VS Code. Its
  right click menu has a Changes submenu: each file with its counts, those
  not committed apart from those committed on the branch, and a click
  shows the file on the stage. Not committed is `git diff --numstat HEAD`
  plus untracked files, all their lines added; committed is `git diff
  --numstat <main tree's HEAD>...HEAD`, from where the branch left. The
  count (`diff.rs`, pure and tested) runs on a thread after the agent did
  something, at most every 3 s, and again as the menu opens. Nothing
  polls, so an edit made outside the agent shows at its next hook. Tested
  on screen with a dev instance: a worktree with a changed file, an
  untracked one and a commit showed `+9 −1`, the menu split them as git
  does, and a click showed the worktree's copy of the file. The VS Code
  button was not clicked, it uses the same launch as the project menu.

### SSH hosts

A project's servers, at hand from the project: a terminal on the VPS a
click away, and agents that know the servers exist and work on them. Asked
for because a lot of what a project runs on lives on a VPS, and telling the
agent what to do there should be enough.

- **The agent stays local.** A session runs on this machine as always and
  reaches a server with `ssh myvps '<command>'` from its own shell tool. It
  has the code and the server in one place, so it can fix a bug, deploy it
  and read the logs after. Running `claude` on the server instead was ruled
  out: its hooks would post to the server's own localhost, which needs a
  reverse tunnel per connection, and every server would need Claude
  installed and logged in.
- **Horadric does not speak SSH.** It runs the `ssh` that ships with
  Windows, which reads `~/.ssh/config` for aliases, users, ports and keys.
  A library would be a dependency for what one executable already does,
  the same reasoning as the VT parser. Keys and passwords never pass
  through Horadric.
- **A project's hosts** are a list in `.horadric/config.json`, the
  `config.json` of step 4 that the task list needs too: `"hosts":
  ["myvps", "deploy@203.0.113.7"]`, each anything `ssh` takes as a
  destination. An alias is best, since it keeps users and ports out of
  the repo. "Add host" in the project menu asks for one, as the rename
  dialog does, and offers the `Host` names in `~/.ssh/config` that have
  no wildcard.
- **The SSH terminal** is a plain terminal (see Plain terminals) whose
  program is `ssh <host>`. "SSH to myvps" in the project menu, one entry a
  host. It gets a tile, a pane and a place in the order like any shell, a
  glyph of its own, and the host as its second line until the remote shell
  sets a title. An exit with code 0 closes it, as a shell does. Code 255 is
  `ssh` failing (connection refused, dropped, key refused), so the pane
  stays with the error in view and the tile goes paused; a click
  reconnects. After a restart it comes back paused, as a shell does.
- **Telling the agent.** Every session in a project with hosts gets them
  through `--append-system-prompt` when it starts or resumes: which hosts
  belong to the project, to reach them with `ssh <host> '<command>'`, and
  to pass `-o BatchMode=yes`, so a host that wants a password or a
  passphrase fails at once instead of hanging on a prompt the agent cannot
  answer. With the task list's prompt the two are joined into one. A
  resume takes the list as it is then, like the defaults.
- **Permissions are the user's.** A command on a server goes through the
  permission mode picked in the usage window like any other command. With
  bypass permissions on, the agent runs remote commands without asking,
  because that is what the user chose. Horadric adds no gate of its own.
- **Keys the agent can use.** An agent's shell is not interactive, so the
  key has to work without typing: no passphrase, or one held by an agent
  its `ssh` can reach. Git Bash, which Claude Code uses on Windows, brings
  its own `ssh` that does not talk to the Windows `ssh-agent` service.
  Worth checking on screen which one the agent gets, and saying in the
  prompt which `ssh` to run if it matters.
- **Pure and tested:** reading `hosts` from `config.json`, the `Host`
  names out of an `ssh_config` (wildcards and `Match` blocks skipped), the
  command line for an SSH terminal, the prompt text, and the exit code to
  close or keep.
- **Verifying it.** On screen against a real server with a harmless
  command (`uptime`), and against `localhost` if the Windows OpenSSH server
  is installed. The agent side with `horadric run -- -p "Run uptime on
  myvps" --model claude-haiku-4-5-20251001` in a project with a host.

Done: hosts in `config.json` (`horadric_core::ssh`, which leaves out a
host that starts with `-` or holds a space, since the file comes with the
repository, and passes the host after `--`), and "SSH to <host>" in the
project menu. An SSH terminal is a shell with `Session::ssh` set, named
SSH, SSH 2 and so on, running `System32\OpenSSH\ssh.exe` before any `ssh`
on `PATH`. Tested on screen with a dev instance against `localhost` with
no SSH server: the menu listed the host once and left out a
`-oProxyCommand` one, the refusal stayed in the pane with the tile paused
and `localhost` as its second line, a click reconnected with one
`ssh.exe`, and a restart brought it back paused with its host. Not tested
on screen: a session that connects and exits 0, since this machine has no
server to reach; it takes the path a shell's exit takes.

Done: the prompt for agents (`ssh::system_prompt`). Every Claude Code
session the app starts or resumes in a project with hosts gets it, joined
with the task list's into one `--append-system-prompt`, and so does one
from `horadric run`. Checked on screen: the agent's Git Bash runs its own
`/usr/bin/ssh`, which cannot reach the Windows `ssh-agent` pipe, so the
prompt names the Windows `ssh.exe` by its full path with forward slashes,
the same one the SSH terminal runs. Verified with `horadric run -- -p "Run
uptime on localhost"` on haiku: it ran
`C:/WINDOWS/System32/OpenSSH/ssh.exe -o BatchMode=yes localhost 'uptime'`
and reported the refusal, and a dev instance's `claude.exe` carried the
prompt on its command line. Not tested: a host that answers, for want of
one.

Order of work: hosts in `config.json` and the SSH terminal from the
project menu, then the prompt for agents, then "Add host" with the
suggestions from `~/.ssh/config`.

### Sessions that outlive Horadric

Today every pseudo console is created by the Horadric process. When that
process ends, its `HPCON`s close, conhost goes, and every agent with it.
Persistence and `reload` make the restart cheap (a click, or nothing, and
`claude --resume` brings the conversation back), but a restart still costs:
the turn in flight is cut off, tools the agent was running die, the
scrollback is gone, and `reload` has to wait for every session to be idle.
A crash used to be worse than a reload, since the sessions came back
paused. Option A, below, is built: they come back running.

The console can only outlive the UI if a process that is not the UI created
it. An `HPCON` is not a kernel handle and can not be handed to another
process, so there is no way to move a console after the fact. Whatever
survives has to own it from the start.

**Option A: stay in process, resume after a crash.** Built, see "After a
crash" under the reload. No new process. Mark
`running` in `state.json` all the time, not only on `reload`, and on a start
after an unclean exit resume those sessions as `app --reload` does. Small,
maybe a day. Covers the conversation, not the turn in flight, the tools or
the scrollback. Worth doing whatever else is chosen, since logoff and
restart end every process anyway.

**Option B: one host process per session.** `horadric host` is a small
process that creates the console, starts the agent, and serves one named
pipe, `\.\pipe\horadric-<instance>-<session id>`, with a DACL for the
current user only. The UI connects to it and sends input and resizes; the
host sends output and the exit code. The host keeps the last few MB of raw
output in a ring, and a UI that attaches replays the ring into a fresh
`Term`, then forces a resize so a full screen program like Claude Code
redraws. The host does not parse anything, so it has no `alacritty_terminal`
and almost no reason to change between builds. A host crash takes one
session; a UI crash takes none. Cost: one more process a session, a few MB
each, about 150 MB for forty.

**Option C: one broker for all sessions.** The same, but one process holds
every console, like a tmux server. Fewer processes and one pipe. A bug in
the broker ends every session at once, which is the thing we are trying to
get away from, and the broker can never be restarted without ending them
all, so every change to it is the same problem again one layer down.

**Option D: an existing multiplexer.** tmux in WSL, or similar. Rejected:
it would put the agents in Linux, not Windows, and it adds a toolkit
between us and the terminal, which is the settled decision.

**Decided 2026-09-25: A first, then B**, with Quit as proposed below. A is cheap and still needed for logoff
and restart. B isolates the failure where it happens and keeps the part that
must never change tiny. Things B has to settle when it is built:

- **Quit.** Tray Quit could leave the hosts running (sessions go on, tiles
  come back on the next start) or end them as today. Proposed: Quit asks,
  with "keep running" as the default when any session is mid turn.
  Decided.
- **Reload** stops waiting for idle sessions: the new build attaches to the
  same hosts. The UI and the host speak a versioned protocol, and a new UI
  must understand every host version still running, since hosts from old
  builds live on until their session ends. Keep the protocol to five
  messages (input, resize, output, exit, kill) so that stays easy.
- **Hooks while no UI runs** go nowhere, since they are HTTP posts to the
  port. On attach, the phase comes from the transcript tail as it does for
  the title, and is right again at the next event. Buffering in the host
  would mean the host listens on the port, which is not worth it.
- **Orphans.** A host with no UI and an exited agent ends itself. A host
  whose agent is still running stays, and `horadric` with no app running
  lists them, so a session never runs where nobody can see it.
- **Jobs.** The host is started with `CREATE_BREAKAWAY_FROM_JOB` and
  detached, so it outlives the UI even when the UI itself runs in a job, as
  it does when started from inside Claude Code. The job that traces windows
  back to a session moves into the host, which answers "is this process
  yours" over the pipe.
- **Dev instances** name their pipes with their own instance, so a dev UI
  never attaches to an installed session.

Sizing: A is a day. B is about a week: the host and its pipe server, the
UI side replacing `Pty` in `Console` behind the same four calls, attaching
on start, the replay, and on screen tests of kill the UI, start it again,
and find every session where it was.

**Option B is built**, as settled above, with these details:

- **`horadric host`** reads a JSON `Spec` (the command and the pipe name)
  on stdin, takes the first instance of its pipe with
  `FILE_FLAG_FIRST_PIPE_INSTANCE` (so a second host for the same session
  fails before it starts a second agent), starts the program in a pseudo
  console, and serves until the program ends. A startup error goes to
  stderr, which the UI reads and shows as it did before.
- **The pipe** is `\\.\pipe\horadric-<port>-<session id>`, the port
  standing for the instance, so a dev instance never attaches to the
  installed one's sessions. Its DACL names the current user's SID and
  nobody else, and it refuses remote clients. Every handle is overlapped:
  a synchronous handle runs one call at a time, and the reader sitting in
  a read would hold up every write.
- **The protocol**, `horadric_pty::wire`: a hello (`HRDH` and a version
  byte, 1 today), then frames of a kind byte, a length and a body. Five
  kinds: input, resize, output, exit, kill. Unknown kinds are skipped. The
  host sends its console size first, as a resize, then the replay, so the
  UI builds its grid at the size the replay was drawn at.
- **The ring** is the last 4 MB of output. Once it has dropped bytes the
  replay starts after the first line break, out of whatever escape
  sequence it began in. On every attach after the first the host resizes
  the console one row smaller and back, which makes ConPTY repaint and
  Claude Code redraw.
- **One client, the newest.** A UI that connects while another holds the
  pipe takes it over, as during a reload. The old one is disconnected.
- **Exit.** The host sends every byte of output, then the exit code, waits
  up to 5 seconds for the UI to read it, and ends. With no UI connected it
  ends at once. A host that disappears without an exit reads as
  `HOST_LOST`, a non zero code, so the session pauses and a click resumes
  it.
- **The binary** hosts run from is a copy of the UI's,
  `horadric-host-<size>-<mtime>.exe` in `%LOCALAPPDATA%\Horadric\hosts`
  (`Horadric-dev` for a dev instance). A long lived host running
  `horadric.exe` itself would stop `swap` from moving it aside a second
  time and lock `target\debug` against the next build. Copies no host
  runs from are deleted when a new build makes its own. The status line
  runs from the copy too. Task Manager shows the name, which tells a host
  from the UI.
- **Jobs.** The host is started with `DETACHED_PROCESS`,
  `CREATE_NEW_PROCESS_GROUP` and `CREATE_BREAKAWAY_FROM_JOB`, and without
  the last when the UI's job refuses it. The session's own job is named
  `Local\horadric-<port>-<id>`; the UI opens it for query and asks
  `IsProcessInJob` itself, so "is this window's process yours" needed no
  message of its own. Session jobs allow breakaway, so a dev Horadric
  started from a session's terminal gets hosts that outlive that terminal.
- **Attaching.** On every start, before resuming anything, the app lists
  the pipes with its prefix and attaches to each one that is a saved
  session. Its phase comes from the transcript's last user or assistant
  record (`title::mid_turn`): working after a prompt, a tool call or a
  tool's result, idle after a reply that ended the turn or an interrupt.
  Resuming after a crash (option A) then skips those sessions, since they
  are no longer paused, so A only resumes sessions whose host is gone.
- **Orphans.** `horadric` with no app running prints the sessions that ran
  on without it, then starts the app, which attaches to them.
- **Quit** asks "keep them going without it?" with Yes, No and Cancel.
  Enter is Yes when a session is mid turn, No otherwise. No kills every
  session and waits up to 3 seconds for the hosts to confirm, since the
  kills leave on threads that end with the process.
- **Reload** hands over at once. An installed Horadric from before hosts
  still waits for idle sessions and ends them on the way out, so the first
  reload into this build resumes them as before; from then on they run
  through.

Tested with a dev instance, three `cmd.exe` sessions (one printing a line
a second) and one real `claude`: the dev UI killed four times and started
again, then `reload`. Every session came back each time with its screen
replayed and the counter further on, the `claude` with the same pid, two
`claude.exe` on the machine (the agent doing the test, and that one).
Killing one host paused only its tile. Found on the way: the `windows`
crate turns an error into an `io::Error` holding the HRESULT, so a missing
pipe never read as `NotFound` and the UI gave up on a host it had just
started; `pipe::os` converts it back. Not seen on screen: the Quit
question itself, since a message box would have taken the screen from
the human.

### Step 5: inbox, installer, updater

- The inbox is the sessions waiting on you, oldest first, with what each is
  waiting for. The hotkey (see The stage) walks it already. Still open
  whether it also needs a window or a sort order inside the clusters. Decide
  when forty sessions is real, not before.
- A signed updater, planned below under "The updater". `horadric install`
  covers installing for now; NSIS only if a download for other people
  needs it.
- The tray icon exists (see Launchers). "Start with Windows" is a checked
  item in its menu, a value under the user's `Run` key that starts
  `horadricw.exe`. A dev instance has no such item, and `autostart` itself
  refuses to write the value under `HORADRIC_DEV`, so no path turns it on.
  "Show terminal" opens the stage again after its cross closed it, on the
  project it showed then, or the first cluster with a terminal when that
  one is gone, and brings it to the front when it is open. Greyed out
  with no terminal to show.

### The updater

The signing is built: `horadric release keygen` and `horadric release
sign`, the manifest in `horadric_core::release`, CNG in
`horadric_ui::update`. The signed bytes are `horadric release 1` and a
newline, then compact JSON of version, notes and files in that order,
rebuilt from the parsed fields, so the file's layout and key order do not
matter. The check is built: `horadric_ui::update::look` over WinHTTP
(`net.rs`), on the first tick after start, every 24 hours, and from "Check
for updates" in the tray. A verified newer version adds "Update to
<version>". A check that finds this build up to date takes the item away
again; a failed one leaves it. The install is built too:
`horadric_ui::update::download`, then the app's own `Reload`, as below.

Today a new build reaches this machine through
`reload`, from a checkout. The updater is for a machine with no checkout:
it fetches a release, checks it was signed by us, and hands it to the same
`reload`.

**What Purrch does.** `tauri-plugin-updater`: a `latest.json` manifest at
`github.com/<repo>/releases/latest/download/latest.json`, a minisign
(Ed25519) signature per installer, the public key in `tauri.conf.json`,
the private key in `%USERPROFILE%\.purrch\updater.key` and a CI secret. A
release is a draft until a human publishes it, and publishing is the moment
every install is offered it. The app never installs on its own: it looks
when asked, says what it found, and installs on a click. `RELEASING.md`
there says why the key must be backed up: lose it and every install is
stranded.

**What we take.** The trust model, the manifest at `releases/latest`, the
draft that a human publishes, and install only on a click. Horadric runs
agents with permission checks off too, so an unattended self replace is the
same bigger ask. Not the plugin: it is Tauri.

**What already exists.** `reload` is most of an updater. Given a folder
holding a new `horadric.exe` and `horadricw.exe`, it saves, runs `swap`
from that folder, moves the installed binaries aside, copies, rewrites the
hooks, starts `app --reload`, and rolls back if the new build is not up in
20 seconds. The sessions run on in their hosts throughout. So an update is:
download into a folder, verify, then post the same `Reload` request with
`exe` pointing into that folder. Nothing about the handover changes.

**The crypto, without a new dependency.** Minisign is Ed25519, and Windows
CNG has no Ed25519 signatures (its Curve25519 is key exchange only), so
minisign means adding `ed25519-dalek` and what it pulls in. CNG does have
ECDSA P-256 and SHA-256 (`BCryptVerifySignature`, `BCryptHash`), and
WinHTTP does HTTPS with redirects (GitHub's release links redirect to its
storage host). Both are in the `windows` crate behind feature flags. The
recommendation is P-256 through CNG and WinHTTP: no new dependency, and
the signature format is ours, so we do not need the minisign tool either.
What we lose is compatibility with `minisign` and `tauri signer`, which
nothing here uses.

**The release.** Three assets on a GitHub release of `Mopra/horadric.dev`
(public): `horadric.exe`, `horadricw.exe`, and `latest.json`:

```json
{ "version": "0.2.0", "notes": "...",
  "files": [ { "name": "horadric.exe", "sha256": "..." },
             { "name": "horadricw.exe", "sha256": "..." } ],
  "signature": "<base64 P-256 signature over the rest, canonical bytes>" }
```

One signature over the manifest, and a hash per file, so both binaries are
covered by one check and the version is inside what is signed (no replaying
an old manifest to downgrade). The URLs are derived from the tag, not
trusted from the file.

- **`horadric release keygen`** writes the private key to
  `%USERPROFILE%\.horadric\updater.key` and prints the public half, which
  goes into the source as a constant. A human backs the key up; that is not
  something an agent can do.
- **`horadric release sign <dir>`** hashes the two binaries in a release
  build folder, writes `latest.json` and signs it. Pure apart from the file
  reads and CNG, so the manifest bytes and the check are tested.
- **Publishing** is `gh release create v<version> --draft` with the three
  files, then a human publishes it after trying it. Local, not CI, for now:
  shipping already happens from this machine, and a key in a CI secret is
  one more place to guard. The workspace version is the release version
  and has to go up each release. 0.2.0 is the first. `RELEASING.md` has
  the steps.

**The check.** On start and once a day, and from a tray item "Check for
updates", fetch `latest.json`, verify the signature against the built in
public key, and compare its version with ours. A newer one shows in the tray
("Update to 0.2.0") and nowhere louder; a failed check (offline, a bad
signature) is logged, and said only when the check came from the tray.

**The install.** A click on "Update to 0.2.0" downloads both binaries into
`%LOCALAPPDATA%\Horadric\updates\0.2.0\`, checks each hash against the
verified manifest, and only then posts `Reload` with `exe` set to the
downloaded `horadric.exe`. Nothing downloaded is ever run before its hash
matches. From there it is `reload`: rollback included, sessions untouched.
The hosts need nothing, since the host binary is a copy of `horadric.exe`
per build already.

As built: only `horadric.exe` and `horadricw.exe` are fetched, whatever
else the manifest lists. The real ones come from
`releases/download/v<version>/`, by tag, so a release published mid
download can not mix two versions; a manifest from anywhere else has its
binaries beside it. Both are held and hashed in memory, and the version
folder is cleared and written only when both match, so nothing under
`updates` was ever not what the signed manifest names. A failure is said
in a notification and changes nothing.

**A dev instance** checks against `HORADRIC_UPDATE_URL` when set and
nothing otherwise, and never installs: `swap` under `HORADRIC_DEV` restarts
from its own folder and copies nothing, which would just restart the
downloaded build in place. Testing the install means the fake install
folder the reload test used (own `APPDATA`, `LOCALAPPDATA`, home and port)
with a local HTTP server serving a signed test release, signed by a test
key given through `HORADRIC_UPDATE_KEY` that only a debug build honours.
A debug build that is not a dev instance also honours
`HORADRIC_UPDATE_URL`, since the fake install is not one; a release build
never does. A dev instance downloads and verifies but does not reload.

Tested that way: a 0.1.0 debug build in the fake install, a 0.2.0 debug
build signed by a key made in the fake home and served by `python -m
http.server`. "Update to 0.2.0" fetched both, reloaded, and the install
folder held the 0.2.0 hashes. With the new build killed as it started,
`reload.log` said "rolled back" and the 0.1.0 build ran again. With one
served byte changed, both were fetched, nothing was written and nothing
reloaded. No session hosts, no `claude.exe`. The fake install's first
start pointed the real "Start with Windows" value at the fake folder
(it is in `HKCU`, shared with the installed Horadric), and it had to be
put back by hand. Now only a binary in `Programs\Horadric` under the real
`%LOCALAPPDATA%` (the known folder, not the variable a fake install sets)
turns it on at first start. A fake install test should still check that
value after.

**Not covered.** Authenticode. Downloads of the updater's own binaries do
not go through SmartScreen, so it is not needed to update; it is needed only
if people download Horadric by hand, the same point as NSIS above.

## Not in any step yet, but needed before daily use

- **Sessions die with Horadric.** The consoles live in the Horadric process, so
  quitting or crashing it ends every agent in a terminal. The options and a
  decision (A, then B) are under "Sessions that outlive Horadric" in
  Next.
- **Terminal gaps.** The kitty keyboard protocol is not implemented.
- **Expanding from a synthetic click can open behind other windows.** Windows
  only lets a process take the foreground after real input. A real click on
  a tile is real input, so this only bites scripted tests.
- **The tray menu is light in dark mode.** Win32 popup menus only follow
  the dark theme through undocumented `uxtheme` calls.
- **New tray icons start hidden.** Windows 11 puts them behind the `^`
  overflow until the user drags them out or turns them on in Settings.
- **No remote.** The repository exists on one disk. Push it somewhere.

## Open questions

Carried from the concept, with what is known now.

- **Does forty sessions hold up?** Five tiles cost 45 MB and no CPU. One
  open terminal adds a few MB. History is bounded at 10,000 rows a session,
  so the worst case for the grids is about 1.1 GB, reached only if all forty
  fill theirs. The other cost is forty
  `claude` processes, which is not ours. Not yet tried with forty.
- **Can a tile light up without stealing focus?** Yes, so far.
  `WS_EX_NOACTIVATE` plus `SWP_NOACTIVATE` holds through raising.
- **How does a session get named?** Claude Code writes its own title for
  the conversation into the transcript (`ai-title`, or `custom-title` after
  `/rename`). The listener reads the transcript's tail on `SessionStart`,
  `UserPromptSubmit` and `Stop` and the tile shows that title. A name given
  with `--name` beats Claude's title but not a `/rename`. The folder name is
  the fallback until the first turn ends.
- **What about the VS Code extension's Claude?** Still unanswered. Probably
  the answer is to stop using it.

## Name collision

`horadricapp/horadric` is a self-hosted dashboard with more than twenty thousand
stars on GitHub. The crate name, the binary name and any published package
need a decision before this goes public. Undecided.

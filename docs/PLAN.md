# Implementation plan

Where Horadric is, what comes next, and what was decided along the way. Update
this file when a step lands or a decision changes. It is the handover
document: someone picking the project up cold should need nothing else.

Last updated 2026-09-26, after step 3, the launchers, persistence, install,
the stage, reload, the project grid, browser windows, the look, plain
terminals, a pass of quality of life, the columns, the task list, session
hosts, drawing every last piece of chrome ourselves, identify, the
stash, and a performance pass.

Ideas that are not planned yet, most of them from the Diablo name, are in
[IDEAS.md](IDEAS.md).

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
- The tiles come up with the stage. Whenever the stage activates (its
  taskbar button, alt-tab, a click on it) every tile window is raised
  too (`Input::StageActive`), so Horadric comes forward as one app. Asked
  for on a laptop, where the tiles were only ever seen by closing every
  other window. Checked on screen: notepad over the tiles, the stage
  activated, the tiles in front of notepad.

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

### Launchers

Typing `horadric new --cwd` was too much friction for starting a session in a
new project, so three ways in that need no terminal:

- **Tray icon.** Always there, even with no tiles. Its menu has "New
  session..." (the folder picker, opening in the last project), up to eight
  recent projects one click away, and Quit, which warns when sessions in
  Horadric terminals would end. Recent projects live in
  `%APPDATA%\Horadric\recent.json`. The tooltip counts sessions and waiting
  ones. The icon is drawn in code (`icon.rs`), so there is no file to ship:
  a dark cube with its lid lifted and light pouring out. One colour lights
  it (gold, red for a dev instance), and `icon::lit` takes any other, so
  the light can later follow what Horadric is doing.
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
- A file that does not read whole is copied aside as
  `state.json.bad-<secs>` before the next save overwrites it, and only
  what is bad is dropped: one session, one list item, one map entry, one
  field. A file that is not JSON, or from a newer Horadric, is set aside
  and reads as empty.
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
- **A project stays open with no session left.** Ending every session is
  often a fresh start in the same project, and the cluster going with the
  last one meant opening the project again. So a cluster stays, with its
  bottom `+`, until Close project in the project menu, which ends its
  sessions (asking first, as End all does) and takes it down. The open
  projects are the `clusters` in `state.json`, so they come back after a
  restart. One with unfinished tasks stays up for them, as before. Tested
  on screen with a dev instance: the last session exited, the cluster
  stayed and came back after a restart, and Close project brought back
  the start window.
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
- **A project closed** has no cluster and so no project menu. Two more
  ways in: History in the tray menu, with a submenu for each
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

**Folded into the quest log window (2026-10-03).** The History submenus
are gone from the project menu, the tray and the start window. The
conversations they listed are rows on the quest log's main line (see The
quest log window), and the three menus offer "Quest log..." for the
project instead: the tray a "Quest log" submenu of the recent projects,
the start window's right click New session and Quest log.

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
- A red lit tray icon and "Horadric dev" in the tooltip.
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
- **Headers.** Every pane has a header: the session name and what the
  agent says it is doing, with a line of the phase's colour along its
  top. No dot before the name: every app has one, and the line already
  says it. The one with the keyboard is underlined in the project's
  colour and the others step back. At its right end sit stash, zoom and
  a cross that ends the session (asking first while an agent runs). A
  pane alone has one too, for those buttons, but no zoom.
- **Drag to swap, live.** Pressing a header gives the stage the mouse
  (`WM_PANE_GRAB`, then `SetCapture` on the stage). Past 4 pixels the pane
  lifts and follows the cursor, and the pane whose cell it comes over
  glides into the cell it left, so the grid shows the swap before the
  mouse is let go. Letting go glides the dragged pane into its cell, its
  centre kept, and the swap goes through the app (`Input::Swap`), which
  owns the order. The stage swaps its own panes at once, so the app's
  `show` of the new order changes nothing.
- **What keeps a drag cheap.** Every grid is held at its size
  (`Pane::hold`) until the drag ends, so a pane passing through a cell of
  another size never reaches its agent as a resize and a redraw. The
  lifted pane floats as a window of its own, owned by the stage
  (`Pane::float`): a child moved across the stage damages the stage and
  every pane it passes over, a popup is only moved by the compositor.
  The pane is carried on the vsync clock, not on each mouse move, so a
  1000 Hz mouse still moves it once a frame, and it is kept on the stage.
  A pane's render target keeps its last frame, and a paint Windows asks
  for when nothing shown has changed (a neighbour uncovered a strip) only
  presents it again, about half the cost of drawing. Measured on a 2x2
  grid in a release build, a drag costs about 2 ms of CPU a frame; with
  the pane left a child window it cost twice that.
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
cut at the edge and scrolled sideways. Asked for so a file an
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
- **Sideways.** Long lines are not wrapped: wrapped code loses the shape
  its indentation gives it, which is what VS Code keeps too. Shift+wheel
  and a horizontal wheel or touchpad scroll it sideways, six columns a
  notch, never past where the longest line ends. The line numbers and the
  changed line marks stay put. Each step lays the file out again from the
  new column (`viewer::render`, one row per line), as a resize does.
- **Keys.** Up and Down, Page Up and Down, Ctrl+Home and Ctrl+End scroll.
  Left and Right scroll sideways four columns, Home and End to either
  side. Ctrl+C copies, and a drag copies on release, as in a session.
  Copying leaves out the line numbers (`viewer::copy` maps grid cells to
  file bytes), and a selection from the gutter or to the right edge takes
  what is off screen on that side of its line too. Esc or the cross at the
  end of its header closes it. Nothing typed reaches anything.
- **Live.** When the project's watcher rescans, the view reads its file
  again if its size or time changed, keeping the same line at the top. So
  an agent's edit shows within a second or so. A resize lays the file out
  again for the new width, also keeping the top line.
- **Limits.** Files over 4 MB are not read, binary files (a NUL in the
  first 8000 bytes) are not shown, and past 10,000 lines the rest is left
  out and counted. Each line is a grid row a full width of cells, so that
  limit is what bounds memory, about 30 MB at 120 columns.
- **Changed lines.** Between a line's number and its text, a bar in VS
  Code's gutter colours says how it differs from the last commit: green
  for added, blue for changed, and a red underline on the line a removal
  follows. It comes from the hunk headers of `git diff -U0 HEAD` for the
  one file (`viewer::marks`, pure), run on the colouring thread. Every
  rescan asks again even when the file did not change, so a commit clears
  the marks. An untracked file, or one outside a repository, has none.
- **Search.** Ctrl+Shift+F opens the terminal's search bar, but a view
  searches its file, not its grid (`viewer::find` and `viewer::cells`), so
  a line number never matches. A match off to a side scrolls the view to
  put it in the middle, and scrolling moves its mark with the text. It
  starts at the top line on screen, and Enter and F3 go down the file, as
  in an editor, where a terminal goes up its history. Shift goes back, and
  both wrap round. Case is ignored unless the query has a capital.

Tested on screen with a dev instance on this repo: `main.rs` opened beside
a `cmd.exe` pane with the colours right, the wheel scrolled it, a click on
another file replaced it, an edit to the shown file appeared within a few
seconds, a drag across the gutter copied only the text, and Esc and the
cross both closed it with the keyboard back in the session. The changed
lines and the search were checked on a scratch repository with a line
changed, two removed and two added: each mark in its place, Enter stepping
to the next match and round to the first, "12" (only a line number there)
not found, and the marks gone a few seconds after a commit. Sideways
scrolling was checked on a scratch file of 150 column lines: Shift+wheel
moved the text 30 columns in five notches with the numbers and the two
marks in place, a horizontal wheel brought it back 12, a search for a word
at column 200 scrolled over to it, and a Shift+wheel notch after that moved
its mark with it.

### Browser windows

An agent testing a web page starts a browser of its own, through Playwright
or the Chrome DevTools MCP. Its window used to land anywhere, with nothing
saying which session opened it. Embedding a browser in Horadric was
considered and dropped at first: the agent drives its browser over a debug
protocol and never needs a window Horadric owns. So Horadric manages the
browser's window instead. The browser pane below came later, for the
user's own browsing and for pages user and agent look at together.

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

### Browser pane

Asked for because a browser beside the stage never fitted with the
sessions and tiles, and because the agent should see the page the user
means and drive it, instead of being pasted a screenshot. A first go
managed an Edge window with a profile per project, which meant logging in
again for every project; it was dropped the same day. This reverses the
settled "no web view" for web pages only: Horadric's own UI stays Direct2D.

- **A pane on the stage** (`web.rs`, `Console::web`, `App::webs`), one per
  project, after its sessions and the file view in the grid. Ctrl+Shift+B
  in a pane (`CharAction::Browse`, plain Ctrl+B stays the program's) or
  "Browser" in the project menu opens it and, when new, asks for an
  address (`web::address`, pure: a scheme kept, a local server over http,
  a host over https, anything else a search). The header is an address
  bar, as in any browser (`glyphs::bar_layout`, pure): back and forward,
  dim with nowhere to go (WebView2's `HistoryChanged`), reload, then the
  address in a field up to the cross. A click or Ctrl+L puts the keyboard
  in the field with the address selected (`Pane::address`, a
  `field::Field`); Enter goes, Esc or a click anywhere else leaves it. A
  page that takes the keyboard as it first loads, with no click to send
  it there, gives it back (`Pane::page_took`). With no pane on the stage,
  Go to still asks in a prompt. A right click on the header has Go to,
  Back, Forward, Reload, Open in your browser and Close. Ctrl+Shift+T in
  the page still opens a terminal: the page has the
  keyboard, so those two are caught by `AcceleratorKeyPressed` first.
- **WebView2**, through `webview2-com`. It is Edge's engine, part of
  Windows 11. The loader is linked statically, so there is no DLL to ship.
  A controller is up about 250 ms after it is asked for, so it is made
  asynchronously: the pane shows at once and the page arrives in it.
  Nothing waits on WebView2 in a handler, and no borrow is held across a
  call into it, since a call can raise an event that comes back.
- **The page outlives its pane.** The stage destroys its panes whenever it
  shows another project, and a destroyed WebView loses its page. So the
  controller is `web`'s: a pane lends it a window while it shows it
  (`attach`, the pane has `WS_CLIPCHILDREN` so its own drawing leaves the
  page alone), and on the pane's `WM_DESTROY` it goes, hidden, onto the
  app's window (`detach`). Closing the stage keeps it too. Only its cross
  or Close ends it.
- **Kept over a reload** (`SavedState::pages`, `web::pages`,
  `web::restore`). Every ship dropped the open pages, so each project's
  tabs are saved with their address, title and the one shown, and a
  start that resumes sessions (a reload, or after a crash) opens them
  again in their project's grid, off the stage like an agent's open. A
  blank tab is not kept (`SavedPages::of`, pure). After Quit they are
  not reopened, as sessions are not resumed. Only the address: WebView2
  has no way to give a page its back and forward history again. Tested
  with a dev instance on its own port and `APPDATA`: two saved tabs came
  back after `app --reload`, and again after `horadric reload`, the new
  process writing them, titles and the shown tab as they were.
- **One profile for everything**, `%LOCALAPPDATA%\Horadric\web`, for every
  project, session and Horadric, dev instances included, so a login made
  once stays. WebView2 lets two processes share a profile when they start
  it with the same arguments, which is why the DevTools port below is
  chosen once and kept in the profile folder rather than per instance.
- **Agents drive it** (`horadric mcp`, `drive.rs`). Every Claude Code
  session gets `--mcp-config=` a file naming `horadric mcp` (Codex gets
  `-c mcp_servers.horadric...`; Grok gets a `[mcp_servers.horadric]`
  table in `~/.grok/config.toml`, see below). The server finds its
  session by the `HORADRIC_SESSION` it inherits and posts each call to
  `/horadric/browser`, the one listener path that waits for the app's answer. The session names the project, so
  an agent only reaches its own project's page. The app does open, close,
  navigate, back, forward, reload, info, and any DevTools call on that
  page through WebView2's `CallDevToolsProtocolMethod`, no port needed.
  Every tool is made of those: `browser_snapshot` is an outline of the
  page with a ref per link, button and field, a click is a script that
  finds the element and scrolls it to the middle, then a real mouse press
  and release there; typing is `Input.insertText`; screenshots
  `Page.captureScreenshot`; the console is kept from the first page on by
  a script added at document creation. An agent's open never takes the
  stage or the keyboard: the page joins its project's grid. A hidden page
  draws nothing, and a screenshot waits for it to draw (90 s, then gave
  up), so a page off the stage is made visible on the app's hidden window
  while a call is in flight, at 1280 × 800 if it was never shown.
  Tested: a script through every tool against a test page and
  example.com, and a Haiku session told to fill in a form, which opened,
  typed, ticked, clicked, looked and closed by itself.
- **Grok's way in** (2026-10-01). Grok Build 1.0.44 has no per session
  way to be given a server: the TUI has no `--mcp-config` or
  `--plugin-dir` (only `grok agent` has the latter), its `GROK_CONFIG`
  overlay keeps only soft settings and drops `mcp_servers` and
  `plugins`, a plugin in `~/.grok/plugins` is put on the disabled list
  when first seen, and `--agent` swaps the whole system prompt. So
  `install` and `reload` write a `[mcp_servers.horadric]` table into
  `~/.grok/config.toml`, beside the hook file, and `uninstall` takes it
  out (`with_grok_mcp`, pure): the only part of that file Horadric
  touches, the dev instance never. Grok passes a stdio server its own
  environment, so the session's tag arrives. Every `grok` starts the
  server, so without a `HORADRIC_SESSION` it offers no tools and no
  instructions. Live check: the table pointing at a debug build, a dev
  instance on its own port, `horadric new --agent grok` told to open
  example.com; Grok found `horadric__browser_open` by `search_tool`, the
  pane for its project opened there, and `browser_snapshot` read it.
  `grok mcp doctor` from a shell outside Horadric shows 0 tools.
- The browser still listens for the DevTools protocol on 127.0.0.1, on
  the port in `web\devtools-port`, for tools outside Horadric. While it
  is open, any program on the machine can drive that logged in browser.
  Loopback only is the whole of the protection so far. Next is letting
  the user point at part of the page and send it to a session with a
  screenshot, the address and the element.
- **Tabs** (`web::Web::tabs`, `glyphs::tab_layout`, pure). Asked for so a
  second page does not mean losing the first. One pane per project still,
  with a strip of tabs along the top of its glass and the page below it;
  each tab is a WebView of its own, only the chosen one visible, so a tab
  keeps its page, history and scroll while another is shown. The `+` at
  the strip's end, Ctrl+T or New tab in the menu opens one and puts the
  keyboard in the address field. A tab's cross, a middle click on it,
  Ctrl+W or Close tab closes it, and closing the last closes the pane.
  Ctrl+Tab and Ctrl+Shift+Tab, Ctrl+PgDn and Ctrl+PgUp, and Ctrl+1 to 9
  pick one, as in a browser (`web::tab_key`, pure), caught in the page by
  `AcceleratorKeyPressed` and in the pane while the address is typed. A
  tab is named by its page's title, else its address (`web::tab_name`).
  An address Ctrl+clicked in a terminal opens in a new tab. An agent's
  tools drive the shown tab. The tabs are kept over a reload, see above.
- Popups and links to a new window (a middle click, `target=_blank`, an
  OAuth login) open in a new tab: `NewWindowRequested` is deferred until
  the tab's WebView is made and then handed it, so the popup keeps its
  opener, and a page closing itself (`WindowCloseRequested`) closes its
  tab.
- **A size of its own** (`viewport.rs`, pure). Fitted, the page is the
  glass and changes size with the grid. Sized, from the size button after
  reload or Page size in the header's menu (Phone, Tablet, Laptop,
  Desktop, Resize by hand, or typed), it lays out at that many CSS pixels
  whatever the grid does, centred in the glass with its size under it.
  Where the pane is too small it is scaled down with WebView2's zoom
  factor, which divides the page's bounds to get the CSS viewport, so it
  still lays out at its own size; the zoom is worked out from the rounded
  pixel bounds so the width comes out exact. Grips just outside its right
  and bottom edges and at the corner resize it. Ctrl and the wheel stop
  zooming a sized page, since that would change the size it lays out at.
  Kept per project in the saved state (`page_sizes`). Only the size is
  kept: the pane still moves between cells as sessions come and go.
  Checked over DevTools: Phone gave `innerWidth` 390, a drag 680 × 497,
  Desktop scaled to 54% still 1920 × 1080, and Fit the whole glass.
- **Its place beside the grid** (`layout::docked_grid`, pure). Three
  buttons after the size button, drawn since the icon font has no dock
  on top, stand the browser pane on the left, on top or on the right of
  the stage, the whole of that side, with the sessions in a grid of their
  own beside it. So it keeps its place and size as sessions come and go.
  The lit one says where it is, and a click on it puts it back in the
  grid, as before. The size menu has the same four. The gap beside it is
  a seam: drag it to change its width, or height on top, kept per project
  with its side (`page_docks`, which still reads the width the first
  build wrote, as the right). It starts half and half with the grid, and
  stays half as the stage resizes until the seam is dragged (640 wide
  was too narrow to work in). It keeps its width going from left to
  right, and each side keeps at least 320. Tested with five `cmd.exe`
  sessions: left, top and right each put the browser along that whole
  side with the grid beside it, the top seam dragged 150 pixels made it
  that much taller, and the lit button put it back in the grid.

Tested on screen with a dev instance on its own port and `cmd.exe`
sessions in two projects: Ctrl+Shift+B put the pane in the grid and asked
for an address, `example.com` loaded with its title in the header, a
DevTools `Page.navigate` from a script moved it, and a value set in the
page over DevTools was still there after the stage switched to the other
project and back, so the page was parked and not reloaded. Two processes
started the WebView on one profile at once and shared it.

Tabs tested on screen with a dev instance on its own port and `APPDATA`
and a `cmd.exe` session: the strip showed the page's title, `+` and Ctrl+T
in the page opened a tab with the keyboard in its address field, Ctrl+L
and an address loaded it and named the tab, a click on the first tab
brought its page and address back, a middle click on a link opened it in
a tab of its own, a middle click on a tab and Ctrl+W closed tabs, and
Ctrl+W on the last closed the pane. Found on the way: the hidden grid's
focused block cursor, a fill rather than a caret, showed through the
strip, so a browser pane now draws none of its grid.

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

With Codex or Grok Build in use too, it has a screen per provider, see
step 6 of "Codex and Grok Build beside Claude Code".

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

### Accounts

Asked for: several Claude Max subscriptions, logged in to all of them, and
a switch between them without logging in again, while the sessions run.

- **What a login is.** Claude Code keeps one. On Windows the token is in
  `.credentials.json` under `claudeAiOauth`, beside the MCP servers'
  tokens, and whose it is sits in `.claude.json` under `oauthAccount`.
  Nothing else cares who is logged in: hooks, settings and conversations
  are shared. So an account is that pair (`horadric_core::accounts`), and
  a switch writes another pair in and leaves the rest of both files as it
  was. `CLAUDE_CONFIG_DIR` per account was the other way, turned down: each
  account would need the hooks, and a conversation could not be resumed on
  another account.
- **Kept logins** are in `accounts.dat` beside `state.json`, sealed with
  DPAPI for this Windows user. Claude Code's own file is plain text, so
  this is no weaker than what it copies.
- **Watched, not asked.** The app reads the login at start and whenever
  either file changes and has held still for a tick, since a login writes
  them one after the other. A pair whose organizations disagree is a login
  caught half written and is not kept, or the old account would get the
  new one's token. A refreshed token replaces the kept one, and a `/login`
  typed into any session adds its account.
- **The Account row** sits under the settings in the usage window and
  names the account in use. A click lists every account kept, with its
  plan ("Max 20x"), Add account, and Forget for the others. Add account
  opens `claude auth login` in a console of its own (a login, not a
  session, so no pane), and once the new login lands the old one goes back,
  so adding stops nothing. The new one is a pick away.
- **The Version row** is the last one: this build's version, or "Update
  to <version>" once a check found a release. A click shows that
  release's notes with Update now, or checks for one and says what it
  found. No chevron, since it drops no list.
- **Switching** is the hot part. A running `claude` holds its token in
  memory, so every agent Horadric runs stops and resumes (`claude --resume
  <id>`), which keeps its conversation. Each stops once
  `Session::free_to_restart`: not mid turn, and nothing typed since its
  last prompt, which could be a draft. A question it waits on comes back
  with the resume. The files change only once every one has stopped: one
  left on the old login would write its token back when it refreshes it.
  Then all resume together, and the pane that had the keyboard gets it
  back. While it waits, the list says for how many turns, and Switch now
  stops them at once.
- **Written over.** Any `claude` that read `.claude.json` before the switch
  can write it back after with the old profile, Horadric's own `claude
  agents` included. For 15 seconds after a switch the login is put back if
  it is found changed.
- **Limits are per account.** The ones shown go with the account leaving
  and the arriving account's come back, so the window never shows one
  account's numbers under another's name.
- **A dev instance** switches only a Claude Code of its own, with
  `CLAUDE_CONFIG_DIR` set, never the real login.

Tested with a dev instance on its own port and `APPDATA`, a fake
`CLAUDE_CONFIG_DIR` holding two made-up logins, and a `claude.cmd`
standing in for the agent: the row named the account, a login written into
the files from outside was picked up, and picking the other account
stopped both sessions and resumed them on it, one process each, the
conversation of one resumed by id, the MCP tokens left alone. A session
made to work by a faked `UserPromptSubmit` held the switch after the idle
one had stopped, the list said "after 1 turn", and its `Stop` finished it.
Add account opened `claude auth login` and its browser page. Not tested:
a real second subscription, which needs its owner to log in. Two things to
watch on that first real switch: that `claude auth login` does not revoke
the login it replaces, and that a resumed session really runs on the new
account (`/status`).

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
- **Nothing jumps** (`glide.rs`, pure and tested; `appear.rs`). Asked for
  on 2026-09-26 as the small niceties that make the app feel good to use.
  A cluster glides to its place in the columns: let go of after a drag,
  pushed down by one above that grew, turned by the wheel. Its first
  place, one from off screen and a screen change still jump. On the stage
  a new session's pane fades in, and the others glide to their new cells,
  as do swapped and zoomed panes; a pane takes its new size at once, since
  a terminal resized every frame redraws its agent every frame, and an
  edge dragged by hand places them at once. A glide stops when something
  else moves the window, so a drag takes hold of a gliding cluster. Menus
  fade in over 90 ms, dialogs and the picker over 160 ms while rising 8
  DIPs into place, by `WS_EX_LAYERED` alpha as the toasts do. Checked on
  screen with a dev instance: the clusters below a growing one, a
  project menu, the End sessions dialog, and panes of five `cmd.exe`
  sessions travelling to their cells (logged, since the stage was behind
  the installed one).
- **Life in the tiles** (`anim.rs`, `motion.rs`, pure and tested). Asked
  for the same day as "more fun or cool animations", and worked as the
  Animations section of the task list. All of it holds still with
  Windows' animations off, and none of it asks for a frame at rest.
  - *A finished turn lands*: its key jumps and bounces once, and a loot
    beam of the lamp's green shoots up over the tiles above and fades in
    a second ("Loot drops" in IDEAS.md).
  - *Loot is heard* (`loot.rs`, pure and tested; `sound.rs`): a soft
    thump and a rising glint with the beam, and a high two note bell, E6
    then B6, when a session's work lands on `main`, merged from its menu or
    found there by git. Made from sine waves into a WAV in memory and
    played by `PlaySound`, so they are our own and need no crate or file.
    Off until "Loot sounds" in the tray turns them on, which plays the drop
    so the choice is heard. Silent in a full screen game, a presentation,
    quiet time, and under focus assist or do not disturb, which
    `ToastNotificationManager::NotificationMode` says.
  - *Waiting quickens*: the breath goes from 1.8 s to 1.4, 1.1 and 0.9 s
    at 1, 5 and 15 minutes, counted across the steps so it never jumps.
  - *Every phase change eases* the key's depth, presence and lamp from the
    old phase's to the new over 450 ms (`Stance`), so a resumed tile rises
    out of its pause and its lamp warms up.
  - *Ending powers down*: the lamp squeezes to a white line and a dot and
    goes out like a picture tube. A tile that leaves stays a moment as a
    ghost (`Leaving`), sinking and fading while the ones below slide up
    over it. Not for the last tile in a cluster: the cluster shrinks at
    once and its bottom row covers the ghost.
  - *Subagents are sparks* circling the working lamp, one for each seen
    in the turn within the last 30 s. Hooks fired inside a subagent carry
    its `agent_id`, so no hook had to be added to anyone's settings; no
    hook says a subagent ended, hence the 30 s (`Session::subagents`).
  - *The icon turns over* like a card when the tool changes, 280 ms.
  - *The activity trace scrolls*: slices stand still on the clock and the
    bars drift left as the newest fills, the oldest fading
    (`Session::trace`).
  - *The context bar fills like a liquid*, rising to each reading with
    the last segment lit only as far as the level reaches, and a glint
    runs along it on a working session past 75 %.
  - *A finished task* keeps its row a moment: a green check, a strike
    through the title, then the row folds and the rows below close up
    (`Finishing`).
  - *A taken task throws a light* from its row to the lamp of the new
    session that took it, once, when a session less than 3 s old turns
    out to hold an item (`Handoffs`).
  - *A busy project glows*: its accent washes deeper from the cluster's
    top and breathes over 4 s while any of its sessions works.
  - *A new project's cluster* rises 14 DIPs into its column and fades in
    over 300 ms, through `appear`. The windows there at start stand at
    once.
  - *A dragged cluster gets room*: the others in the column under the
    cursor glide apart for it, laid out as the drop would lay them but
    saved only on the drop. Only within a column already there with
    others in it, since a new or emptied column moves the columns under
    the cursor.

  Checked on screen with a dev instance and fake sessions posted to its
  port, captured by `PrintWindow` since the desktop was in use: the beam,
  the power down and the ghost, three sparks, the icon turning, the
  context bar rising to 88 %, a struck task folding, the light from a
  taken row. A new cluster's arrival was read back from its window's
  alpha and position. A real drag of one cluster up over another in its
  column: the other glided down while the button was held, and the drop
  landed in the gap.
- **How it ended, in item colours** ("Item rarity colours" in
  IDEAS.md; `rarity.rs`, `theme::rarity_color`, pure and tested). A
  session's name is printed in Diablo's colours, the third job colour has
  and only on the name, never lit: white changed nothing, blue changed
  files, yellow changed files and the tests passed after the last change,
  green one of a batch the runner held at once that changed something,
  gold what it committed is in what the main tree has checked out. Gold
  beats green, since landing is what a batch is for. All from the tool
  hooks (`Session::loot`, saved): `Edit`, `Write` and friends change
  files, and a shell command that runs a test suite or a `git commit` or
  `git merge` is read from `tool_input.command`, its outcome from
  `PostToolUse` or `PostToolUseFailure`. A worktree's diff counts as a
  change too. Whether it landed is asked of git (`merge-base
  --is-ancestor`) with the diff recount once it committed, and a merge
  from the project menu gilds its sessions at once, since the sweep may
  take the worktree before anyone asks. Each colour leans off its nearest
  lamp and is paler, so a yellow name does not read as waiting. Checked on
  screen with a dev instance and fake sessions in a scratch repository:
  each colour, a worktree session turning gold when its branch was merged,
  and green and gold coming back from the saved state on paused tiles.
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

Panes are rounded too, not only the glass inside them. A pane is still a
square child window, but a groove cut round it with corners of the glass's
radius plus the bezel shows its edge, and outside the groove its plate runs
on into the stage's, so the corners read as round. No window region: a
region's edge is aliased, and the plate at the corners is the same either
way. A lifted header is rounded with it. Checked on
screen with `cmd.exe` panes on a dark glass and on a light one (a program
setting the background by OSC 11): the corners run parallel to the
glass's, and nothing clips the text. The drop and lift highlights were not
seen, since scripted mouse input did not reach the stage. The tray menu and
the stage's title bar were left here and are drawn now, see "No Windows
chrome" below.

### No Windows chrome

Decided 2026-09-26: everything the app shows is drawn in its own look.
Menus, questions, warnings, errors, notifications, the folder picker and
the stage's title bar all were Windows' own and read as a different app.
Asked for as "no default Windows crap, make it premium". The one piece of
Windows left is Explorer's folder dialog, a click away behind Browse, for
a folder easier found by looking.

- **Menus** (`menu.rs`): the tray's, a tile's, a project's, a task's and
  the stage's window menu, through `menu::popup` in place of
  `TrackPopupMenu`, with the same `Item`s. On a plate like a setting's
  list: a lamp by a checked line, the text after a tab right aligned and
  faint (a shortcut, a place, an age), a chevron on a submenu, grooves
  between groups. The first window takes the focus and the mouse, a
  submenu opens beside its line after the mouse rests 220 ms and takes
  neither, and every mouse message is sorted by where it lands on screen.
  The arrows, Home, End, Enter, Right and Left, Esc and a typed first
  letter work; a menu taller than the screen scrolls with the wheel and
  the keys. A button coming up before one went down on the menu is the
  end of the click that opened it and picks nothing. Labels are plain
  text now, no `&&`. Layout, placement, flipping at the screen's edges,
  submenus and the keyboard's steps are pure and tested.
- **Dialogs** (`dialog.rs`): Quit, ending sessions, merging a finished
  task, and `horadricw`'s startup error, in place of message boxes. A lamp
  by the title says what kind (a question in blue, a warning in amber, an
  error in red), the text wraps, and each answer is a key named for what
  it does: Keep running, Stop them, Cancel; End, Cancel; Merge, Not now.
  Enter presses the ringed key, the arrows and Tab move the ring, Esc or a
  click elsewhere answers nothing. Centred a little high on the screen
  under the mouse. `horadricw` has no app, so `error_alone` makes its own
  Direct2D and sets per monitor DPI first. A `horadricw` started by a
  script is not allowed the foreground, so its dialog does not get the
  keyboard; from Explorer it does.
- **Notifications** (`toast.rs`): in place of the tray's balloons, which
  Windows showed as its own toasts. In the bottom right corner of the
  primary screen's work area, over the tray. A lamp in what it is about
  (waiting amber, done green, news blue, failed red), fading in and out,
  standing 7 s not counting while the mouse is on it, a cross while it is.
  One at a time and a new one takes its place, since a click acts on what
  was said last: it comes back to the app as the balloon's
  `NIN_BALLOONUSERCLICK` did, so both paths are one. Held back only for a
  full screen game or presentation mode. `SHQueryUserNotificationState`
  also says busy for any window the size of the screen, which the stage
  often is, so that one is ignored or they would never show. Lost against
  Windows' toasts: the notification centre keeps no history, and Focus
  Assist is not heard.
- **The folder picker**: the question input (`ask.rs`) with a list under
  its field, through `ask::Pick`. Empty, it offers the recent projects;
  before a separator, the recent ones whose names hold what is typed;
  after one, the folders it could be completing to, hidden ones left out
  (`paths.rs`, pure and tested, with `~` for the home folder). Up and Down
  pick, Tab fills one in with a separator after it so its folders come
  next, Enter or a click starts there, and a folder that does not exist
  is refused in red where the hint was. Browse opens Explorer's dialog in
  what was typed, if it is a folder.
- **The stage's caption** (`caption.rs`): `WM_NCCALCSIZE` gives the client
  the top of the frame, and a child window paints the caption there with
  the plate's light and seam carried on, the project's lamp and name, the
  session and what its agent is doing, and minimise, maximise and close
  keys, close lit red. The child answers `HTTRANSPARENT`, so the stage
  hit tests: `HTCAPTION` to drag and double click, the frame's thickness
  along the top as `HTTOP`, and the keys as `HTMINBUTTON`, `HTMAXBUTTON`
  and `HTCLOSE`, which keeps Windows 11's snap layouts over maximise. The
  keys are pressed and let go on the non client messages, since
  `DefWindowProc` would draw the old ones over them. A right click on the
  caption and Alt+Space open the window menu, drawn. Maximised, the client
  starts where the screen does. Behind other windows the caption goes
  quiet.

Tested on screen with a dev instance on its own port and `APPDATA`, with
`cmd.exe` sessions: a tile's menu, the project menu and its History
submenu by mouse and keys; the End sessions dialog with Esc keeping the
session; `horadricw`'s error from a copy with no `horadric.exe` beside it;
a waiting toast from a posted hook, held under the mouse, faded after,
and a click on it bringing the stage forward; the picker completing
through three folders, refusing one that is not there and starting a
session in the scratch folder; the caption active and behind, maximised
and restored by its keys, close lit, the window menu. Not tried on
screen: the tray's own menu (its icon sat in the overflow), the Quit
dialog, a dragged or top-resized stage, and snap layouts.

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
- **When the runner stops.** At a blocked item that waits on the human
  (one that names a wait it can check is passed, see "Waits a blocked
  quest can name" below), since the order is the
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
any other; its catch-up line has a mark of its own (see below).

### Identify

In Diablo an unidentified item drops grey until you identify it. A turn
that finished while you looked elsewhere is the same: you do not know what
it did until you look. Before this a finished tile stayed green until its
next prompt, so ten green tiles said nothing about which ones you had read.

- **Unread** (`Session::unseen`, `Session::unread`, pure and tested). Set
  when the phase becomes done, cleared by the next phase or when you look.
  Only a change into done sets it, so a second `Stop` on a turn you have
  read changes nothing. Not saved, like the phase it belongs to.
- **Looking** is the session's pane having the keyboard with the stage in
  front (`App::identify`), the same session whose key is latched in. Just
  being in the grid is not enough: the panes without the keyboard step back
  behind a veil, and you may be typing into the one beside it. Checked when
  the keyboard moves (`Input::Spotlight`), after hook events, and every
  second, since the stage coming to the front tells the app nothing.
- **On the tile** an unread turn keeps the green lamp it has now. Once
  identified the lamp goes dark, the check mark dims and the key stands back
  like an idle one (`render.rs`, drawn as idle). So a lit lamp still always
  means something for you, which is the rule of "The look".

Tested on screen with a dev instance on its own port and `APPDATA`, two
`cmd.exe` sessions given a fake prompt and `Stop` each: with the stage
behind other windows both lamps stayed green; with the stage in front the
one with the keyboard went dark within a second and the other stayed lit;
Ctrl+Alt+Left moved the keyboard over and that one went dark too.

### Stay a while and listen

Deckard Cain's catch-up. Asked for after the task list ran by itself for
more than six hours: coming back, the only way to know what had happened
was to read every terminal. The tiles say what is true now, not what
happened while you were gone.

- **When.** On coming back after being away: no real input for 15 minutes
  (`GetLastInputInfo`) or the screen locked (`WTSRegisterSessionNotification`),
  then input again, and only if something happened meanwhile. Also on
  demand from the tray menu and a hotkey, for "since this morning".
- **What it says,** in the order you act on it: sessions waiting on you,
  oldest first; task items ready for review; turns that finished unread
  (see Identify above); items blocked, with the reason; then what simply
  happened: items the runner finished and their commits, branches merged,
  sessions that ended, a usage limit that ran out and when it reset. One
  line each, grouped by project, and a click on a line shows that session
  the way a tile click does.
- **A journal** is what makes it possible, since the registry only knows
  the present. An append only `journal.jsonl` beside `state.json`: a line
  per thing worth telling (a phase into waiting, done or ended, a task
  mark, a runner start, a merge, a limit), with the time, the session and
  its project. Trimmed to a week. `horadric_core::journal` reads it and
  turns the lines since a time into the summary; that is pure and tested.
- **Drawn like the rest,** a panel on the plate in the look of the toasts
  and the picker, not a window of Windows'. Dismissed with Esc or a click
  outside, and it marks nothing read by itself: identifying is still
  looking.
- **Later.** A sentence per session on what it did, not just its last
  message. The last assistant message and the commits say a lot already;
  asking a small model to summarise a run is a step after that, and a
  cost to justify.

Built:

- **The journal** is `journal.jsonl` beside `state.json`
  (`store::journal`, `horadric_core::journal::Entry`). The app writes a
  line when a session's phase changes into waiting, done or ended (a
  session that vanishes counts as ended), compared in `reconcile` against
  the phases last written (`App::journaled`); a session first seen at a
  start or a reload writes nothing. A task list read that changes an
  item's mark writes started, review, blocked or finished
  (`journal::marks`, matched by title). A merge from the menu or the
  notification writes merged, and a usage limit that holds the runner
  writes once per reset. Lines older than a week go at each start.
- **The summary** (`journal::summary`, pure and tested) keeps only the
  last word on each session and each item, so waited, answered and
  finished is one line. What asks something of you is told only while it
  still holds, which the app answers from the registry and the lists.
  Lines go waiting, review, unread, blocked, then what happened, oldest
  first in each, grouped by project, the project with the most pressing
  line first. The account's lines (a limit) stand under "Account".
- **Away** (`catchup::Away`, tested): 15 minutes without input by
  `GetLastInputInfo`, looked at every second, or the screen locked
  (`WTSRegisterSessionNotification`); input within 5 seconds, or
  unlocking, is coming back. Input on the lock screen is not. Coming back
  opens the panel only if something happened, and not while Windows asks
  for quiet (a full screen game, a presentation), as the toasts do.
- **On demand** from the tray's "Stay a while and listen" and
  Ctrl+Alt+Home (Ctrl+Alt+Shift+Home for a dev instance): since local
  midnight, or the last eight hours early in the day. Asked for, it says
  "Nothing happened" rather than staying shut.
- **The panel** (`catchup.rs`, laid out by `layout::catchup`, drawn by
  `render::catchup`) is a plate like the toasts in the middle of the
  primary screen, a lamp by each line in its section's colour and dark
  for what simply happened, the age at the right. As many lines as fit
  the screen, then "and N more". It takes the focus like a setting's
  list; Esc, the cross or a click outside closes it. Windows may refuse
  the focus to an app you are not using, and then it closes when the
  focus moves anywhere. A click on a line with a live session shows it
  as a tile click does.

Tested on screen with a dev instance on its own port and `APPDATA`, a
journal seeded with a finished item, a merge, a limit and a line ten
days old, and three fake sessions posted to it: the old line was trimmed
at start; a permission wait, a `Stop` and a `SessionEnd` each wrote their
line; the hotkey opened the panel with the waiting session first, the
unread turn, the ended one, then the other project's finished item and
merge, and the limit under Account. Not tried on screen: coming back
after 15 minutes or an unlock, clicking a line, and the tray item.

- **Commits under a finished item** are asked of git when the list says
  it is done, not heard as they happen: `git log --first-parent
  --no-merges --since` the item's last started line, in the tree of the
  session that held it (its worktree, or the main tree when it is gone).
  First parent only, since a merge of main into the branch would bring
  every commit main had meanwhile. They go on the finished line
  (`What::Finished`'s `commits`, newest first) and its detail says the
  one, or how many and their subjects. An item the journal never heard
  taken has none. Checked with a dev instance: an item held by a session
  in its own worktree, two commits and a merge of main there, then done;
  the line had the two and the panel said "2 commits: Test the gizmo ·
  Add the gizmo". Commits on main meanwhile were rightly not its.

- **A background session's line** carries a small cloud after its words
  (`theme::BACKGROUND_ICON`), known by its `bg-` id
  (`background::is_tile`), since Claude Code's daemon holds it and a click
  attaches rather than shows a pane of ours.
- **The wheel scrolls** a panel that did not fit, a row a notch as the
  tasks tile does (`layout::catchup_scroll`, tested): down while rows are
  left under the last one showing, up to the top. A panel that does not
  fit takes all the height it may have, so scrolling never resizes it.
  The last line counts what is under it, and at the bottom what is
  above. Checked with a dev instance and a journal of 39 lines: the cloud
  on both background lines, ten notches down stopped with the last line
  showing and "1 more above", four up came back to the top.

### The stash

"The stash" in [IDEAS.md](IDEAS.md). History has every conversation, and
that is too many to keep the few you mean to come back to. The stash is
those few, kept by hand: a session put away is paused and out of the
columns, and a click brings it back as it was.

- **Out of the registry, not flagged in it.** A stashed session leaves
  `Registry::sessions` for `Registry::stash` as a `SavedSession`
  (`registry.rs`, pure and tested). So every list that draws, counts or
  runs sessions (clusters, the stage, the tray tip, the journal, the
  catch-up) leaves it out without being told, and a flag nobody forgot
  to check was the alternative. `Registry::apply` ignores its id, so a
  hook still on its way from a stopped `claude` does not bring back a
  tile. Nine slots (`STASH_SLOTS`); Stash is greyed out as "Stash is full"
  past that.
- **Stashing.** "Stash" in the tile menu of an agent session, live or
  paused, never a plain terminal or a background session. A live one
  stops: out of the registry first, then its host is killed, so its exit
  finds nothing to pause. Asked first only when it is mid turn, since the
  turn is cut short; one waiting or at its prompt resumes to the same
  place. Its arguments, conversation id, loot and worktree go with it.
- **Bringing back.** A click on a slot puts the session back in its
  project as a paused tile and resumes it on the stage, the same path a
  paused tile's click takes. Its right click has Bring back, Bring back
  paused, and End session, which removes its worktree as ending any
  session does.
- **Kept safe while stashed.** Its folder and worktree count as busy, so
  the sweep of merged worktrees leaves them. The runner reads a stashed
  holder as paused: it waits rather than start past an item put away.
  History leaves its conversation out, since a slot already holds it.
- **The window** (`stash.rs`) is a column window like the usage window,
  key `columns::STASH`, dragged the same way, under the usage window when
  it first comes (`Columns::add_under`) and there only while the stash
  holds something. A grid of nine in a well sunk into the plate
  (`layout::stash`, tested): a stashed session a key with its name in its
  item colour and its project beside a lamp in the project's accent, an
  empty slot a latched bay. "Stash" and "n of 9" above.
- Saved as `stash` in `state.json`, left out while empty. A saved entry
  that is also a tile, or past the ninth, is dropped on load.

Tested on screen with a dev instance on its own port, `APPDATA` and
`LOCALAPPDATA`, with `cmd.exe` as the agent. Stash from a tile's menu
took it out of its cluster and showed the stash under the usage window;
after a restart it was still there. A session with a worktree stashed
the same way closed its project's cluster and kept the worktree. A click
on the first slot put it back in its cluster and resumed it on the
stage; End session on the other removed its worktree and branch, and
the stash window went, the column closing up. The first try found the
new window born under the installed Horadric's cluster, so it is raised
when created. Not tested on screen: the mid turn question, a full stash,
and a real `claude` resuming from the stash (the resume path is the
paused tile's).

### Tal Rasha's tombs

"Tal Rasha's tombs" in [IDEAS.md](IDEAS.md). Seven tombs and only one is
real: one item of the task list worked by several sessions at once, each in
a worktree of its own, and the human keeps the best.

- **One holder, still.** The list keeps one `@id` per item, so the file
  alone says who has what. An item in tombs is held by its batch,
  `@write-a-poem-63879.x3` for three, and tomb 2's session id is
  `write-a-poem-63879.x3.2`. How many tombs an item has and which session
  is which tomb needs nothing beside the list and the session ids
  (`horadric_core::tombs`, pure and tested). A batch holds its item as its
  tombs do between them: paused while one is, live while one runs, gone
  once none is left.
- **Starting.** "Start in tombs, pick the best" in an open row's menu, 2 to
  8, only where each session gets a worktree. The file is written first,
  as for any item, then tomb 1 starts in its own worktree, named "Tomb 1:
  <title>", with the item as its prompt. The runner starts the rest, one
  every 10 s in the project, in any mode, since the click asked for them.
  None starts beside a paused tomb, and none for a batch with no tomb left:
  after a restart that is the human's to start again. A tomb started once
  stays started, so one the human ends is not started again. Each tomb is
  told it is one of several and not to look at the others or merge its
  branch.
- **The fuses.** Never more than 8 tombs. A batch takes one place of
  `parallel` for each tomb, so the runner starts no other item past it:
  three tombs with `parallel` 3 hold the list. Tombs are nudged like any
  item's session, each until it reports.
- **Reporting.** `task done` in a tomb leaves the list alone, since the
  item is the batch's until the human picks. It posts to the app, which
  keeps it in the tomb's loot (`finished`, saved), and says so if the app
  did not hear. `task blocked` in a tomb is a notification.
- **The row** (`board::state_in`, `board::tombs_row`, pure and tested)
  reads "tombs" while they work, "asks you" when one stopped without a
  report, "paused" while one is, and "pick one" once every tomb still
  there says it is done. With every tomb started and done, a notification
  says so once.
- **Picking.** "Pick this tomb..." in a tomb's tile menu, or "Pick the one
  to keep" in the row's. It asks: merge its branch into what the main tree
  has checked out, keep the branch to merge later, or cancel. The others
  end, fading out as any ended tile does, and their worktrees and branches
  are deleted with `git worktree remove --force` and `git branch -D`,
  their work with them, which the question says. The item goes `[x]` held
  by the winner, which is the review, and the winner's session closes as
  any finished item's does; its worktree goes, and its branch too once
  merged. A kept branch is offered for merging as a finished item's is.
  "Put back in the list" ends every tomb, keeping their branches as ending
  any session does.

Tested on screen with a dev instance on its own port, `APPDATA` and
`LOCALAPPDATA`, `cmd.exe` as the agent, on a scratch repository. The
screen was locked, so every click was a message posted to the dev
instance's own windows and every look a `PrintWindow`. "3 tombs" wrote
`@write-a-poem-63879.x3` and started tomb 1 in `tr.write-a-poem`; tombs 2
and 3 followed 10 s apart in `tr.write-a-poem-2` and `-3`, and four
`cmd.exe` ran the whole time (three tombs and a plain session). Each tomb
committed a poem and reported done from its worktree with its session's
environment, the third blocked first: the row read "tombs", the blocked
report was a notification, and the third done turned the row to "pick
one" with the notification. "Pick this tomb..." on tomb 2 asked as above,
and "Merge it" gave a merge commit of tomb 2's poem on main, `[x] ...
@write-a-poem-63879.x3.2`, tombs 1 and 3 gone with their worktrees and
branches, and tomb 2's worktree and branch gone once its session closed.
One `cmd.exe` left. Not tested: a real `claude` in the tombs, "Keep its
branch", picking from the row's menu, and a restart halfway through a
batch.

### Transmute

"Transmute" in [IDEAS.md](IDEAS.md). The Horadric Cube: tiles dropped in
it, and a recipe that runs on what it holds. Dragging tiles together says
"these belong in one action" quicker than a menu on each tile could.

- **Recipes are small named actions, decided by pure matching**
  (`horadric_core::cube`, tested). The cube holds up to three sessions and
  `main`, and `cube::recipe` says which recipe that makes, if any:
  - Two sessions at rest that changed something: **Review both**. A new
    session starts in the first one's main tree and is told to read both
    diffs (`git diff <base>...<branch>` and what is uncommitted, or the
    shared tree's `git diff HEAD`), say what each does and what is wrong,
    which is better if they did the same thing, and to change nothing.
    It is named "Review: a + b" and comes onto the stage.
  - One session with a branch of its own and `main`: **Merge into main**,
    the same merge a finished item's branch gets, with its notification
    and journal line. The session stays; its name turns gold.
  - Three sessions at rest: **Close all three**. Each ends as End session
    does and leaves one line in the journal (`What::Closed`, its last
    message cut at a word to 120 characters), which the catch-up shows in
    place of "ended".
  - Nothing runs on a session mid turn (working or waiting), since every
    recipe would cut its work short. `cube::hint` says what is missing or
    in the way instead: "beta is mid turn", "gamma changed nothing to
    review", "alpha has no branch of its own", "Main takes one session".
- **Runewords are the same system.** A recipe casts one action on what is
  combined at once; a runeword casts several on one session over time. See
  "Runewords" below.
- **Dropping.** A tile lifted in a cluster (the reorder lift) and let go
  over the cube goes in instead of moving. A lone tile drags its window,
  so the window let go over the cube puts its session in and glides back.
  A pane dragged by its header on the stage and let go over the cube goes
  in too. While one is carried over it the cube's lid lifts with a light
  inside, and the cube comes above the dragged cluster so it is seen.
  Clusters and the stage find the cube by its window in `Shared::cube`
  and tell it with a posted message, since they hold the mouse and may be
  inside the app's borrow.
- **Only agent sessions go in**, never a plain terminal or a background
  session. A full cube says so in a notification. A click on a slot takes
  its session out, a click on `main` puts it in or takes it out, and the
  key between them runs the recipe and empties the cube.
- **The window** (`cube.rs`, the app's side in `transmute.rs`) is a column
  window like the stash, key `columns::CUBE`, under the stash or the usage
  window when it first comes, and there while any agent session has a tile.
  `layout::cube` (tested): the cube drawn from above one corner, the key
  that runs the recipe (latched and saying what is missing while there is
  none), the `main` rune, and a well of three slots showing each session
  as the stash does. The cube's runes light gold once a recipe is ready.
- **Off by default.** The recipes mostly repeat what a tile's menu does,
  so the cube is shown only once "Horadric Cube" is ticked in the menu a
  right click on the usage window opens (`cube` in `state.json`). Turned
  off, it lets go of what it held and goes, after a transmute playing has
  finished. The recipes it could grow into are in [IDEAS.md](IDEAS.md).
- **Not saved.** What the cube holds is a hand of tiles on the way to a
  recipe; after a restart they stand in their clusters as before.
- **The transmute animation.** Running a recipe plays it in the cube's
  window (`CubeWindow::transmute`, called before the app empties the
  cube): the lid lifts, each slot's key shrinks and swirls half a turn
  over the top into the cube's mouth, one after another, trailing gold
  light, and the lid drops. Then a burst of gold comes off it with sparks
  thrown out, and the key says what came of it (`Recipe::outcome`: "A
  reviewer starts", "Merged into main", "Closed and journaled"). The
  curves are `motion::transmuting` and `motion::swirl` (tested), the
  swirl flattened as the cube is seen from above one corner, which also
  keeps it inside the window. 1.5 seconds on a timer of its own that runs
  only while it plays, so the cube costs nothing at rest. The window
  stays while it plays even when the recipe leaves nothing for it to
  hold, and `Input::CubeSettled` lets it go after. With Windows'
  animations off nothing plays.
- **A batch closing plays it too.** Picking the tomb to keep closes Tal
  Rasha's batch, and the cube plays it (`CubeWindow::transmute_batch`)
  before the others end: every tomb with a tile swirls in from the
  slots, taken in turn past three, `main` after them when the winner
  merges, and the key says "Tomb 2 merged" or "Tomb 2 kept". What the
  cube holds stays in it. `motion::stagger` (tested) spaces the keys
  0.12 of the swirl apart, closer for more, so the last of eight still
  has most of the swirl.

Tested on screen with a dev instance on its own port, `APPDATA` and
`LOCALAPPDATA`, three sessions in worktrees of a scratch repository, and
fake hook events for their phases and edits. The screen was in use, so
clicks were messages posted to the dev windows and every look a
`PrintWindow`; for a drop the cube was slid under wherever the cursor
stood, the lift posted, and the cube put back. The lid lifted while a tile
was over it; one tile read "Add main to merge, or another to review", two
finished ones offered Review both, and the reviewer tile started. With a
fake `claude.cmd` that writes its arguments down, the reviewer got the
whole prompt as one argument. alpha and `main` offered Merge into main and
gave a merge commit on main, alpha's name gold. Three at rest offered
Close all three: three hosts stopped and three `closed` lines were
journaled with no `ended` beside them. A lift let go while the cursor had
moved off the cube reordered the tiles instead, as it should. Not tested:
dropping a pane from the stage, dropping a lone tile's cluster, a full
cube, and a real `claude` reviewing.

The animation was checked the same way: three fake finished sessions
dropped in, Close all three posted, and a `PrintWindow` of the cube every
110 ms. The keys swirled in inside the window, the burst and "Closed and
journaled" followed, and the cube went once it had played. Not checked:
Review and Merge, whose `main` rune swirls in too.

A batch closing was checked with a click on the cube that, in a build
never committed, played five tombs and `main`: the five swirled in one
after another from the three slots, `main` last, then the burst and
"Tomb 2 merged", and the cube settled back to "Drop tiles here". Not
checked on screen: a real pick of a tomb calling it.


### Runewords

"Runewords" in [IDEAS.md](IDEAS.md). A named sequence of actions a session
is given, one per turn: "Test, review, merge" tells it to test, has a
reviewer read its work and tells it to answer, then merges its branch.

- **One system with the recipes.** The actions are runes, and the cube and
  runewords are two ways to cast them: the cube casts one on what it holds
  at once, a runeword casts several on one session over time. Review and
  merge are the same actions in both (`App::start_reviewer`,
  `App::merge_session`, `cube::diff_of`). A second system with its own
  review and merge would drift from the first. What the cube matches stays
  as it was: recipes are about what is combined, which a sequence on one
  session is not.
- **The runes** (`horadric_core::runeword::Rune`): **test** tells the
  session to run the tests, fix what fails and commit; **review** starts a
  reviewer on the session's diff that writes its review to
  `%APPDATA%\Horadric\reviews\<session>-<n>.md`, and once the reviewer's
  turn ends tells the session to read the file, fix what it agrees with and
  commit, then ends the reviewer; **merge** merges the branch of its own
  worktree as the cube does, and passes on a session in the shared tree,
  whose commits are already there. Any other word in a config's runeword is
  said to the session as it is. The review goes through a file because the
  hooks carry only a reviewer's first line, and the file outlives the
  reviewer for the human to read.
- **The next step is pure** (`runeword::act`, tested): a rune is cast only
  on a session at rest (done or idle, not waiting on the human, not
  paused); a rune told to the session is done when a turn that ended after
  the telling ends; a review is answered once the reviewer's turn ends.
  The session ending, the reviewer ending first, a reviewer that cannot
  start, a merge that fails or the human taking over stops the runeword
  with a notification naming the rune.
- **The human taking over stops it.** A turn cut short (Esc, or No to a
  permission) sends no `Stop`, so the next turn to end is one the human
  asked for. The runeword notes when the told prompt went in, and a
  prompt after it, the human's, stops it: "you took over from it". It
  does not wait and count the human's turn as the rune, which would take
  "test" as done and merge untested work, and it does not tell the rune
  again, over the top of whatever the human cut it short to do. The
  human gives it again when ready. A turn cut short and never followed
  by a prompt leaves the runeword waiting, gold on the tile, with Stop
  in the menu. After a restart the app has heard no prompt yet, so a
  prompt it missed while down goes unnoticed. The last rune done says "<name> is complete".
- **Telling** is the runner's nudge: the line typed to the session and its
  Enter 400 ms after, so the agent takes it as typed. The app looks once a
  second, in the runner's tick.
- **Given from the tile menu**: "Runeword" opens the project's runewords,
  and a session with one offers "Stop <name> (<rune> n/m)" in its place.
  Stopping leaves a reviewer it started running on its own. Every project
  offers "Test, merge", "Test, review, merge" and "Review, merge", after
  its own in `.horadric/config.json`, which replace a built in one with
  the same runes:

  ```json
  { "runewords": { "Ship": ["test", "Update the changelog", "merge"] } }
  ```

- **The tile** shows the rune being cast before its last line, in the
  cube's gold: "review 2/3", or "rune 2/3" where the word does not fit.
- **Saved** on the session in `state.json`, step and all, so a reload or a
  restart goes on where it was.

Tested on screen with a dev instance on its own port, `APPDATA` and
`LOCALAPPDATA`, `cmd.exe` as the agent, a session in a worktree of a
scratch repository and fake hook events for its turns. The screen was in
use, and a tile menu opens at the real cursor, where the human's clicks
landed on it twice and ended the session; so the runewords were written
into `state.json` with the dev UI stopped, which tested the save as well.
"Tidy" (`"echo tidy said"`, merge) typed its line into the terminal, and
the turn's end merged the branch into main and said "Tidy is complete".
Review then an `echo`: the tile read "review 1/2" in gold, a "Review:
beta" tile started, and its turn's end typed the answer prompt naming the
review file into beta and ended the reviewer; beta's next turn end cast
the `echo`, and the one after completed it. The menu's Runeword submenu
was seen but not picked from.

Then with a real `claude` (Haiku for the session, the default model for
the reviewer), in a scratch repository whose one test failed and whose
`main` got a commit on the same line after the worktree was made. "Test,
review, merge": the test rune was typed, the session fixed the line and
committed, and its turn's end started "Review: rune". The reviewer ran the
test, found with `git merge-tree` that the branch conflicts with `main`,
wrote a full review to `reviews\rune-68476-2.md` and changed nothing else;
its turn's end told the session to answer and ended it. The session
rebased on `main`, resolved the conflict, committed, and the merge rune
landed it: "Merged rune", then "Test, review, merge is complete". A second
session was given a said rune that changed the same line as a fresh
commit on `main`, then merge: the merge stopped on the conflict, was
aborted (no `MERGE_HEAD`, `main` clean), the branch was kept, and the
notifications said "Cannot merge howdy" with git's `CONFLICT` line and
"Howdy stopped". What it showed:

- In Claude Code's default permission mode the session asks before it
  reads the review, since the file is outside its worktree. The reviewer
  did not ask to write it only because it started in auto mode.
- A told rune whose turn the human cuts short (No to a permission, or
  Esc) sends no `Stop`, so the runeword waited and counted the human's
  next finished turn as the rune done. Now the human's prompt stops it,
  see above.
- A `state.json` that does not parse (a said rune written by hand as a
  bare string instead of `{"say": ...}`) lost every session, and the
  next save overwrote it. Now the file is set aside and only that
  session is dropped.

### Experience

"Experience" in [IDEAS.md](IDEAS.md). One XP for every commit of yours
that landed, and the level they add up to, on the first line of the tray
menu: "Level 7    105 / 140 XP".

- **Counted from git, never kept.** In every recent project, the commits
  on what the main tree has checked out, merges left out, whose author
  email is the one `git config user.email` gives there. A hash counts
  once, so two projects in one repository count its commits once. A
  reinstall or a new machine loses nothing.
- **On a thread.** The count runs at start and each time the tray menu
  opens, which shows the one before, so opening it never waits on git.
- **Levels come slower as they go**, as in Diablo: level L at
  `5 * L * (L - 1) / 2` XP, so level 2 at 5 commits, 10 at 225 and 99, the
  last, at 24255. Pure and tested (`horadric_core::experience`).

Not seen on screen: the one try found the screen in use by a full screen
game and was stopped before the menu was read.

### The Cow Level

"The Cow Level" in [IDEAS.md](IDEAS.md). A secret recipe in the cube, as
in Diablo II, where Wirt's Leg and a tome of town portal open the way.

- **The way in.** A session whose name has "wirt" in it, in any case
  ("Wirt's Leg" after a rename), alone in the cube with `main`, the tome.
  `cube::recipe` checks for it before the rest and whatever the session
  is doing, so it wins over the merge its branch would make and runs mid
  turn: it does nothing to the session. Nothing on screen hints at it.
  The leg alone still says "Add main to merge, or another to review",
  and only once `main` is in does the key offer "Open a portal".
- **The portal.** It plays in the cube's own window, never over a tile:
  the leg and `main` swirl in with red light, then a red portal stands
  over the cube with specks turning inward on three rings, and the key
  says "There is no cow level". `motion::portal` (tested) swirls as long
  as a transmute does, opens fast, holds, and fades, 3.5 seconds in all.
- **It starts nothing.** The cube empties and the session stands in its
  cluster as before: no agent, no merge, no journal line.

Tested on screen with a dev instance on its own port, `APPDATA` and
`LOCALAPPDATA`, and `cmd.exe` as the agent: "Wirt's Leg" and "other" in
one cluster, the leg lifted onto the cube (slid under the cursor, the lift
posted), `main` and the key clicked by posted messages, and a `PrintWindow`
of the cube every 120 ms. The portal opened and closed as above, both
tiles stayed, and there were still two hosts and two `cmd.exe`.

### Quests

"Quests" in [IDEAS.md](IDEAS.md). The task list is a quest log: an item
is a quest, taking it is accepting it, done is completed. Renamed once,
with the old names still read, since the list that runs the work runs
through the rename too.

- **The file** is `.horadric/quests.md`. A project with only
  `.horadric/tasks.md` keeps using it, read and written, until a quest log
  sits beside it; then the quest log wins. Nothing moves the old file: a
  running list keeps its file, and an installed build from before the
  rename, still reading `tasks.md`, sees the same list as the new one. A
  new list is a quest log. `tasks::list_file` picks, pure and tested;
  `horadric_hooks::tasks::file` and `rel` are where it is on disk.
- **The command** is `horadric quest done [SUMMARY]|blocked WHY|add
  TITLE|list`. `horadric task` does the same, so every session started
  with the old prompt still reports back. New prompts say `quest` and name
  the file the project really has, and ask for `quest done "<one short
  line on what you achieved>"`: the words after `done` go into the
  chronicle as the quest's summary (a bare `done` still works). A `quest
  add` from inside a session goes into the chronicle too, as added by
  that session, so the quest log draws the new quest branching from the
  one that session works.
- **The tile** says quests: "New quest", "Accept", "Mark completed",
  "Edit the quest log", and the toasts "Quest log completed" and "N quests
  need you".
- **Editing on the tile.** A quest's right click menu has "Edit quest"
  (the themed input with its title and notes filled in), "Move up",
  "Move down" and "Delete quest", so the log is kept without VS Code.
  Asked for on 2026-09-29. Each changes the file as the rest of the list
  does, found by line and title and doing nothing when the file moved
  under it (`tasks::edit`, `remove`, `shift`, pure and tested). A move
  swaps a quest with the next one, notes and all, and leaves a heading
  between them where it is, so a quest crosses into the next section.
  Delete asks first and is not offered while a session holds the quest:
  put it back first. Checked on screen with a dev instance.
- **A draft survives a click away.** Clicking elsewhere to look something
  up used to throw away the title and notes typed for a new or edited
  quest. Asked for on 2026-10-03. The input now hands what was typed back
  when it is left rather than answered (`ask::Reply::Left`), and the
  runner keeps it, for a new quest by project and for an edit by the
  quest's line and title too (`runner::draft_for`, tested). The next +
  or Edit quest fills it in with the caret at its end. Esc still throws it
  away, and adding or saving clears it. Kept in memory only, so a reload
  forgets it. Checked on screen with a dev instance: type, click away, +
  again, the text is there.
- **The quest giver.** A gold ! left of the plus, the mark over a quest
  giver's head in the games, starts a session named "Quest Giver" in the
  project, on the stage. Asked for on 2026-10-01. Its
  prompt (`tasks::giver_prompt`, tested) has it read the README, the docs,
  the quest log, the git log and the code, change nothing, and suggest three
  to five quests not on the log, each with notes. It asks which to add and
  adds only those, with `quest add "title" --notes "..."`. It suggests
  rather than writes, since a project in auto mode starts whatever lands in
  the log at once. It holds no quest, so it gets no quest system prompt and
  the runner leaves it alone. Checked on screen with a dev instance and
  `cmd.exe` as the agent: the session starts and takes the stage.
- **Left alone**: the `"tasks"` key in `config.json`, the
  `/horadric/tasks` path, its header and `HORADRIC_TASKS`. They are what
  builds on either side of the rename say to each other, and no one reads
  them.

Not checked on screen: the tile's words are the only change there.

### The quest log window

Asked for on 2026-10-03: a quest done leaves the tile and its session
closes, and there was no way back to what it did or how it ended short of
asking another session.

- **The chronicle** (`horadric_core::chronicle`, pure and tested) is
  `chronicle.jsonl` beside `state.json`, never trimmed, unlike the
  journal's week. A line for each quest accepted, marked (review, blocked,
  done, put back), summed up, added, each turn of its session ended (last
  message, conversation id, folder, tile name), its commits and its merge.
  The app writes most of them (`runner.rs`, `journal_phases`);
  `horadric quest done "summary"` writes the agent's own one line and
  `quest add` inside a session writes who added the quest, which is how a
  quest becomes another's child. Every prompt now asks for that summary.
- **Quests from before the chronicle** come from the list itself, every
  held item, with what the journal's week still knows of them.
- **The diagram** (`chronicle::graph`, pure and tested) is drawn like
  `git log --graph`, newest at the top: the main line is the trunk, each
  quest a lane from where it branched (the trunk, or the quest that added
  it, whose lane runs on until its children branch) until it converged
  back into the trunk when done, or stopped with a cap when blocked or put
  back. Dots take the tile's lamp colours.
- **The window** (`questlog.rs`, `render/questlog.rs`, `chronicler.rs`),
  one at a time, from "Quest log..." in the quests tile's mode menu, a
  quest's right click menu and the project menu. A row a quest: title,
  outcome, age and its one line result. Click one for the detail: times,
  where it came from, what came of it, notes, commits, merged branch.
  "Read the session" turns its transcript into text in
  `chronicle\<id>.md` and opens it read only on the stage; "Carry it on"
  resumes the conversation in a new tile, or shows the one that holds it.

Checked on screen with a dev instance and a dozen fake quests in dark,
light and narrow, and the recording with `cmd.exe` as the agent and a
fake `Stop`. Not clicked on screen: the three menu entries (the window was
opened by messages), and the `Commits` and `Merged` records.

**Conversations on the main line** (asked for on 2026-10-03, folding the
History menu in):

- **What shows.** The project folder's newest 40 conversations of every
  agent (`past_in`, the History menu's reader with its rules: untitled
  ones left out), less those a quest holds and those in the stash. A
  conversation a live tile holds stays: it is usually the main session
  the quests came from, and Carry it on shows its tile. They are read
  again every 30 ticks, since nothing says when one starts.
- **Where.** `chronicle::with_talks` (pure and tested) puts each among
  the quests as a `Quest` with `main` set, at when its transcript file
  was created, and `graph` gives it a row on the trunk and no lane. A
  dot of its own colour (the magic blue), a faint band behind its row.
- **A quest added in one** branches from its dot: `quest add` now records
  the conversation of the session that ran it (`CLAUDE_CODE_SESSION_ID`,
  which Claude Code gives its commands), and when that session worked no
  quest, the quest's lane starts on the conversation's row (`Row::forks`)
  and runs up to the quest. Quests added before this grow from the trunk
  as before.
- **Its detail**: the title, Started, Last touched, the quests it added,
  and Read the session and Carry it on as for a quest. Read is Claude
  Code's only; a Codex or Grok conversation can be carried on.
- **All conversations...** sits in the main line's band: a session on
  Claude Code's own picker of the folder's conversations.

Checked on screen with a dev instance and `cmd.exe` as the agent: the
tray's Quest log submenu opened the window for a project with no cluster
and for this one, whose real conversations lined the main line between
the quests; a conversation's detail, Read the session (the transcript on
the stage), Carry it on (`--resume <id>`) and All conversations
(`--resume`) all clicked; the project menu's Quest log... switched the
window; a fake `Added` with a conversation drew the fork from its dot and
listed it under the conversation. Not clicked: the start window's right
click, which was not up with clusters open.

### Waits a blocked quest can name

Asked for on 2026-10-01: a `[!]` quest stopped the whole list until the
human came, even when what it waited on was another quest that the list
would get to anyway.

- **The form.** `horadric quest blocked "why" --on-quest "title"`, or
  `--on-main <commit or branch>`, `--on-file <path>`, `--on-cmd
  "<command>"`, `--until <+30m, Unix seconds or 2026-10-01T14:05Z>`. With
  a wait the why may be left out. The wait goes at the end of the item's
  line in braces, `- [!] Wire it @wire-1: needs it {on quest: Build it}`,
  so the file stays the state and a human can write one too
  (`tasks::Wait`, pure and tested). A time is kept in UTC, so the file
  means one moment wherever it is read.
- **What holds.** A quest of that title, any case, marked `[x]` in the
  same log. A ref that `git merge-base --is-ancestor <ref> HEAD` passes in
  the main tree, so a branch with nothing new on it counts as merged. A
  file from the project folder. A command `cmd.exe /d /c` runs in the
  project folder exiting 0 within 30 seconds. The list and the clock are
  asked on every look; git and a command run on a thread at most every 15
  seconds, and until one has answered the wait is not over.
- **The runner** (`tasks::next`, tested) passes a blocked quest with a
  wait that does not hold yet and starts the next open one. A quest
  blocked with only a why still stops the list there, since that needs the
  human. A list with nothing left but waits is not finished. Once a wait
  holds the quest takes a place like a start (`Next::Resume`), in list
  order, within `parallel`, at most one start per project every 10
  seconds. Its session, if there and between turns, gets `[/]` back with
  its holder and is told what happened and to go on (`tasks::waited`),
  typed in like the usage limit's go on. A session that is gone, ended, or
  whose terminal exited, starts the quest again as a click on a gone row
  does. A paused session, after a restart or a crash, waits for a click,
  as every paused session does. Only in review and auto mode: manual
  starts nothing. A toast says the quest goes on; a waiting quest raises
  no "Blocked" toast, since it needs nobody.
- **The row** reads in the idle colour with a clock and says what it
  waits on, "after Build it", "for done.txt", "in 25 min"
  (`Wait::label`, `board::note`).
- **The system prompt** spells out the flags and asks the agent to use one
  whenever it fits.

Checked on screen with a dev instance on its own port and app data and
`cmd.exe` as the agent, in auto mode: of two quests, the runner started
the first, which was blocked on the second with `--on-quest`; the row read
"after Build the engine" and the runner started the second. `quest done`
on it marked it `[x]`, closed its session, put the first back to `[/]`
with its own session and showed "Quest goes on". A quest held by a session
that no longer exists, waiting on a file that was there, started again in
a new session. One `cmd.exe` at any time. A killed terminal read as
paused, so its quest waited for a click. Not checked on screen: `--on-cmd`,
`--on-main` and `--until`.

Built as the quest asked, with every kind of wait, before Orchestration
below was written. If its `After:` lines are built, `--on-quest` should
write one of those instead of `{on quest: ...}`, and `tasks::next`
already passes a quest that waits and wakes it in the same place.

### Performance

On 2026-09-26 everything felt less smooth: typing in the stage, right
clicks, the app in general. Every thread but the UI thread sat idle, and
the UI thread did work nobody asked for. Measured on a dev instance with
fifteen clusters, three animating tiles and eight hook events a second,
its share of a core fell from 75% to 6%. With the tiles
animating and no events, it fell from 15% to 1.3%. What was
fixed, and what to keep that way:

- **No paint waits for the screen.** Render targets present with
  `D2D1_PRESENT_OPTIONS_IMMEDIATELY`. With the default, every `EndDraw`
  waited for the next refresh, and every window paints on the one UI
  thread, so a few animating clusters filled each frame and a keystroke's
  echo waited behind them. DWM composes the windows, so nothing tears.
- **A keystroke paints once.** Sending typed bytes repaints the pane only
  when that cleared a selection or scrolled back to the live screen. The
  echo repaints; a paint before it showed the old screen and pushed the
  echo a frame back.
- **A cluster redraws only when what it shows changed.** A full redraw of
  the kept layer is the dearest thing the app does. `Cluster::update`
  lays out again and redraws only if the layout, the sessions, the quest
  rows, the board or the stage marks differ from what the kept layer was
  drawn from (`Still`). Every hook event, every arrange and every change
  count goes through it, so an event for one project redraws that one.
  `Cluster::fit` still always redraws, for a click that changed something
  only the cluster knows. The second tick redraws a cluster only when an
  age label or an activity trace moved (`Clock`), which for an old tile is
  once a minute.
- **Hook events are taken in bursts.** The feeder posts
  `WM_HORADRIC_EVENT` only if the UI has not been told yet, and the flags
  wait in `EVENTS`. Ten events queued behind a menu cost one pass.
- **Folder keys of missing folders are remembered for five seconds.**
  `folder_key` is called per session, per cluster, per paint. For a
  folder that is gone (a swept worktree, a deleted project) it started
  `git` every time, which made each event cost 90 ms with four such
  folders in the saved state.
- **Menus do not wait for git.** The tile menu lists the change count the
  tile keeps, counted again within seconds of the agent's last move, and
  counts only when there is none. The project menu reads history and asks
  git side by side, transcript titles are kept while their file is
  unchanged, and `worktree::main_tree` remembers its answer.
- **Light nobody can see does not move.** A cluster's animation frame is
  skipped while it is minimised, off screen or wholly under another
  window. Browsers and Electron apps draw without a redirection bitmap,
  so that style alone does not make a window see-through; layered,
  click-through and overlay tool windows never count as covering.

A second pass on 2026-10-01 took the stage panes, measured the same way
on a release build with the stage on top. One pane full of characters
the font lacks (CJK, `⏺`, `★`) went from 37% of a core to 1.5% when only
its title's spinner turned, and from 38% to 3.5% with a line of output
every 50 ms. Two panes of Claude Code like output at ten frames a second
went from 8% to 5%.

- **Loose characters are laid out once.** A character the terminal font
  lacks is drawn from an `IDWriteTextLayout` kept in `Font` by text and
  style, not by a `DrawText` that looked for its fallback font on every
  paint. A new size or family starts the cache over, and so do more than
  2048 of them.
- **The glass's shade needs no layer.** The shade under the glass's top
  edge, and under a latched tab key's, is the rounded shape filled with
  the clamped gradient and clipped to the band, instead of a layer and a
  geometry made on every paint.
- **A spinner in a title is left out.** `shell::title` drops a braille
  glyph in front of a title, which is how Claude Code shows it works. The
  pane's header, the stage's caption and the window title no longer
  change on every turn of it.
- **A pane that would draw what it shows presents it again.** The pane
  keeps what it last drew (`Shown`: the frame, the header, the search,
  the veil, the size, the font) and only presents the kept frame when a
  paint would draw the same. Output that changes nothing on screen costs
  a comparison. Browser panes always draw.
- **A cluster paint copies its sessions once.** It reads them from the
  registry once, as `Rc`, and the tiles leaving and the `Still` it keeps
  share them.

### The Runetome

Asked for on 2026-10-01. Runewords become shortcuts: programmable
buttons that do anything, from one prompt to a chain of prompts, keys
and commands. Each is a rune stone with a generated runeword carved on
it, kept in a tile of its own, the Runetome. Builds on "Runewords"
above, whose engine (one step a turn, the human taking over stops it,
saved in `state.json`) stays.

- **The tile.** One Runetome a project, in its cluster beside the
  quest log, shown whenever the project has a cluster. Its stones sit
  in rows, the built in ones first, then the project's, then the global
  ones, and last an empty stone. (The built in ones were "Test, merge",
  "Test, review, merge" and "Review, merge" until the feedback below.)
- **A stone** is drawn in Direct2D: a rough rounded slab, lit from the
  top left like the cube, with a glyph cut into it and its label under
  it. The glyph and a runeword name ("Tal Eth Ko", two to four of the
  33 rune names) both come from a hash of the stone's label, so a
  stone keeps its look when its steps are edited and two stones rarely
  match. `runeword::carve` (strokes from the hash) and `runeword::name`
  are pure and tested. Hovering a stone shows its name and its steps
  in a tooltip (tip.rs), so what a click does is never a guess.
- **Steps.** A stone is a list of steps, cast in order:
  - `say`: typed to the session, done when its turn ends (the said
    rune of today).
  - `keys`: keystrokes into the session's terminal at once, such as
    `"Esc"`, `"Ctrl+C"` or `"/clear{Enter}"`. Parsed by a pure, tested
    `runeword::keys`. Done once written; it waits for no turn.
  - `run`: a command run with `cmd /c` in the project's folder (or the
    session's worktree when it has one), hidden. Done when it exits; a
    non zero exit stops the runeword with a toast carrying the
    command's last line of output. `"show": true` runs it in a plain
    terminal pane on the stage instead, for a command worth watching.
  - `test`, `review`, `merge`: the runes as they are.
- **Keys stay inside Horadric.** A `keys` step only reaches Horadric's
  own terminals. Input sent to other programs' windows is fragile and
  fights the window manager; anything outside Horadric is a `run` step
  (a script, AutoHotkey, `start ms-settings:`), which needs no new
  dependency.
- **Casting.** A stone with any step that needs a session (`say`,
  `keys`, `review`, `merge`, `test`) casts on the session focused on
  the stage when that session is this project's, and asks "Cast on
  which session?" with the project's sessions otherwise. Dragging a
  stone onto a tile or a pane casts on that one, the way a tile goes
  into the cube. A stone of only `run` steps needs no session and
  casts at once, with the project's folder as its directory. A
  sessionless runeword lives on the project rather than a session, so
  it is saved beside the sessions in `state.json` and goes on through a
  reload too.
  A session casts one runeword at a time, and any other stone cast on
  it says to stop that first, except a stone of only `keys` steps:
  keys wait for no turn, so they are typed in beside the runeword
  already running, which is how a permission prompt that holds it up
  is answered from the tome without stopping it.
- **While one runs** the stone glows in the cube's gold and shows its
  step ("2/4"); a click on it then offers Stop. The tile it casts on
  shows "rune 2/4" as today. The tile menu keeps "Stop <name>" and
  loses the "Runeword" submenu, since the tome is where they are given.
- **Where stones live.** A project's in `.horadric/config.json`, every
  project's in `%APPDATA%\Horadric\runewords.json`, same shape:

  ```json
  { "runewords": {
      "Fresh start": { "steps": [ { "keys": "/clear{Enter}" },
                                  { "say": "Read docs/PLAN.md and take the next quest" } ] },
      "Open the site": { "steps": [ { "run": "start http://localhost:3000" } ] },
      "Ship": ["test", "Update the changelog", "merge"] } }
  ```

  The list form of today is still read: a bare word is a rune or a
  `say`. The tome reads both files again when they change, so a stone
  an agent adds appears without a restart. `runeword::parse` is pure
  and tested, and a stone that does not parse shows cracked, with the
  reason in its tooltip, rather than vanishing.
- **The empty stone** makes new ones, the way the quest giver makes
  quests. A click starts a session named "Runesmith" in the project, on
  the stage. Its prompt (`runeword::smith_prompt`, tested) says what a
  stone is, the step kinds, both files and their shape, and asks the
  human what the stone should do and whether it is for this project or
  every one. It writes the stone, then runs `horadric runeword list` to
  check it parses, and reports the stone's runeword name. Agents make
  stones; a human never has to write the JSON, though they can.
- **Trust.** A `run` step is any command, and a project's config comes
  with its repository, so a cloned project can carry stones. Nothing
  runs without a click, and the tooltip shows the command before it.
  A stone from a project's config whose steps changed since it was
  last cast shows a small mark until it is cast once, so a pull that
  changes a command is seen.
- **Left alone**: the cube and its recipes, and how runes are cast
  turn by turn.

The engine is built (2026-10-01). `runeword::parse` reads both forms
and every step kind, `runeword::stones` lays out built in, project and
global stones with a cracked one's reason, and `runeword::keys` turns
a spec into pieces written 400 ms apart, a run of text one piece and
each key in braces one of its own, so `/clear{Enter}` lands as typed.
A `run` step goes through `horadric runestep <file> [--show]
<command>`, started out of the app's job, which writes the exit code
to `<file>.exit` (and, hidden, the output to `<file>.log`) under
`runes` in the app's folder. That file is how a build after a reload
learns how a command it did not start ended. A shown one runs in a
plain pane that waits for Enter after a failure. Stones of only `run`
steps cast on the project (`OnProject`, saved as `runewords` in
`state.json`). The global file is `runewords.json` beside the state,
in `Horadric-dev` for a dev instance, and both files are read again
when their time or size changes. Until the tome existed, the tile
menu's Runeword submenu offered every stone that parsed. Checked on a dev instance with `cmd.exe` as the agent: `keys` typed
`echo ...{Enter}` and cmd ran it, keys then a hidden `run` wrote its
file in the project, a failing command toasted "it exited with 3:
boom went the command", a shown one opened a pane and closed it on
exit 0, the global stone ran, and a 30 second command cast before a
`reload` finished after it, the new build completing the runeword.

The tile is built (2026-10-01). It sits under the quest log in every
cluster of a project with a folder, folded by its header like the
others, the fold kept with the cluster. The app reads each project's
stones once a second (both files only when they changed) and hands the
tiles what moved: a stone written, one cast, one done. A stone's tooltip
is `runeword::tip`, its runeword name then its numbered steps, or why it
is cracked; the tooltip plate now takes a line made at run time as well
as a fixed one. The mark is a small amber dot. It is on a project's stone
whose steps are not the ones last cast, a stone never cast included, so
a stone a clone or a pull brings shows it before anything runs; built in
and global stones never carry it. The steps last cast are kept as
`runeword::fingerprint` (FNV-1a over the steps as JSON) in `state.json`
as `stones_cast`. A click casts at once a stone of only commands, or on
the session with the keyboard on the stage when it is this project's,
and otherwise asks "Cast on which session?" with the project's live
sessions; a paused, background or plain terminal one cannot take a
stone. A session that is casting already is not given a second
runeword: a toast says to stop the first. A drag that lets go over a
tile in any cluster or a pane on the stage casts there, the window on
top at that point deciding, so a stone dropped on a tile under another
window casts nothing. While a stone is cast it glows and its label reads
"1/3" in gold, and a click offers "Stop <label>" for each session or the
project casting it. The empty stone starts "Runesmith" as the quest
giver starts its session. The tile menu lost its Runeword submenu and
its stops for project casts, and keeps "Stop <name>" for the session's
own.

Checked on a dev instance with `cmd.exe` as the agent and a scratch
project with a stone of each kind: a hidden `run` of only commands wrote
its file at a click and its dot went; `keys` typed into the focused
session; `say` glowed "1/1" and the tile read the rune, and the click's
menu stopped it; a shown `run` opened a pane that closed on exit; keys
then a `run` did both; a global stone ran. With the stage on another
project, a click asked which session and cast on the one picked. A stone
dragged onto a pane on the stage and onto a tile in the other project's
cluster cast on each. Then with a real `claude` (Haiku) the empty stone
started the Runesmith: it read its prompt, was answered by a `say` stone
cast on it, wrote "Hello file" into the project's config, and the stone
showed on the tile with its dot within the second; a stone dragged onto
its pane approved its permission prompt, and a click on the new stone
wrote `hello.txt`. No `claude.exe` of the dev instance was left after.
What it showed:

- A tile added above the tome, a shown command's pane or a new session,
  moves every stone down. A stone pressed as the layout changes is the
  one now under the cursor.
- Claude Code asks whether to trust a folder it has not seen, and a
  stone of keys is how to answer it from the tome: `{Down}{Enter}` there,
  `{Enter}` for a permission prompt.

#### The human's feedback (2026-10-02)

After using the tome: no way to remove a stone, built in stones nobody
would click, and a click that casts without asking. What changed:

- **Built in stones that show what a stone can do**, one or two of each
  step kind, each worth a click on day one: Approve (`{Enter}`, which
  slips in beside a runeword held up on a permission prompt), Interrupt
  (`Esc`), Recap and Commit (`say`), Fresh start (`/clear{Enter}` then a
  `say`, a chain), Second opinion (the review rune) and Open folder
  (`run start "" .`, sessionless). A file's stone with a built in one's
  label takes its place, as one with the same steps did already.
- **`"about"`**: a stone in its object form may say in a sentence what
  it is for. The tooltip, `horadric runeword list` and the question
  before a cast show it, and the Runesmith is told to write one.
- **A click asks first.** "Cast <label>?" with what it is for, what it
  casts on and every step, Cast or Not now, and a "Do not ask again"
  check (`cast_without_asking` in `state.json`). The dialog took a check
  for it, beside its buttons. A stone with a command is asked in the
  warning tone. A project stone whose steps are not the ones last cast
  asks even when told not to, since that is the trust mark. A pick from
  "Cast on which session?" and a drag are not asked again: the pick and
  the drop were the human saying so.
- **Right click a stone** for its menu: its label and runeword name,
  Cast (or Stop while cast), and for a stone in a file "Change with the
  Runesmith" (`runeword::reforge_prompt`, the smith told which stone and
  file) and "Remove", which asks, then takes it out of the file with
  `runeword::unwrite`. That edits the text in place rather than through
  serde, whose maps are sorted (no `preserve_order`), so every other key
  and all the spacing stay as written, a BOM too. A built
  in stone offers "Put away" (`stones_hidden` in `state.json`). Every
  menu, and the one on the empty stone or the tome's header, has "Ask
  before a click casts" and, once one is put away, "Bring back".
- **Drag a stone within the tome to move it** (asked for 2026-10-02).
  While carried over the tome it shows in the place it would take
  (`layout::stone_slot`, the nearest stone's, the last over the empty
  stone), the others closed up around it; let go there and it stays.
  Let go outside the tome and it casts as before. The order is kept per
  project as labels (`stones_order` in `state.json`), laid out by
  `runeword::arrange`, so built in, project and global stones mix
  freely and a stone the order does not name, a new one, shows last.
  Checked on a dev instance: a move, a drop on the empty stone and a
  drop on a tile, which cast.
- **Found on the way**: a toast about a session casting already named the
  project rather than the session (`on_label` without a session in hand
  looked the id up as a project key).

Checked on a dev instance with a scratch project whose config was written
with a BOM: the click asked with the steps and the dot's note, the check
ticked and was kept, the command ran; the right click menu removed a
project stone, leaving the file byte for byte but for the stone and its
comma, BOM included; Approve was put away and brought back from the
header's menu.

## Next

### Codex and Grok Build beside Claude Code

Asked for: a ChatGPT subscription and an xAI subscription used from
Horadric the way a Claude one is, tiles, stage, limits and account
switching included. API keys wait. The concept left other agents out of
v1 because hooks were Claude Code's alone. That has changed: both now have
hooks close to Claude Code's, and one of them even reads Claude Code's.

**What each gives**, read from their docs, their source and this machine.
The spike (2026-09-29) installed Codex CLI 0.159.0 from npm and read
Grok Build 0.2.22. The live checks (2026-09-30) ran real turns on both
after the human signed in again: Codex 0.159.0 on a free ChatGPT plan,
Grok Build 1.0.44 on a free xAI plan, with throwaway logging hooks, `exec`
and `-p` turns, and each TUI driven through a pseudo console. "Seen"
means it happened on one of those turns.

| | Claude Code | Codex CLI (ChatGPT) | Grok Build (xAI) |
|---|---|---|---|
| Program | `claude` | `codex`, from npm `@openai/codex` | `grok`, in `~/.grok/bin` |
| Login | `.credentials.json` + `.claude.json` | `$CODEX_HOME/auth.json`: `auth_mode`, `tokens` (`id_token`, `access_token`, `refresh_token`, `account_id`), `last_refresh` | `$GROK_HOME/auth.json`: one entry per `issuer::client_id`, with `email`, `key`, `refresh_token`, `expires_at` |
| Login read again while running | no | only on a 401 and before a refresh, never watched; refreshed tokens are written back; a different `account_id` on disk is a permanent error (from the source) | read at session start; a running session did not notice the file emptied or its key changed, and kept its held login (seen). The binary has reload code ("auth.json changed but token key is identical") that did not fire |
| Hooks | `http`, headers from env | `command` only, in `~/.codex/hooks.json`, `config.toml` or `-c hooks.<Event>=[...]`; the parent's environment reaches it (seen); an unreviewed hook is skipped silently unless trusted in `/hooks` or run with `--dangerously-bypass-hook-trust` (seen) | `command` and `http`, in `$GROK_HOME/hooks/*.json` (always trusted; a project's `.grok/hooks` needs folder trust); the `http` fields are `type`, `url`, `timeout`, `env`, `matcher`, no headers; every `http://` URL is refused, `127.0.0.1` and `localhost` alike ("SSRF protection: only https:// URLs are allowed"), while `https://127.0.0.1` is tried (seen on 1.0.44); a command hook gets the parent's environment (seen); `[compat.claude] hooks = false` stops it running Claude's hooks, though `grok inspect` still lists them |
| Events | the set Horadric uses | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`, `Interrupt`, `SessionEnd` and more. Seen: `PermissionRequest` when the TUI asks; `Interrupt` on Esc mid turn or at an approval, with no `Stop` after it; `Stop` at every completed turn; no `Stop` for a turn that failed; `SessionEnd` with `reason: "other"` for a failed and a good session alike. `exec` forces approval to never, so it never asks | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied`, `Notification`, `Stop`, `StopFailure`, `StopCancelled`, `SessionEnd`, subagent and compact events. Seen: `Notification` `permission_prompt` when it asks ("Tool permission requested", "Plan approval requested"); `Stop` with `reason: "end_turn"` at every completed turn; `StopCancelled` instead of `Stop` for a rejected prompt (`permission_rejected`, after `PermissionDenied`) and for Ctrl+C (`user_interrupt`); `Notification` `idle_prompt` 60 s after a turn; at quit, `SessionEnd` and then one more `Stop` with `reason: "shutdown"` |
| Payload | `session_id`, `cwd` | `session_id`, `hook_event_name` (Pascal case), `cwd`, `transcript_path`, `model`, `permission_mode`, plus `turn_id` and `prompt` on a turn, `source` on start (seen) | both spellings: `sessionId` and `session_id`, `hookEventName` (snake case) and `hook_event_name` (Pascal case); plus `cwd`, `workspaceRoot`, `transcriptPath`, `timestamp`, `permissionMode`, `promptId` on a turn, `reason` on `Stop` (seen) |
| Resume | `--resume <id>` | `codex resume <id>` and `codex exec resume <id>`, any session in `$CODEX_HOME/sessions`, the desktop app's too and from another cwd (seen) | `--resume <id>`, `-c` |
| Limits | status line JSON | `token_count` events in `sessions/**/rollout-*.jsonl`: `rate_limits.primary` (300 min) and `secondary` (weekly), `used_percent`, `resets_at` | none found; `/usage` shows credits in the TUI |
| Model, effort, permissions | `--model`, `--effort`, `--permission-mode` | `-m`, `-c model_reasoning_effort=`, `--ask-for-approval`, `--sandbox` | `-m`, `--effort`, `--permission-mode`, `--always-approve`; the default is `[ui] permission_mode`, which Shift+Tab in the TUI rewrites (seen); `--permission-mode default` still reported `auto` (seen) |

**The shape.** An `Agent` in `horadric-core`, Claude, Codex or Grok,
saved on every session (missing reads as Claude, so old state loads). It
answers every question that is Claude specific today: the program, the
tag it gets, how its hooks are installed, how its events become a
`HookEvent`, its resume arguments, its flags for the defaults, its login
files and its limits. Everything downstream, the registry, phases, tiles,
stage and journal, keeps working on `HookEvent` and does not learn the
agent's name. No new dependency: the Grok hook file is JSON, Codex's
hooks go on its command line, so Horadric never has to write a TOML
file, and a JWT's payload is base64 and JSON.

**Steps**, each landing on its own:

1. **Spike, done 2026-09-29, live checks 2026-09-30.** What it found
   is in the table. The answers to the questions it was given:
   - A Codex command hook sees the parent's environment:
     `HORADRIC_SESSION` and `HORADRIC_OWNER_PORT` reached it.
   - Codex reads `auth.json` again only when a request gets a 401 and
     before it refreshes, and writes the refreshed tokens back. If the
     file now holds another `account_id`, it stops with a permanent
     error rather than use it. A failed refresh leaves the file alone.
   - `codex resume <id>` takes a session started elsewhere: a Codex
     Desktop session (originator "Codex Desktop", another cwd) loaded,
     and an unknown id says "no rollout found".
   - A Grok `http` hook cannot reach Horadric. 1.0.44 still refuses
     every `http://` URL, loopback included ("only https:// URLs are
     allowed for HTTP hooks"). It does try `https://127.0.0.1`, so only
     a TLS listener with a certificate Grok trusts would work, which is
     not worth it. Grok needs a command hook, the same `horadric hook`
     Codex uses. Its command hooks see the parent's environment.
   - The Claude hook Grok borrows from `~/.claude/settings.json` is an
     `http` hook to `127.0.0.1`, so Grok refuses it and posts nothing.
     There is no header question: nothing arrives. Grok also loads the
     hooks of installed Claude plugins.
   - Grok is waiting on you at a `Notification` whose
     `notificationType` is `permission_prompt`, for a tool ("Tool
     permission requested") and for a plan ("Plan approval
     requested"). 1.x does send `idle_prompt`, 60 s after a turn ends,
     and that is not waiting.
   - Grok's `Stop` comes at the end of every completed turn, with
     `reason: "end_turn"`. A rejected prompt and a Ctrl+C send
     `StopCancelled` instead (`permission_rejected`,
     `user_interrupt`), and an API error sends `StopFailure`. Quitting
     sends `SessionEnd` and then one more `Stop` with `reason:
     "shutdown"`, which must not read as a finished turn.
   - Grok reads `auth.json` at session start. A running session kept
     answering after the file was emptied and after its key was
     changed, with no reload in its log. The binary has reload code
     (it compares the token key), but nothing set it off in 45 s.
   - Codex on real TUI turns: `PermissionRequest` fires when the
     approval shows, `Interrupt` on Esc at the approval and on Esc mid
     turn, and neither is followed by `Stop`. `Stop` comes at the end
     of every completed turn, with `last_assistant_message`. `codex
     exec` forces approval to never, so it never asks.
   - A failed Codex turn (the configured `gpt-5.5` is refused on a free
     plan) sent `SessionStart`, `UserPromptSubmit` and then only
     `SessionEnd`, and `SessionEnd` has `reason: "other"` after a good
     session too. So a failed turn is a `SessionEnd`, or a new
     `UserPromptSubmit`, with no `Stop` since the last prompt.
   - Codex's `-c` flags must all sit on one side of the subcommand.
     Hooks given before `exec` were dropped without a word once
     another `-c` came after it.
   - The Codex TUI asks "Trust this folder?" the first time it opens in
     a folder, before any hook runs. A project's first Codex tile shows
     that question, and it is the user's to answer.
2. **`horadric hook`, the command hook. Done 2026-09-30.** `horadric
   hook codex|grok` posts the payload as it came with an
   `X-Horadric-Agent` header, and the listener reads it in that agent's
   shape through `Agent::event`: Codex's `prompt` becomes the prompt,
   Grok's camel case keys and snake case event names become Claude's.
   The event names are left as each agent sends them, for steps 3 and 4
   to give phases. It prints nothing, so an overlap warning does not
   reach these agents yet. Both agents need it, since
   neither can post to Horadric over HTTP. It reads the event on stdin
   and posts it with the tag from its environment, as `horadric status`
   does for the status line, adding which agent sent it. It is the one
   place a process is spawned per event, which Claude Code does not
   need, so it must start fast and never block the agent (a short
   timeout, fail open), and it does nothing when `HORADRIC_SESSION` is
   unset, so an agent started outside Horadric is not slowed.
3. **Codex. Done 2026-09-30.** Horadric passes its hooks on the command line it starts,
   `-c hooks.<Event>=[{hooks=[{type="command",command="..."}]}]` for each
   event, with `--dangerously-bypass-hook-trust`, since a hook nobody has
   reviewed in `/hooks` is skipped without a word. That keeps
   `~/.codex` untouched: nothing for `install` to add, a `codex` started
   outside Horadric runs no Horadric hook, and a dev instance and the
   installed one each pass their own. The cost is that the bypass also
   runs any unreviewed hooks of the user's own. `PermissionRequest` is
   waiting, `Stop` is done, `Interrupt` is idle, and a `SessionEnd`
   after a turn with no `Stop` is a turn that failed. Every `-c` flag
   goes before the subcommand, together, or the hooks are lost.
   `codex resume <id>` carries a paused tile on.
   Live check: through the npm `codex.cmd`, `SessionStart`,
   `UserPromptSubmit`, `Stop` and `SessionEnd` all arrived, and a `-c`
   after `exec` lost them all again, so `Agent::line` moves the
   session's own `-c` flags up beside the hooks. Codex runs only when
   `HORADRIC_AGENT` names it until step 5.
4. **Grok.** A `command` hook calling `horadric hook`, in
   `~/.grok/hooks/horadric.json`, added and removed by `install` and
   `uninstall`, the dev instance never. The Claude hook Grok borrows
   needs nothing: Grok 1.0.44 refuses it. Should a later Grok accept
   a loopback URL, the listener must still drop a Grok payload on the
   Claude path by its shape. Phases: `Notification` `permission_prompt`
   is waiting, `Stop` with `reason: "end_turn"` is done,
   `StopCancelled` is idle, `StopFailure` is failed, and the `Stop`
   with `reason: "shutdown"` after `SessionEnd` is ignored.
   `idle_prompt` changes nothing.
   Done 2026-09-30. The file is written only when Grok's home (`$GROK_HOME`
   or `~/.grok`) exists, so a machine without Grok gets no `~/.grok`, and
   `reload` rewrites it with the Claude hooks. Every entry says a 5 s
   timeout, since `Stop` is a gate Grok would otherwise wait ten minutes
   on. `StopFailure` reads its class from `error` and its text from
   `errorDetails`. The listener drops a payload with a camel case
   `hookEventName` that came in as Claude's. Live check: the same file,
   as a trusted project's `.grok/hooks`, ran `horadric.exe hook grok`
   unquoted with forward slashes on a real `grok -p` turn, and
   `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd` and the
   shutdown `Stop` all arrived tagged. Grok sessions start with step 5.
5. **Starting one.** The plus button and `horadric new --agent codex` start
   any agent found on PATH, Claude by default. A tile carries a small mark
   for its agent, so two sessions in one project can be told apart.
   History lists each agent's own conversations: Codex keeps them in
   `sessions/` by date, Grok in `sessions/<cwd>/<id>/summary.json`.
   Codex done 2026-09-30: `horadric new --agent codex`, and "New Codex
   session" in the project menu when `codex` is found (the plus stays
   Claude's). A paused tile resumes with `codex resume <id>`, without the
   prompt it was started with, which would be sent again. The defaults
   pass as `-m` and `-c model_reasoning_effort=`: a Claude model is not
   passed on, and Max becomes `xhigh`, where Codex's scale stops, until
   step 6 gives each agent its own lists. History reads each rollout's
   first line for its folder, the desktop app's too, with the name from
   `session_index.jsonl` or else the first prompt, and lists them among
   Claude's by date. Live check: a tile started, went done on "pong",
   and resumed its conversation after the dev UI and its hosts were
   killed, one `codex.exe` each time. The tile mark comes with Grok.
   Grok done 2026-09-30: "New Grok Build session" in the project menu and
   `horadric new --agent grok` when `grok` is found, on `PATH` or in
   `~/.grok/bin`, where its installer puts it. A paused tile resumes with
   `--resume <id>`, without the prompt it was started with. The defaults
   pass as `-m` and `--effort`, Max as `xhigh`, where Grok's scale stops
   too. History reads `sessions/<folder>/<id>/summary.json`, the folder
   percent encoded, titled by its `session_summary` or else the first
   prompt in `prompt_history.jsonl`. A tile that is not Claude's says
   "Codex" or "Grok" in small dim letters before its age; Claude's tiles
   carry no mark. Live check: a tile asked Grok's folder trust question,
   went done on "pong", and resumed its conversation after the dev UI and
   its hosts were killed, one `grok.exe` each time; a start with Max and a
   Claude model passed `--effort xhigh` and no model. Grok prints an
   "SSRF protection" line in its TUI for every event, from the Claude
   `http` hook it borrows from `~/.claude/settings.json`.
6. **Limits per provider.** The usage window gets a screen per provider
   in use, named this time ("Claude", "ChatGPT", "Grok"), since with more
   than one the numbers need saying whose they are. Codex's come from the
   newest `token_count` in the transcript its hooks point at, read on each
   `Stop`, and map straight onto `Limit`: `used_percent` and `resets_at`.
   Grok shows no limits until a source turns up. The settings follow the
   agent: each has its own models and efforts, and Default passes nothing.
   Done 2026-09-30. A screen for Claude always, and for Codex and Grok
   when they were found installed at start or have a session. With more
   than one, a line on top names the provider with a chevron, and a click
   on it goes on to the next; folded, the line stays, so the one limit
   left still says whose it is. The listener reads the rollout's last
   256 KB on a Codex `Stop` (`Limits::from_codex`) and the feeder keeps
   them per agent (`agent_usage` in `state.json`, Claude's stay `usage`,
   which goes with the account). A limit now keeps its `window_minutes`
   and is named by it: a free ChatGPT plan's one limit is 30 days long and
   reads "Month", not "Session". Each agent has its own lists
   (`Setting::choices`), Codex's models and efforts as its own picker has
   them (GPT-6 Luna to GPT-5.5, Low to Max), Grok's Grok 4.7 and Low to
   Extra high, and no permission modes, so its screen has Model, Effort
   and Version. They are saved as `agent_defaults`, passed as is, and never
   typed into a running session: only Claude Code has the commands.
   Tested with a dev instance: three named screens, a faked Codex `Stop`
   whose rollout said 41 % of five hours and 82 % of a week showed both
   under ChatGPT, GPT-5.5 picked from Codex's list and saved, Grok's
   slider with five stops and its screen saying it has no limits, and
   folded under a header. Not tried: a real Codex turn feeding the window.
7. **Accounts per provider**, built on the Claude switching above. Codex:
   the whole `auth.json` is the login, the account is `account_id`, the
   email is in the `id_token`. Horadric forces the file store if a user
   moved it to the OS keyring. A running Codex treats another account on
   disk as a permanent error, so a switch stops and resumes its sessions
   as a Claude switch does.
   Grok: the login is the one entry in `auth.json`. A running Grok did
   not notice a changed file in the live check, so a switch stops and
   resumes its sessions, as for Claude and Codex. A switch touches one
   provider's sessions and leaves the others running.
   Done 2026-09-30. Every provider's screen in the usage window has the
   Account row, and its list holds that provider's accounts alone, with
   the plan for ChatGPT ("Plus", from the `id_token`'s
   `chatgpt_plan_type`). An account now says whose it is (`agent`,
   missing reads as Claude, so `accounts.dat` from before loads), and
   Codex's and Grok's ids start with the agent's name. Codex's id is its
   `account_id` and the ChatGPT user, Grok's its `user_id` and `team_id`.
   The app follows each agent's login on its own (`Login`): watched,
   kept, put back after Add account, guarded for 15 seconds after a
   switch, and a switch waits only for that agent's sessions. Add
   account opens `codex login` or `grok login`. Every Codex start, resume
   and login gets `-c cli_auth_credentials_store=file`, the value bare
   since a non TOML value is taken as the string. A dev instance
   switches Codex only with `CODEX_HOME` set, Grok only with `GROK_HOME`.
   ChatGPT's limits go with its account as Claude's do.
   Tested with a dev instance, a fake `CODEX_HOME` and `GROK_HOME` with
   two made up logins each, and `codex.cmd` and `grok.cmd` stand-ins
   that post a `SessionStart` through `horadric hook`: a login written
   from outside was kept, the list named both with their plans, picking
   the other ChatGPT account stopped the Codex sessions only and resumed
   the prompted one with `resume <id>`, one process each, and a Grok
   switch picked while its session was mid turn said "after 1 turn" and
   went through at its `Stop`, `--resume grok-1`, Codex left alone. Not
   tested: real second subscriptions, and whether `codex login` or
   `grok login` revokes the login it replaces. Grok has a leader process
   (`grok leader`), and whether a resumed session reads the file again
   or gets the old login from a leader still running is for the first
   real switch to show.

**Not in this.** Cursor, Gemini and the rest, until one is asked for.
API keys and OpenRouter, asked to wait. Grok Build on Windows is
"best-effort" by its own README, so a failure there may be Grok's.

### Discord Activity

Asked for on 2026-10-01: Horadric shows on the human's Discord profile
what its agents are doing, the way a game shows "Playing". Discord calls
that line an activity, and a desktop app sets it through Rich Presence.
(Discord's other "Activities", web apps run inside a voice channel, are
not this: Horadric is not a web page and has nothing to embed.)

- **How it talks to Discord.** The desktop client listens on a named
  pipe, `\.\pipe\discord-ipc-0` up to `-9`, the first one free. Each
  message is a frame: an opcode and a length, both little endian `u32`,
  then that many bytes of JSON. Opcode 0 is the handshake
  (`{"v":1,"client_id":"<id>"}`, answered by a `READY` dispatch), 1 a
  command or its answer, 2 close, 3 and 4 ping and pong. The activity
  is set with `{"cmd":"SET_ACTIVITY","args":{"pid":<ours>,"activity":{...}},"nonce":"<n>"}`
  and cleared with `"activity": null`. No SDK and no dependency: the
  frames are `serde_json` and a `CreateFileW` on the pipe, which is less
  code than wrapping Discord's Game SDK DLL and keeps "pure Rust".
- **Its own thread.** The pipe is read and written on a thread of its
  own, so a slow or hung Discord never holds the UI thread. The app
  hands it the latest presence; it keeps only the latest, connects when
  it has one to show, tries the pipes again every 30 s while Discord is
  closed (a missing pipe is the normal case, not an error, and says
  nothing), and sends at most one update every 4 s, since Discord allows
  5 in 20 s and drops the rest. An unchanged presence is never sent.
- **What it says** (pure and tested, from the registry and the setting):
  `details` counts the sessions ("3 agents working, 1 waits for you"),
  `state` names the project on the stage, or the busiest, only when the
  human allowed names. The large image is Horadric's own, the small one
  the state of the most urgent session (waits, working, idle), the
  colours of the lamps. `timestamps.start` is when the current run of
  work began, so Discord counts up "for 1:12:04", and it holds while any
  session works instead of restarting at every turn. No session running:
  the activity is cleared, not "0 agents".
- **Off until turned on, names hidden until allowed.** Project names are
  the human's business and a Discord profile is public to their friends
  and servers. The tray has "Show on Discord" with Off (the default),
  "Without project names" and "With project names", checked as chosen and
  kept beside the other app settings. A dev instance keeps its own, so it
  stays off unless turned on there too, and two Horadrics showing at once
  fight over one activity.
- **Going away.** Quit, reload and the setting going Off clear the
  activity before the pipe closes, since Discord keeps showing a stale one
  for a while after the process is gone. A reload hands over: the old
  build clears, the new one sets it again.
- **The Discord application.** Rich Presence needs an application in
  Discord's developer portal, whose name is what the profile shows
  ("Playing Horadric") and whose id goes in the handshake. The id is
  public, so it is a constant in the code, `1555242626897416212`, made by
  the human on 2026-10-01; `HORADRIC_DISCORD_CLIENT_ID`
  overrides it for testing. Creating the application was the human's
  step. The images are uploaded to it as Rich Presence art assets and
  named by key, unless Discord takes an `https` URL for them, which the
  art quest checks first; then they live in this repository.

Built as quests, in the quest log under "Discord Activity": the pipe
client, what the presence says, the art and the tray setting side by
side in the shared tree, then wiring them together, then the check on
screen with a real Discord.

Done, all of it. Checked on screen on 2026-10-01 with a dev instance,
fake sessions posted to it and the real Discord desktop client, the
profile popout read from screenshots: "Without project names" showed
"Playing Horadric, 1 agent working" with no project line; a second
session working and the first asking for permission gave "1 agent
working, 1 waits for you"; both stopping gave "2 agents idle"; a new
turn after that kept the clock going (0:15, 0:45, 1:15 across all of
it) instead of starting at zero. Picking "With project names" in the
tray added "in beta" within the 4 s, Off took the card away at once,
and on again brought it back. Reload cleared it for the 3 s of the
handover and the new build set it again; Quit, with "Stop them", cleared
it within 2 s. Two things the check left: the images show as Discord's
question mark until `docs/discord` is on `main` on GitHub, since their
URLs are raw GitHub ones, and a reload started the clock again, since
the run of work was not handed over. It is now: the run is kept in the
saved state (its last work cut to the minute, so a working session does
not write the file every tick), and a start finds it there and carries
on, unless the ten minute gap passed meanwhile.

### Orchestration

Asked for on 2026-10-01. Proposed, not built. The goal is development
that runs itself across many sessions, with the human hearing only
about what needs a human. Today the human is the orchestrator: with
several sessions in a project, a quest that cannot go on because it
waits on another sits `[!]` until the human notices, works out what it
waits on and starts it again. "Blocked quests resume by themselves" in
the quest log fixes part of that. It is not the whole answer, and this
section says what is.

**Why a blocked quest is the wrong place to start.** When quest B waits
on quest A, the dependency was there before either started. B finds out
halfway in, after a session and some of its context are spent. The
Runetome tile is the example on our own log: it was taken, worked until
it saw that two quests above it had not landed, and blocked. Resuming
it once they land is a good safety net. Not starting it until they land
is the fix. So the work splits in three layers, each useful alone, built
in this order.

**1. Dependencies in the quest log, declared before work starts.**

This reopens "No dependencies" in The task list, so it waits for the
human's yes before it is built.

- **The form.** A notes line `After: <title>` names a quest this one
  waits for, one line each. The title is matched exactly or by a unique
  start, since titles are long and carry colons. A name that matches
  nothing or several quests makes the quest not ready, and its row says
  which name, so a typo never starts work early. The file stays the
  state; nothing is kept beside it.
- **Ready.** A quest is ready when every quest it names is `[x]`. Not
  `[?]`: in review the work may not be on `main` yet, and with worktrees
  it is not.
- **The runner picks the first ready open quest**, not the first open
  one. A quest waiting on another does not stop the list, it is passed
  over, which is what `parallel` needs: with three slots and a chain of
  three, one runs and the other slots take quests that do not wait. A
  cycle is not ready either; its rows say so and the list stops there,
  since only a human can break it.
- **Who writes them.** The quest giver's prompt says to add `After:`
  lines when a suggested quest needs another, and a planning quest that
  writes its own quests below itself does the same. `quest add "title"
  --after "other"` writes the line.
- **"Blocked quests resume", cut down.** `quest blocked "why" --on
  "title"` marks the quest `[!]` and writes the same `After:` line. When
  that quest is `[x]`: a live holding session is told to go on, typed
  in as `go_on` does after a usage limit, and the quest goes `[/]`; a
  session that is gone starts again (`start_again`). One rule with two
  ways in. Waits on a file, a command or a time are left out until a
  quest needs one. A plain `blocked "why"` still stops the list for the
  human.
- **The board row** of a quest that waits reads "after <title>", dim,
  with no lamp, since nothing runs.
- **Pure and tested** in `horadric_core::tasks`: reading `After:` lines,
  matching titles, ready, cycles, the runner's choice, the `--on` form.
  On screen with a dev instance and `cmd.exe`: two quests, the second
  after the first, auto mode, `quest done` on the first starts the
  second. Count `claude.exe` after, since this starts agents.

**2. An orchestrator session, woken by events.**

Some blocks no rule can clear: an agent asks a question another agent
could answer, two quests turn out to overlap, a merge conflicts, a
quest is too big and should be split. Today each of those goes to the
human. Most need judgment, not a human in particular. That judgment
lives in an agent; Horadric stays the plumbing that wakes it, which
keeps the runner small and testable and keeps state coming from hooks
and the file.

- **One per project, named Warriv** (the caravan master in the games,
  who moves the camp on when the way is clear), a session on the stage
  like the quest giver. It holds no quest and the runner never gives it
  one.
- **Woken by events, not left running.** A quest goes `[!]` without
  `--on`; a session on a quest reads "asks you" after the nudge; a quest
  names a dependency that matches nothing, or a cycle; a merge into
  `main` fails; the log runs out of ready quests in auto mode while some
  wait. Not every `Stop`: that would make it a second runner.
- **A fresh session per wake**, for the reason the runner gives each
  quest one: carrying a session from event to event fills its context.
  Its first prompt is the event, the quest, the reason and the holding
  session's last turn. Its memory is the quest log: it writes what it
  decided as a notes line on the quest (`Warriv: split into the two
  below`), so the next wake reads it. Events that arrive while it works
  wait and go to it at its next `Stop` in one prompt. When it has no
  event left and is not mid turn, it closes.
- **What it may do.** Change the log (`quest add`, `--after`, notes,
  move, put back), answer a holding session with `quest tell "title"
  "message"` (typed in once the session is not mid turn, the same way
  `go_on` types), and hand the event to the human with `quest blocked
  "question"` on the quest, worded so the human can answer in one line.
  It changes no code and starts no session; the runner starts what the
  log says, with its fuses.
- **Fuses.** At most one Warriv a project. At most six wakes a project
  an hour, then events go to the human as today, with a notification
  saying why. It is never woken by its own changes. A `quest tell` to a
  session goes once per event. Its permission mode is the project's, and
  a prompt it stops at is an "asks you" like any session's.
- **Off by default.** `"orchestrator": true` in `.horadric/config.json`
  turns it on. Without it, every event goes to the human as today.
- **Pure and tested:** which events wake it, the wake budget, the first
  prompt, the queue of events. On screen with a dev instance: a fake
  quest blocked with a question its notes answer, Warriv wakes, tells the
  session, the quest goes on. Count `claude.exe` after.

**3. The human hears only what Warriv could not settle.**

Not an inbox: that was dropped on 2026-09-26, and the tiles, the hotkey
that walks waiting sessions and the notification still give the
overview. What changes is what reaches them. With Warriv on, a quest is
red only when it handed the question on, and its reason is that
question. "N quests need you" counts only those. The notes say what
Warriv tried, so the human answers without reading the session first.

**Open questions.**

- Whether Warriv may review `[?]` quests in review mode, as a first
  reader before the human. It would make review mode run alone for
  longer, and it is the first place a wrong judgment costs real work.
- Whether a merge conflict between worktrees is Warriv's to resolve, or
  a quest it files for a worker. The second keeps "changes no code".
- Which model Warriv runs on. Its turns are short and many, which says a
  small one, but a wrong call costs more than the turn saves.

### The agent's cursor

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

Built so far: the project key resolves to the main working tree, a project
works in trunk mode or with a branch per session, a session in a worktree
has a tile that shows what it changed, and sessions in one tree are told
when they edit the same file.

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
  be kept out of git: `"worktrees": {"enabled": true, "setup": [...],
  "ports": 10}`. Without `enabled` the project is in trunk mode, see work
  modes below, and `setup` and `ports` apply to the worktrees it asks for.
- **Work modes.** A worktree for every session was the default, and it
  got in the way. The human mostly works on one branch, committing and
  pushing often, and says "build", "show on dev" or "ship" expecting all
  of today's work. With a worktree per session that work sat on branches
  not merged yet, or not committed, and it was hard to tell which. So a
  project now works in one of two modes, picked under Work mode in the
  project menu. **Trunk**, the default: every session starts in the main
  tree, and its system prompt says the tree is shared, to commit small and
  often on the branch that is checked out, to stage only its own files by
  name and never `git add -A`, to push when there is an upstream, and not
  to make branches unasked (`worktree::trunk_prompt`). A worktree is had by
  asking: "New session in its own worktree" in the project menu, tombs,
  and task items run side by side, which get one in either mode since
  they would edit the same files. **Branch per session**: every new
  session gets a worktree, as below. Switching writes `enabled` into the
  config and changes only the sessions started after.
- **Overlaps.** In a shared tree two agents can overwrite each other. The
  listener claims each file a session edits (`Edit`, `Write`,
  `MultiEdit`, `NotebookEdit`) for it until it commits or ends, or four
  hours pass (`overlap::Claims`, pure and tested). An edit to a file
  another session claims is answered, in the reply to that `PostToolUse`
  hook, with `additionalContext` telling the agent another session
  changed the file and has not committed, to look at `git diff` before
  changing it further, keep what is theirs and stage only its own
  changes. Once per file and set of others, so an agent editing a file
  ten times hears it once. The app shows a toast with both sessions'
  names, and a click shows the one that edited last. Paths are compared
  whole, so sessions in worktrees of their own never overlap. The
  installed Horadric hears a dev instance's hooks first and answers them,
  and toasts only for sessions it holds.
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
  A dev instance knows only its own sessions, so to it a worktree another
  instance's agent works in looks free, and a test session in a worktree
  of this repository once swept other agents' merged, clean worktrees
  from under them. So a dev instance leaves a `horadric-dev` file in the
  git folder of each worktree it adds (git keeps that folder per worktree
  and removes it with the tree) and sweeps only worktrees that carry it.
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
- **Choosing how many at once.** The mode menu lists "One at a time" and
  2 to 4 at once below the modes, the current one checked, and writes
  `parallel` into the config beside the mode (one takes the key out). A
  project without a worktree per session shows one greyed line saying
  why instead. Where the runner runs, the mode key says the number too,
  "Review ×3". Asked for because the setting lived only in the config
  and the user never found it. Tested on screen with a dev instance and
  `HORADRIC_AGENT=cmd.exe`: review mode with `parallel` 3 started three
  items 10 s apart, each in its own worktree, and the key read "Review
  ×3". The menu itself was not clicked, since a synthetic click opens it
  behind other windows.
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

### Fleets

Some projects manage hundreds of devices over SSH, a fleet of IoT boxes
rather than a few servers. SSH hosts was built for a handful and breaks
there in three places: the project menu lists every host (and stopped at
80), each connection is a tile and a pane, and every agent's prompt names
every host, hundreds of names on every session for the few it touches.
Asked for because that is how some of the human's projects work.

- **An inventory, not a list.** `"inventory": "devices.ini"` in
  `.horadric/config.json`, a path from the project's folder or absolute.
  The fleet already lives in a file like it, so Horadric reads that
  rather than a copy in `hosts`. The format is Ansible's INI, which a
  plain list of names also is: `[group]` headings, one device a line, its
  name first, and `ansible_host`, `ansible_user` and `ansible_port` when
  the name alone does not reach it. `:vars` and `:children` sections are
  skipped. A port becomes an `ssh://user@host:port` destination, the one
  form `ssh` takes a port in. YAML inventories are out: a parser is a
  dependency, and INI covers the common case.
- **Found by name, not listed.** With an inventory, or more than eight
  hosts, the project menu has one "SSH to..." in place of a line a host.
  It opens the question dialog with the hosts and the devices as
  suggestions, each with its group, found as it is typed by every word in
  the name, the group or the address, names that start with it first. The
  best match is lit, so Enter takes it, and anything else `ssh` takes
  connects too when nothing matches. "Add host" does the same with the
  `~/.ssh/config` names when there are more than a submenu reads well.
- **A device names its terminal.** "SSH 7" says nothing among dozens, so
  a terminal opened on a device from the inventory is named after it. The
  destination is its second line, and a restart brings it back by both.
  Terminals are opened one at a time, on demand, so a fleet of 240 is
  still a few tiles.
- **The agent is told where, not which.** Its prompt gives the
  inventory's path, how many devices and the groups (twelve named, the
  rest counted), how to read a line, to look a device up before reaching
  it, and to loop with `-o ConnectTimeout=10` so a device that is offline
  does not hold up the rest. Past twenty, `hosts` are counted the same
  way.
- **Pure and tested:** reading the inventory and its path, destinations
  with users and ports, finding, the groups, the prompt, counting many
  hosts, and what a typed host is refused for.

Done, all of it. Seen on screen with a dev instance and a project of 240
devices in three groups plus one host: the menu had "SSH to..." and no
host lines, the picker said 241 to pick from with groups beside the
names, "cam 07" lit camera-007 first, Enter opened a tile named
camera-007 running `ssh -- pi@camera-007.invalid`, and its refusal kept
the pane with the tile paused. The picker first drew the folder dialog's
folder icons and Tab added a `\`, so a pick now says its icon and whether
its suggestions are paths. Checked with `horadric run` on haiku: told
nothing else, the agent knew the count, groups and path, and asked for
the command for camera-012 it read the inventory and gave
`ssh.exe -o BatchMode=yes pi@camera-012.invalid 'uptime'`. The first
wording let it guess the bare name, hence the line about looking a device
up. Not tested: a real device, for want of one, and running a command
across many.

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

- The inbox, a list of the sessions waiting on you, is dropped (decided
  2026-09-26). The tiles, the hotkey that walks the waiting sessions (see
  The stage) and the notification already give the overview.
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
<version>" and, once per release (`update_told` in the saved state), a
notification with the first paragraph of its notes
(`release::teaser`). The item and a click on the notification both show
all the notes, and install only on "Update now" (decided 2026-09-29:
the tray item alone told no one, and the signed notes were never shown).
A check that finds this build up to date takes the item away again; a
failed one leaves it. The install is built too:
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

- **Expanding from a synthetic click can open behind other windows.** Windows
  only lets a process take the foreground after real input. A real click on
  a tile is real input, so this only bites scripted tests.
- **New tray icons start hidden.** Windows 11 puts them behind the `^`
  overflow until the user drags them out or turns them on in Settings.

## Open questions

Carried from the concept, with what is known now.

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

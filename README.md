# Horadric

Every coding agent session you have running becomes a small tile on your
Windows desktop, grouped by project, that lights up when it needs you and
grows into a full terminal when you click it. Sessions share one working
tree and commit on it, or, if you pick that work mode for a project, each
works in a git worktree of its own and the tile shows you what it changed.
When two sessions in one tree edit the same file, both you and the agent
hear about it.

Horadric is a window manager for agent sessions that already exist. It never
puts its own chat UI in front of the agent. The terminal is the UI.

Status: early. The tiles, the state stream behind them and the terminals
work. Worktrees are next.

## What works now

- `horadric` opens one cluster window per project on the left edge of your
  screen: frameless, rounded, never takes focus, not in the taskbar or
  alt-tab. It stacks like a normal window: other windows can cover it, a
  click brings it forward, and "Bring tiles to front" in the tray menu
  brings them all back. The clusters stand in columns as tall as the
  screen, and the files tiles in a column share the room the tiles leave.
  Drag a cluster to another place, another column or a new one at the
  right. A new project never moves the others, and another screen lays the
  same columns out again. The wheel scrolls a column that holds more than
  fits. Click the header to collapse a cluster.
- Each tile shows a session: an icon for its state, name, an age line like
  "needs permission 12 min", and the last line worth reading. Waiting tiles
  light up amber.
- A project in git gets a files tile at the bottom of its cluster: the file
  tree as VS Code's explorer shows it, changed files coloured with their
  letter (M, A, U, D), folders marked when something inside changed. It
  updates as files change, without polling. Click a folder to open it, a
  file to open it in VS Code, the header to fold it.
- A tray icon starts sessions: "New session..." asks for a folder, and your
  recent projects are one click away. It also quits Horadric.
- `horadric explorer install` adds "Open in Horadric" to folders in Explorer,
  which starts Horadric too if it is not running.
- A session runs `claude` in a Horadric terminal: a real terminal with the
  real CLI in it, permissions, slash commands and all. The sessions share one
  terminal window, the stage, which shows one project at a time: every
  session of that project is a pane in a grid. One fills the window, two
  sit side by side, four are two by two. Click a tile and the stage switches
  to its project with that session typing; click it again or close the
  window to collapse it. Other projects keep running unseen, and the tiles
  on stage are lit. The `+` in a cluster header starts another, and
  `horadric new --name fix-login` does it from a script.
- Drag a pane by its header onto another to swap the two. The order is
  remembered. "Fit terminal beside tiles" in the tray menu fills the space
  right of the clusters. The terminal snaps to the screen edges and the
  tiles, when moved and when an edge is dragged to resize, and Shift
  places it freely.
- Plain terminals for everything that is not the agent: a dev server, a
  log, a deploy. The small button beside a cluster's bottom `+`, Ctrl+Shift+T
  in any pane, or "New terminal" in the project menu opens PowerShell in the
  project, as a pane on the stage and a tile in the cluster. The tile shows
  what the terminal's title says and how busy its output is. `exit` closes
  it. `HORADRIC_SHELL` picks another shell.
- Zoom a pane with the button at the end of its header, a double click on
  the header, or Ctrl+Shift+Enter: it fills the stage alone until you zoom
  out. Ctrl+Alt and an arrow moves the keyboard to the pane beside it. Ctrl
  and plus, minus or the wheel sizes the font, Ctrl+0 puts it back.
- Ctrl+Shift+F searches a pane's history, which keeps 10,000 lines. Enter
  finds the next match up, Shift+Enter the next one down, Esc closes it.
- A browser per project: Ctrl+Shift+B in any pane, or "Browser" in the
  project menu, opens Edge on a profile of the project's own, beside the
  stage. It hides when the stage shows another project and comes back
  with its own. Pressed again, it brings that window to the front.
- Ctrl+click opens what a pane shows: a web address in the browser (the
  project's own when it has one open), a
  file in VS Code at its line (`src/main.rs:12`, relative to the session's
  folder), a folder in Explorer, a program by running it. Hold Ctrl and
  the link under the mouse is underlined.
- A quest log per project: `.horadric/quests.md` (a `tasks.md` from
  before the rename is still read), a Markdown checklist in the repo,
  shown in a quests tile in the cluster. Click a quest and a session
  starts on it, with the quest as its prompt; the agent reports back with
  `horadric quest done` or `horadric quest blocked "why"`, and the row
  shows where it is. The mode button picks how the list is worked: Manual,
  one click per item; Review, the next item starts once you approve the
  last; Auto, down the list until it is done. The `+` adds an item, and so
  does `horadric quest add` from any shell.
- Right click a tile to rename its session, or a project's header to open
  its folder in VS Code or Explorer.
- Ctrl+Alt+Space, from anywhere, shows the session that has waited on you
  longest. Press it again to move on to the next one. When a session starts
  waiting and you are not looking at it, Windows shows a notification; a
  click on it shows the session. The tray menu can switch that off.
- A Claude window at the top of the stack shows how much of your five hour,
  weekly and spend limits is used and when each resets, and picks the
  model, effort and permission mode for every session Horadric starts or
  resumes. Each tile shows how full its session's context is. The numbers
  come from Claude Code's status line, which Horadric sets for its own
  sessions only.
- `horadric hooks install` adds Claude Code hooks to `~/.claude/settings.json`.
  They are `http` hooks: Claude Code posts each lifecycle event to Horadric on
  localhost. No script runs, no process is spawned per event.
- `horadric run --name fix-login` starts a tagged `claude` in the terminal you
  are in instead. Its tile shows state but can not expand. A `claude`
  started any other way is ignored.
- `horadric serve` shows the same state as a table in the terminal.

The whole app is one process. With two projects and four sessions on screen
it uses about 45 MB and no CPU between events. The terminals are ConPTY,
`alacritty_terminal` for the grid, and our own DirectWrite glyph renderer.

## How it knows

Horadric sets `HORADRIC_SESSION` in the environment of every `claude` it starts.
The hook is configured to send that variable as a header, so every event
arrives already tagged with the tile it belongs to. Outside Horadric the header
is empty and the event is dropped.

State comes from hook events, never from reading terminal output:

| Event | State |
|---|---|
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | working |
| `PermissionRequest`, `Notification` (permission, idle, dialog) | waiting |
| `StopFailure` | waiting, with the error |
| `Stop` | done |
| `SessionEnd` | ended |

Subagent events and compaction restarts do not change state.

## Build

Rust stable on Windows.

```
cargo build --release
target\release\horadric.exe install
```

That installs Horadric for your user, no admin rights: it is in the Start
menu (search "Horadric"), `horadric` works in any new terminal, "Open in Horadric"
is on folders in Explorer (under "Show more options" on Windows 11), it
starts with Windows, and the Claude Code hooks are in place. Horadric lives in
the tray; Windows may hide a new icon behind the `^` by the clock.

Each session's console runs in a small host process of its own, so the
agents keep running when Horadric crashes, reloads, or quits with "keep
running": the next start finds them where they were, screen and all.
Sessions whose process is gone (a restart of Windows, or Quit with "stop
them") come back as paused tiles, and a click resumes the conversation.

`HORADRIC_AGENT=cmd.exe` runs a shell instead of `claude` in the terminals,
which is the cheap way to try them.

`HORADRIC_DEV=1` runs a build beside the installed Horadric without touching
it: its own port and saved state, no autostart, a red lit tray icon. That is how
Horadric is developed from a session inside Horadric.

`target\release\horadric.exe reload` updates a running Horadric to a new build
without the quit. It hands over at once and the new build attaches to the
running sessions, mid turn or not. A build that fails to start is rolled
back.

`horadric uninstall` takes it all back out. The hooks it removes are only
Horadric's; everything else in your Claude Code settings stays as it was.

## Plan

1. Hook receiver and state machine. Done.
2. Cluster windows on Win32 and Direct2D. Done.
3. Terminal window: ConPTY, the alacritty grid, our own glyph renderer. Done.
4. Worktree per session, changed files, open in VS Code.
5. Inbox, installer, updater.

Pure Rust, Win32 and Direct2D directly. No web view, no toolkit.
[docs/PLAN.md](docs/PLAN.md) has the detail, the open questions and the
known gaps.

## Not doing

- A chat UI of its own.
- Progress bars or time estimates.
- An embedded editor or diff viewer. VS Code does that.
- Merging worktrees back. Git and the human do that.
- Mac or Linux, for now.

## License

MIT.

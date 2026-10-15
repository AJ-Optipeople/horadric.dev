# The usage window

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

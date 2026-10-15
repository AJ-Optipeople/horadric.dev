# Persistence

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

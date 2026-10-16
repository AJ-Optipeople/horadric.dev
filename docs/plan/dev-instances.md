# Dev instances

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

Two dev instances in two worktrees may be given the same port. The second
quits ("already running"), and its `horadric new` once started a `claude`
among the first one's tiles. So a command that chose its port itself
(`new`, `reload`, Explorer's "Open in Horadric", `quest` outside a
session) sends its state folder as `X-Horadric-State`, and an app keeping
its state elsewhere answers 409 with its own folder named. A session's
`quest` and `mcp` post to the owner they were given and send none, and a
request without the header is taken at its word, so older builds still
work. Checked with a dev `serve` on port 4111 and `horadric new` from two
state folders: the other one was refused by name, the same one got through.

Tested on screen: a dev instance beside no installed one, a `cmd.exe` tile,
state written to `Horadric-dev` only. The hand off between two running
instances is covered by a listener test, not yet seen with a real `claude`.

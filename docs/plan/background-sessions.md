# Background sessions

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

# The stash

"The stash" in [IDEAS.md](../IDEAS.md). History has every conversation, and
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
  holds something. A list in a well sunk into the plate (`layout::stash`,
  tested), a row per stashed session the column's full width, so the
  window grows and shrinks with what it holds. A row is a key with its
  name in its item colour, and under it a lamp in the project's accent,
  the project and the last thing it said. Resting the cursor on a row
  shows all of it (`tip::stashed`, tested): name, project and branch,
  the whole last line, then what a click does. "Stash" and "n of 9"
  above. It was a three by three grid first, and a third of the column
  cut every name to a word.
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

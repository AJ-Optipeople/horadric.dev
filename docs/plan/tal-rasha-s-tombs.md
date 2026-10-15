# Tal Rasha's tombs

"Tal Rasha's tombs" in [IDEAS.md](../IDEAS.md). Seven tombs and only one is
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

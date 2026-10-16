# Runewords

"Runewords" in [IDEAS.md](../IDEAS.md). A named sequence of actions a session
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

# Transmute

"Transmute" in [IDEAS.md](../IDEAS.md). The Horadric Cube: tiles dropped in
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
  finished. The recipes it could grow into are in [IDEAS.md](../IDEAS.md).
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

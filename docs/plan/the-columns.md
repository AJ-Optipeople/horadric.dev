# The columns

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

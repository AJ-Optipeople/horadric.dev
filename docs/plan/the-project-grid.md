# The project grid

The stage shows every session of one project at once, as panes in a grid
(`layout::grid`): one fills the window, two sit side by side, three are two
above one wide, four are two by two. Asked for so a project reads as one
piece of work, with less freedom and more structure.

- **A pane is a child window** (`pane.rs`) with its own render target,
  keyboard and mouse, the old terminal window's insides. The stage
  (`terminal.rs`) is the frame: title, snapping, layout, and the drag.
  Child windows rather than one window drawing every grid, because focus,
  capture, dropped files and the caret then belong to the right session by
  themselves.
- **A tile click switches the project** and gives that session the
  keyboard. A click on the tile of the session typing, with the stage in
  front, collapses it. The hotkey for the next waiting session does the
  same switch.
- **Which sessions are panes**: every console of the project except a
  clean exit. A crash keeps its pane, showing what went wrong, until the
  tile resumes it into the same place. Paused sessions have no pane.
- **The stage follows the registry.** `reconcile` ends in `sync_stage`, so
  a session started in the project on stage joins it, an ended one leaves,
  and an empty stage closes. `TerminalWindow::show` keeps the panes it
  already has, with their selection and scroll, and lays out again only
  when the set changed.
- **Headers.** Every pane has a header: the session name and what the
  agent says it is doing, with a line of the phase's colour along its
  top. No dot before the name: every app has one, and the line already
  says it. The one with the keyboard is underlined in the project's
  colour and the others step back. At its right end sit stash, zoom and
  a cross that ends the session (asking first while an agent runs). A
  pane alone has one too, for those buttons, but no zoom.
- **Drag to swap, live.** Pressing a header gives the stage the mouse
  (`WM_PANE_GRAB`, then `SetCapture` on the stage). Past 4 pixels the pane
  lifts and follows the cursor, and the pane whose cell it comes over
  glides into the cell it left, so the grid shows the swap before the
  mouse is let go. Letting go glides the dragged pane into its cell, its
  centre kept, and the swap goes through the app (`Input::Swap`), which
  owns the order. The stage swaps its own panes at once, so the app's
  `show` of the new order changes nothing.
- **What keeps a drag cheap.** Every grid is held at its size
  (`Pane::hold`) until the drag ends, so a pane passing through a cell of
  another size never reaches its agent as a resize and a redraw. The
  lifted pane floats as a window of its own, owned by the stage
  (`Pane::float`): a child moved across the stage damages the stage and
  every pane it passes over, a popup is only moved by the compositor.
  The pane is carried on the vsync clock, not on each mouse move, so a
  1000 Hz mouse still moves it once a frame, and it is kept on the stage.
  A pane's render target keeps its last frame, and a paint Windows asks
  for when nothing shown has changed (a neighbour uncovered a strip) only
  presents it again, about half the cost of drawing. Measured on a 2x2
  grid in a release build, a drag costs about 2 ms of CPU a frame; with
  the pane left a child window it cost twice that.
- **The order is kept** per project in `grids` in `state.json`. A paused
  session keeps its place, so a restart puts each back where it was as it
  resumes (`layout::grid_order`). A reload brought a swapped grid back
  swapped.
- **Tiles and panes share one order.** It lives in `Shared::orders`, so
  the cluster sorts its tiles by it (`layout::rank`) and the stage lays
  out its panes by it. Every session gets a place as the registry
  changes, new ones last in the order they started, so a tile and its
  pane agree from the first. A pane swap moves the two tiles too.
- **Drag a tile to move it.** In a cluster of more than one tile, pressing
  a tile and moving past 4 pixels lifts it: it follows the cursor up and
  down, solid and outlined blue, and the others slide out of its way.
  Letting go puts it in the place it is over (`layout::tile_slot`) and
  the stage's grid follows (`Input::Reorder`). A lone tile still drags its
  window, and the header drags any cluster. Asked for so the tiles read
  in the same order as the panes, and either can be arranged.

Tested on screen with a dev instance and three `cmd.exe` sessions: tiles
and panes started in the same order; dragging the bottom tile to the top
moved its pane to the first place; the order came back after a restart
with every session paused; a plain click on a tile still resumed it; a
pane swap swapped the two tiles.
- **Pop out is gone**, with `popped` and `placement` in `state.json`. An
  older file still reads; those fields are ignored.
- The tray's "Arrange terminals" became "Fit terminal beside tiles".

Tested on screen with a dev instance and `cmd.exe` sessions: three in one
project laid out two above one; typing reached the pane clicked; `exit` in
one left the other two side by side with the keyboard moved over; a tile in
another project switched the stage, and a second click collapsed it;
Alt+F4 inside a pane closed the stage; the drag swapped two panes and the
reload kept them swapped.

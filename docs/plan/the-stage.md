# The stage

Ten sessions open used to mean ten terminal windows piled on each other.
Now they share one, the stage, which shows one project at a time.

The first version showed one session and swapped it on a click, with a
"pop out" for sessions wanted side by side. Replaced, see The project
grid below: switching sessions one at a time hid the rest of a project,
and popped windows were the freedom that made the desktop a pile again.

- **The project on stage is lit**: its cluster has an edge in the project's
  colour, the same colour as the stage's border (see The look).
- **Where the stage goes.** A new session docks it against the tiles as a
  square filling top to bottom: its top and bottom at the margin, its edge
  a gap from the box around every cluster on the primary screen, on the
  side with more room, narrower only when that side is (`layout::beside`,
  `layout::square`). The first time, it opens the same way. A resume, a
  tile click or the hotkey leaves it wherever it was left (`stage` in
  `state.json`).
- **Next waiting session.** Ctrl+Alt+Space, from anywhere, shows the session
  that has waited on you longest. Pressed again while that one is in front,
  it moves on to the next (`inbox::next`), so one you are not ready for does
  not block the rest. A dev instance uses Ctrl+Alt+Shift+Space so both can
  hold one. Also in the tray menu, with the shortcut beside it.
- **Fit terminal beside tiles** in the tray menu fills that same space
  beside the clusters with the stage, by its visible edges: a Windows 11 window
  has an invisible resize border that would otherwise double every gap.
- **Terminals snap** to the work area edges and the other Horadric
  windows, with the margin and gap the columns use: when moved
  (`WM_MOVING`, `layout::snap`) and when an edge is dragged (`WM_SIZING`,
  `layout::snap_edges`). Both measure visible edges (`snapping::others`),
  and Shift places it freely. The system loops build each rect from the one returned
  last, so a snapped rect snapped again on every small step and the window
  never let go. The free rect comes from the cursor instead, at the
  distance from each edge it had when the drag began.
- The windows to snap to are found by class name, so a dev instance also
  snaps to the installed Horadric's windows. Harmless, and useful: that is
  where they are on screen.

Found while testing it: a click on a tile made the cluster the foreground
window, `WS_EX_NOACTIVATE` or not. So "is this terminal in front?" was
always no at click time, and the second click that should collapse a
terminal only brought it forward, before the stage too. The cluster now
answers `WM_MOUSEACTIVATE` with `MA_NOACTIVATE` itself.

A test trap, not a bug: an app started with `Start-Process -WindowStyle
Hidden` has its first `ShowWindow(SW_SHOWNORMAL)` turned into a hide by
Windows, so the first terminal never appears. Start dev builds with
`-NoNewWindow` instead. `horadricw` and the `Run` key are not affected.

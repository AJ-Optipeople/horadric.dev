# Step 2: cluster windows

One frameless window per project, standing in columns down the left edge
of the work area (see The columns), drawn with Direct2D and DirectWrite. The Win32 rules that make it feel
native rather than like an app window:

- `WS_EX_NOACTIVATE` so clicking a tile never takes focus from the editor.
- `WS_EX_TOOLWINDOW` so it stays out of alt-tab and the taskbar.
- `SWP_NOACTIVATE` on every move, resize and raise.
- Dragging handled by hand, because the system move loop activates.
- A dragged cluster takes the place in the columns it is let go over.
  Placing it freely, with snapping and Shift, was dropped for the columns.
- DWM rounded corners and a suppressed border, so it matches Windows 11.
- Not topmost. It stacks like a normal window, so an editor can cover it.
  A click raises it by going topmost and straight back, since a plain
  `HWND_TOP` from a background process stays below the foreground window.
  "Bring tiles to front" in the tray menu raises them all. It used to be
  topmost and raised on every phase change, which put it over everything.
- The tiles come up with the stage. Whenever the stage activates (its
  taskbar button, alt-tab, a click on it) every tile window is raised
  too (`Input::StageActive`), so Horadric comes forward as one app. Asked
  for on a laptop, where the tiles were only ever seen by closing every
  other window. Checked on screen: notepad over the tiles, the stage
  activated, the tiles in front of notepad.

Measured: one process, about 45 MB with two clusters and five tiles, no CPU
between events.

# Quality of life

Seven things found by reading the app for what daily use would miss first.

- **A notification when a session needs you.** The tiles are not topmost,
  so an amber tile under an editor went unseen. When a session starts
  waiting (`Phase::Waiting`, never idle after a finished turn), the tray
  icon sends a notification: "fix-login needs permission" over the tool it
  asks about (`inbox::alert`). Several at once are one, "3 sessions need
  you". Nothing is sent for a session you are looking at, the stage in
  front with it on it. A click shows that session, or for several the one
  that waited longest (`NIN_BALLOONUSERCLICK`). "Notify when a session
  needs you" in the tray menu switches it off, saved as `quiet`. It is a
  `Shell_NotifyIcon` balloon, which Windows 11 shows as a toast and keeps
  in the notification centre, so Do not disturb holds it back by itself.
- **Zoom a pane.** Its header has a button at the end, and a double click
  on the header or Ctrl+Shift+Enter does the same: the pane fills the stage
  alone, the rest hidden at their size so their programs are not told of a
  resize. Zoom follows the keyboard: a tile click or Ctrl+Alt+arrow shows
  that pane instead, so zoomed reads as one pane at a time. Switching
  project ends it.
- **The keyboard between panes.** Ctrl+Alt+arrow moves it to the pane in
  that direction (`layout::neighbour`, the nearest one past the edge, most
  in line). Ctrl+Alt, not Alt, because Alt+arrow is word movement in a
  shell. The stage chords are one pure table (`keys::chord`), and a chord's
  own `WM_CHAR` is taken off the queue so it never reaches the program.
- **Font size.** Ctrl and plus or minus, Ctrl+0 for the default, or Ctrl and
  the wheel, one DIP a step from 9 to 32 (`keys::font_size`). One size for
  every pane, saved as `font_size`. The `Font` makes new text formats for
  the size; the glyph cache keeps, since glyph indices do not depend on it.
- **Rename a session.** "Rename..." in a tile's menu asks in a plain Win32
  dialog built from a template in memory (`ask.rs`), so no resource file
  ships. The name beats Claude's title (`Session::renamed`, saved) until a
  newer `/rename`, and an empty one gives the naming back to Claude.
- **Search the history, and more of it.** Ctrl+Shift+F opens a bar over the
  pane's top right corner. Typing searches up from the bottom, literally
  (`find::pattern` escapes what a regex would read), ignoring case unless
  the query has a capital; the match is selected and scrolled into view, so
  Ctrl+C copies it. Enter or Up goes further up, Shift+Enter or Down comes
  back down, Esc closes it. It is `alacritty_terminal`'s own regex search,
  and works in a file view too. History went from 2000 rows to 10,000, what
  Windows Terminal keeps: 2000 was gone in an afternoon of Claude Code.
- **Open the project** in VS Code or in Explorer, from the project menu.
  VS Code is found as for the files tile; without it the line is greyed.

Tested on screen with a dev instance on its own port and `APPDATA`, running
`cmd.exe`: the zoom button zoomed and unzoomed, a double click on a header
unzoomed, moving right while zoomed showed the next pane, Ctrl+Alt+Left and
Ctrl+Shift+Enter did the same from the keyboard, Ctrl+Shift+F then "needle"
selected the last match and Enter the one above with nothing reaching the
shell, Ctrl+= twice made the font 17 and saved it, and Rename through the
tile menu renamed the tile, the stage's title and `state.json`. The keys
were sent to the dev instance's thread with its key state set through
`AttachThreadInput`, not as real input, so nothing reached another window.
The notification was accepted by Windows (logged with `HORADRIC_DEBUG`) but
not seen, since Do not disturb was on; how it looks and a click on it are
not tested yet. The rename dialog opened behind other windows, as anything
opened from a synthetic click does.

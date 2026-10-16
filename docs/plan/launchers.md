# Launchers

Typing `horadric new --cwd` was too much friction for starting a session in a
new project, so three ways in that need no terminal:

- **Tray icon.** Always there, even with no tiles. Its menu has "New
  session..." (the folder picker, opening in the last project), up to eight
  recent projects one click away, and Quit, which warns when sessions in
  Horadric terminals would end. Recent projects live in
  `%APPDATA%\Horadric\recent.json`. The tooltip counts sessions and waiting
  ones. The icon is drawn in code (`icon.rs`), so there is no file to ship:
  a dark cube with its lid lifted and light pouring out. One colour lights
  it (gold, red for a dev instance), and `icon::lit` takes any other, so
  the light can later follow what Horadric is doing.
  Pulled forward from step 5.
- **The full width `+` below a cluster's last tile** starts another session
  in that project at once, no picker. It is outlined, not filled, so it
  reads as the slot where the next tile goes rather than as a session.
- **The `+` in a cluster header** is for a new project. It opens the folder
  picker in the folder that holds this project, since projects tend to sit
  side by side. It used to open in the project itself, which made another
  session there the easy path; the bottom button is that path now.
- **Explorer.** `horadric explorer install` adds "Open in Horadric" when right
  clicking a folder or the empty space inside one, under
  `HKEY_CURRENT_USER`, so no admin rights. On Windows 11 it is under "Show
  more options"; the short menu only takes packaged apps. It runs
  `horadricw.exe`, a second, windowless binary, so a right click never flashes
  a console. `horadricw` starts the app when it is not running (hidden, no
  console window) and then asks it for the session. With no folder it only
  starts the app. The installed copy adds the verbs the first time it runs
  (`explorer_offered` in the state, the same once only rule as Start with
  Windows), so an update brings them to a machine that never ran
  `install`, and the Startup settings have "Open in Horadric in Explorer"
  to take them away or put them back. The Windows 11 short menu was tried
  on 2026-10-13: it needs a packaged `IExplorerCommand` handler, and
  Windows refuses an unsigned package that declares a context menu ("not a
  valid unsigned package", even registered loose with `AllowUnsigned`). It
  waits on a signing certificate.

- **The start window** (`start.rs`). With no project open the desktop
  used to show only the usage window and a tray icon, and nothing said how
  to begin. Now a ghost cluster stands where the first cluster will go,
  below the usage window: "No project open", a dashed tile "Open a
  project" that opens the folder picker (beside the most recent project),
  and up to five recent projects that start a session with one click. A
  folder dropped on it from Explorer opens as the project, and a dropped
  file opens its folder. It exists only while there are no clusters, so it
  is gone the moment the first one appears. Tested on screen with a dev
  instance: the empty and the recent versions drawn, and a click on a
  recent project started one `cmd.exe`, replaced the ghost with its
  cluster and opened the stage. The picker and a drop not yet tried by
  hand.

Two Win32 details that decided the shape. The app's hidden window is now a
top level tool window, not message-only, because only top level windows
hear Explorer's `TaskbarCreated` broadcast, after which the tray icon must be
added again. And menus and dialogs run modal loops that dispatch our own
messages, so they run with the app state not borrowed, from the window
procedure itself.

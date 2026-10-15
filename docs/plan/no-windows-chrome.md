# No Windows chrome

Decided 2026-09-26: everything the app shows is drawn in its own look.
Menus, questions, warnings, errors, notifications, the folder picker and
the stage's title bar all were Windows' own and read as a different app.
Asked for as "no default Windows crap, make it premium". The one piece of
Windows left is Explorer's folder dialog, a click away behind Browse, for
a folder easier found by looking.

- **Menus** (`menu.rs`): the tray's, a tile's, a project's, a task's and
  the stage's window menu, through `menu::popup` in place of
  `TrackPopupMenu`, with the same `Item`s. On a plate like a setting's
  list: a lamp by a checked line, the text after a tab right aligned and
  faint (a shortcut, a place, an age), a chevron on a submenu, grooves
  between groups. The first window takes the focus and the mouse, a
  submenu opens beside its line after the mouse rests 220 ms and takes
  neither, and every mouse message is sorted by where it lands on screen.
  The arrows, Home, End, Enter, Right and Left, Esc and a typed first
  letter work; a menu taller than the screen scrolls with the wheel and
  the keys. A button coming up before one went down on the menu is the
  end of the click that opened it and picks nothing. Labels are plain
  text now, no `&&`. Layout, placement, flipping at the screen's edges,
  submenus and the keyboard's steps are pure and tested.
- **Dialogs** (`dialog.rs`): Quit, ending sessions, merging a finished
  task, and `horadricw`'s startup error, in place of message boxes. A lamp
  by the title says what kind (a question in blue, a warning in amber, an
  error in red), the text wraps, and each answer is a key named for what
  it does: Keep running, Stop them, Cancel; End, Cancel; Merge, Not now.
  Enter presses the ringed key, the arrows and Tab move the ring, Esc or a
  click elsewhere answers nothing. Centred a little high on the screen
  under the mouse. `horadricw` has no app, so `error_alone` makes its own
  Direct2D and sets per monitor DPI first. A `horadricw` started by a
  script is not allowed the foreground, so its dialog does not get the
  keyboard; from Explorer it does.
- **Notifications** (`toast.rs`): in place of the tray's balloons, which
  Windows showed as its own toasts. In the bottom right corner of the
  primary screen's work area, over the tray. A lamp in what it is about
  (waiting amber, done green, news blue, failed red), fading in and out,
  standing 7 s not counting while the mouse is on it, a cross while it is.
  One at a time and a new one takes its place, since a click acts on what
  was said last: it comes back to the app as the balloon's
  `NIN_BALLOONUSERCLICK` did, so both paths are one. Held back only for a
  full screen game or presentation mode. `SHQueryUserNotificationState`
  also says busy for any window the size of the screen, which the stage
  often is, so that one is ignored or they would never show. Lost against
  Windows' toasts: the notification centre keeps no history, and Focus
  Assist is not heard.
- **The folder picker**: the question input (`ask.rs`) with a list under
  its field, through `ask::Pick`. Empty, it offers the recent projects;
  before a separator, the recent ones whose names hold what is typed;
  after one, the folders it could be completing to, hidden ones left out
  (`paths.rs`, pure and tested, with `~` for the home folder). Up and Down
  pick, Tab fills one in with a separator after it so its folders come
  next, Enter or a click starts there, and a folder that does not exist
  is refused in red where the hint was. Browse opens Explorer's dialog in
  what was typed, if it is a folder.
- **The stage's caption** (`caption.rs`): `WM_NCCALCSIZE` gives the client
  the top of the frame, and a child window paints the caption there with
  the plate's light and seam carried on, the project's lamp and name, the
  session and what its agent is doing, and minimise, maximise and close
  keys, close lit red. The child answers `HTTRANSPARENT`, so the stage
  hit tests: `HTCAPTION` to drag and double click, the frame's thickness
  along the top as `HTTOP`, and the keys as `HTMINBUTTON`, `HTMAXBUTTON`
  and `HTCLOSE`, which keeps Windows 11's snap layouts over maximise. The
  keys are pressed and let go on the non client messages, since
  `DefWindowProc` would draw the old ones over them. A right click on the
  caption and Alt+Space open the window menu, drawn. Maximised, the client
  starts where the screen does. Behind other windows the caption goes
  quiet.

Tested on screen with a dev instance on its own port and `APPDATA`, with
`cmd.exe` sessions: a tile's menu, the project menu and its History
submenu by mouse and keys; the End sessions dialog with Esc keeping the
session; `horadricw`'s error from a copy with no `horadric.exe` beside it;
a waiting toast from a posted hook, held under the mouse, faded after,
and a click on it bringing the stage forward; the picker completing
through three folders, refusing one that is not there and starting a
session in the scratch folder; the caption active and behind, maximised
and restored by its keys, close lit, the window menu. Not tried on
screen: the tray's own menu (its icon sat in the overflow), the Quit
dialog, a dragged or top-resized stage, and snap layouts.

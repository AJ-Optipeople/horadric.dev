# Remote screen: watching another machine's desktop from the browser pane

A session that works on another Windows machine (a VM in Azure, say)
drives it over SSH, and the human used to follow along only through RDP
in a window of its own, while the session saw still screenshots. Twice a
task stalled on a dialog nobody saw in time. This puts the remote
desktop, live, in a tab of the browser pane, where the human and the
session see the same picture, and the human takes over mouse and
keyboard with one click.

## The shape

The remote machine runs Horadric too. Its app serves its own desktop:
frames as JPEG, input posted back, on `127.0.0.1` only. This machine
reaches it through an SSH tunnel (`ssh -N -L`), so the remote opens no
port beyond the SSH it already has, and the trust is the SSH key, as
for Linux nodes. The viewer is a page the remote serves, so the pane
shows it as any web page, and a session opens it with `remote_open`.

Not RDP, on purpose: an RDP login takes the console session over, and
closing the window locks the desktop, which desktop control can not
work on. Watching the app's own capture of the console leaves the
console as it is.

## The server (the remote's app)

Off unless the user environment variable `HORADRIC_SCREEN_SHARE=1` is
set when the app starts (a normal install serves nothing new). Then the
app listens on `127.0.0.1:43119` (`43120` for a dev instance).

Every request carries the token as `?t=<token>` in the query. The viewer
moves it from its own address into the tab's `sessionStorage` as it
loads, so the tab's URL (which the agent can read) no longer shows it,
and adds it to each request itself. The token
is 32 hex characters in `<state dir>\screen-token`, written on first
start and kept. Without it, or with a wrong one: `403`. Reading the
token takes a login on the machine, so a tunnel alone is not enough.

- `GET /` the viewer page (HTML, self contained). It alone needs no
  token: it holds no data, and a reload of the tab comes back without
  the token in its address.
- `GET /frame.jpg?t=&w=<max width>` the whole virtual screen (all
  monitors) as a JPEG with the cursor drawn in, scaled down to at most
  `w` pixels wide when `w` is given. Headers `X-Screen-Left`,
  `X-Screen-Top`, `X-Screen-Width`, `X-Screen-Height` give the virtual
  screen in physical pixels, `Cache-Control: no-store`.
- `POST /input?t=` one JSON event, coordinates in virtual screen pixels
  from its top left:
  `{"kind":"move","x":10,"y":20}`,
  `{"kind":"down"|"up","x":..,"y":..,"button":"left"|"right"|"middle"}`,
  `{"kind":"wheel","x":..,"y":..,"delta":-120}`,
  `{"kind":"key","vk":13,"down":true}`,
  `{"kind":"text","text":"æøå"}` (typed as Unicode).
  Answers `204`, `400` with a reason, `409` when Windows took fewer
  events than given (the window under them runs elevated, or the
  desktop is locked), shown briefly in the viewer's bar, or `413` for a
  body over 64 KiB. The viewer posts one event at a time, each after the
  one before is answered, so a button's up never overtakes its down; a
  move still waiting is replaced by a newer one. Letting go of control
  (the button, Esc twice, the tab losing focus) releases every key and
  mouse button it holds down there. A single Esc goes to the remote
  after 600 ms unless a second one follows.
- `GET /display?t=` the primary display's resolution and the ones it
  offers, as JSON
  `{"current":{"width":2560,"height":1440},"modes":[{"width":2560,"height":1440},{"width":1920,"height":1080}]}`:
  each width and height the driver lists at the bits per pixel in use,
  once whatever its refresh rates, none smaller than 800 by 600, widest
  first.
- `POST /display?t=` with `{"width":1920,"height":1080}` switches the
  primary display to one of those, kept in the registry, at the refresh
  rate in use when the new size has it. Answers `204`, `400` when the
  size is not offered or the body is not one, or `409` with the
  `DISP_CHANGE` code Windows refused it with. Frames after it come at
  the new size, as each one reads the virtual screen afresh. This
  changes the remote machine's own desktop, for RDP and for the
  programs' layout too, unlike the viewer's zoom, which only scales the
  picture here.
- `GET /sessions?t=` the remote's Horadric sessions as JSON
  `[{"id","name","project","state","waiting"}]`, `waiting` being the
  question or approval it waits on, or null.

## The client (this machine)

`horadric remote open <ssh destination>` and the MCP tool
`remote_open {"ssh": "<destination>"}`: start
`ssh -N -L 127.0.0.1:<free local port>:127.0.0.1:43119 -o
ExitOnForwardFailure=yes <destination>` (kept for the destination,
reused while alive, under a lock file so two sessions opening one
machine at once share one tunnel), read the
token with `ssh <destination> cmd /c type "%APPDATA%\Horadric\screen-token"`,
and open `http://127.0.0.1:<port>/?t=<token>` in a tab of the browser
pane. The token goes in no log or reply text.

`horadric remote close <ssh destination>` ends the tunnel kept for it
and forgets it.

`remote_screenshot {"ssh", "width"?}` fetches `/frame.jpg` through the
same tunnel and gives it to the session as an image: the picture the
human sees. Its text line gives the screen's size and, read from the
JPEG's own header, the picture's, with the factor that turns a spot in
the picture into screen pixels ("multiply image coordinates by 1.6"),
since the picture is scaled down to `width` (1600 unless asked).

The session operates the desktop the way it drives a web page, each
tool taking `ssh` like the others and posting `/input` events through
the same tunnel, one at a time, each after the one before is answered:

- `remote_click {ssh, x, y, button?: "left"|"right"|"middle", double?}`
  a move, then down and up (twice for a double click).
- `remote_type {ssh, text}` `text` events, split so no body passes the
  remote's 64 KiB.
- `remote_press {ssh, key}` a key ("Enter", "Tab", "Escape", "F5", "PageDown",
  a letter or digit) or a chord ("Ctrl+S", "Alt+F4", "Ctrl+Shift+Esc",
  "Win+R"): the modifiers go down in order, then the key, and all come
  up in reverse.
- `remote_scroll {ssh, x, y, delta}` one `wheel` event, `delta` in
  notches (times 120), negative scrolling down.

x and y are screen pixels from the top left of the whole virtual
screen. Each answers a short line ("clicked at 812,440") and no picture:
the descriptions tell the session to look with `remote_screenshot`, act,
then look again. Anything but `204` stops the sequence and the remote's
own words become the tool's error, a `409` saying the window runs as
administrator or the desktop is locked. The token is masked out of every
error.

## Limits now

- Input from the app reaches programs at its own integrity level. A
  program running as administrator (Sysmac Studio) takes clicks only
  when the remote's Horadric runs elevated, or through the desktop
  helper later.
- The viewer is frames polled a few times a second, not video.
- Watching does not count as use for the VM's idle shutdown.

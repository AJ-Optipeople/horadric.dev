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

Every request carries the token as `?t=<token>` in the query. The token
is 32 hex characters in `<state dir>\screen-token`, written on first
start and kept. Without it, or with a wrong one: `403`. Reading the
token takes a login on the machine, so a tunnel alone is not enough.

- `GET /?t=` the viewer page (HTML, self contained).
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
  Answers `204`, or `400` with a reason.
- `GET /sessions?t=` the remote's Horadric sessions as JSON
  `[{"id","name","project","state","waiting"}]`, `waiting` being the
  question or approval it waits on, or null.

## The client (this machine)

`horadric remote open <ssh destination>` and the MCP tool
`remote_open {"ssh": "<destination>"}`: start
`ssh -N -L <free local port>:127.0.0.1:43119 -o ExitOnForwardFailure=yes
<destination>` (kept for the destination, reused while alive), read the
token with `ssh <destination> cmd /c type "%APPDATA%\Horadric\screen-token"`,
and open `http://127.0.0.1:<port>/?t=<token>` in a tab of the browser
pane. The token goes in no log or reply text.

`remote_screenshot {"ssh"}` fetches `/frame.jpg` through the same tunnel
and gives it to the session as an image: the picture the human sees.

## Limits now

- Input from the app reaches programs at its own integrity level. A
  program running as administrator (Sysmac Studio) takes clicks only
  when the remote's Horadric runs elevated, or through the desktop
  helper later.
- The viewer is frames polled a few times a second, not video.
- Watching does not count as use for the VM's idle shutdown.

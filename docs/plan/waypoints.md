# Waypoints

Asked for on 2026-10-11: two PCs, a VPS and a Raspberry Pi, each with
Horadric and its own projects, and every one of them reachable from the
others. Sit at any machine and see every session on every machine as a
tile, answer it, watch it in the stage and start new ones, with remote
tiles plainly marked as remote. Named after Diablo II's waypoints, which
join every place you have been into one network.

**What it is and is not.** Remote *sessions*: their tiles, their phases,
their terminals, and starting and resuming them. Not one machine: the
files tile, the file viewer, the browser pane, the git glance, the quest
log and Warriv all assume a local disk, so a remote project has none of
them in the first version. Promising "every device is one device" and
then lacking files would be worse than saying plainly what carries over.

Not part of this: the agent driving a phone it is testing an app on.
That is a tool for the agent, like the browser pane (adb or a simulator
over MCP), not a remote tile, and it goes in IDEAS.md on its own.

**Most of it exists already.** Town Portal (see [PORTAL.md](../PORTAL.md))
lists sessions and projects, streams a screen, takes input, starts,
resumes and creates, all end to end encrypted through the relay. A second
PC is a phone with a better screen. What it lacks:

- **The stream is too poor for the stage.** `screen` is plain text
  without colour, at most 4 times a second, fine on a phone and second
  rate in a pane. A remote pane needs the raw output bytes, drawn by the
  same `alacritty_terminal` grid a local pane uses.
- **The client side is a web page.** The Windows UI has to become a
  client too: connect out as a phone does, keep remote sessions beside
  local ones, draw them.
- **Only Windows serves.** The PC side's crypto is CNG and its WebSocket
  is Windows only (`portal_crypto.rs`, `portal_link.rs`), and the serving
  lives in the UI. A VPS or a Pi has neither a UI nor CNG.

**Decisions.**

- **A device serves only its own sessions.** B shows A's sessions, A
  shows B's, and neither passes on what it got from the other. Otherwise
  three devices mirror each other forever, and a phone paired with one PC
  would quietly reach every machine. Each link is one the human made.
- **Two transports, one protocol.** Between Windows PCs, the relay, as
  phones use it today. To a Linux machine, `ssh <host> horadric node
  --stdio`, the same messages framed on stdin and stdout. The VPS and the
  Pi are already in `~/.ssh/config`, `ssh` does the encryption and the
  keys, and the Linux build needs no TLS, no AES and no WebSocket, so no
  new dependency. This follows SSH hosts: Horadric does not speak SSH, it
  runs `ssh.exe`. A Linux node on the relay too (for a Pi behind NAT with
  no way in) is a later step and a dependency decision of its own
  (`ring` or `aes-gcm`, and a WebSocket with TLS).
- **This reopens one thing in SSH hosts, by the human's ask.** There,
  running `claude` on the server was ruled out because its hooks post to
  the server's localhost. A node is a Horadric on that server, with its
  own listener, so that reason goes. The agent still stays local by
  default for SSH hosts; a node is for when the human wants agents living
  on that machine, logged in there.
- **Pairing is mutual and named.** "Add a device..." in the tray menu.
  For a Windows PC: the other PC shows a pairing link (the Town Portal
  key's "Pair a phone..." with a "Pair a PC" beside it), pasted here; in
  the same exchange each side gives the other a key of its own, so one
  pairing joins both ways. For a Linux node: an SSH destination, picked
  with the Fleets picker from `~/.ssh/config`; the trust is the SSH key.
  Each device has a name ("Laptop", "vps") the human can change.
- **Each link can be view only.** "Can control this PC" is a switch per
  peer, on by default for a PC the human paired themselves. Off, the peer
  sees tiles and screens but sends no input, start, resume or create. The
  reason is the blast radius: every machine typing into every other
  machine's shells means one stolen laptop opens all of them.
- **Whose size wins.** A session watched in two stages at once has one
  pseudo terminal and one size. The viewer that last typed or clicked in
  it sets the size; the other draws the grid at that size, clipped or
  with a margin. Like tmux's `latest`. Phones never resize, as today.
- **A remote project is its own cluster.** Even when the same repository
  is checked out on both machines: they are two working trees with their
  own state. The cluster's title carries the device ("horadric.dev ·
  vps") and its frame a device mark, so a remote tile is never mistaken
  for a local one. A device out of reach keeps its tiles, dimmed, with
  "vps out of reach" as the second line and their last phase, and they
  come back when it does. Hiding them would make a dropped link look
  like finished work.
- **Waiting is waiting.** A remote session that turns waiting lights,
  chimes and counts like a local one. That is most of the point.

**Protocol v3.** Added to PORTAL.md when built. A v2 phone or PC keeps
working; a device that answers `v` 2 shows as "needs a newer Horadric".

- Hello gains `"kind":"phone"` or `"pc"`, and the PC's answer
  `"device":"<name>"` and `"control":true|false`.
- Phone or PC to PC, `{"t":"attach","id","cols","rows"}`: stream this
  session raw. The answer is the host's ring first, then live output, as
  `{"t":"out","id","seq","b64"}`, split to stay under the relay's 1 MB.
  `{"t":"detach","id"}` stops it. Several at once, since a stage shows a
  project's panes together.
- `{"t":"resize","id","cols","rows"}` from the viewer that owns the size.
- `{"t":"size","id","cols","rows"}` from the PC whenever the size
  changes, so every viewer draws at it.
- `input` is refused with `error` "view only" on a link without control,
  and so are `start`, `resume` and `create`.

**Steps.**

1. **Raw streams from a Windows PC.** `attach`, `out`, `detach`,
   `resize`, `size` and the `kind` and `control` fields on the serving
   side, with the size rule as a pure function. Verified with a small
   script client against a dev instance first: attach, replay, type, see
   it on the dev stage.
2. **The Windows UI as a client of another PC.** Pairing a PC both ways,
   peers in `portal.json` beside phones, one link out per peer. Remote
   sessions in the registry with a `device` field, never served on.
   Remote clusters with the device in the title and the mark on the
   frame, remote panes fed by `out` into a local grid, typing sent as
   `input`, new session and resume on a remote cluster sent as `start`
   and `resume`. Verified with two dev instances on this machine, the
   second with `HORADRIC_PORT=43119` and `APPDATA` in a scratch folder,
   both through the real relay: a tile on each for the other's sessions,
   a session started from one running on the other, the link cut and the
   tiles dimming, then coming back.
3. **The serving pulled out of the UI.** What `app/portal.rs` does for a
   phone (sessions, projects, folders, start, resume, create, screens,
   streams) moves behind a trait the Windows UI implements, so a headless
   node can implement it too. No behaviour change, checked by step 1 and
   2's runs still passing.
4. **`horadric node` on Linux.** A Linux build of `horadric-core`,
   `horadric-hooks`, the POSIX host the Mac port already has
   (`posix.rs`, `socket.rs`) and the serving from step 3, with no UI.
   `horadric node` runs the listener and the hosts; `horadric node
   install` writes a systemd user unit (with lingering, so it runs logged
   out) and the hooks into `~/.claude/settings.json`; `horadric node
   --stdio` is what `ssh` runs to speak to it. A static musl build in the
   release, and `install.sh` learns Linux. Tested in CI on Linux like the
   Mac's smoke run: a node, a fake session posted to its port, a shell
   session through it, read over `--stdio`.
5. **Linux nodes on the stage.** "Add a device..." with an SSH
   destination; the UI keeps `ssh <host> horadric node --stdio` running
   per node through `System32\OpenSSH\ssh.exe`, as SSH terminals do, and
   reconnects when it drops. Verified against a real VPS: a session
   started from the stage running there, its hooks lighting the tile
   here.
6. **Later, each its own decision.** Linux nodes on the relay; the Mac
   as a client (its portal has no crypto yet either); remote files and
   the git glance over the same protocol, read only first.

**Pure and tested:** the v3 messages both ways, splitting and joining
`out`, the size rule, refusing what a view only link sends, never serving
a remote session, the dimmed tile's line, the stdio framing, the systemd
unit's text.

**Open questions.**

- Does a Linux node on the relay matter soon enough to take the crypto
  dependencies now, rather than SSH only? It does if a Pi sits behind a
  router with no way in.
- Should a phone paired with A see B through A after all, as an explicit
  per peer switch? Proposed no, for the reason above.

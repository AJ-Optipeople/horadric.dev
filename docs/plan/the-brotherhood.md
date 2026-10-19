# The Brotherhood

Every machine with Horadric on it, joined into one brotherhood: a
desktop, a laptop, a VPS, a home server. Sit at any of them and see
every session on every brother as a tile, use it as if it were local,
and see plainly where it lives. Hand a session to another brother when
you leave one, and when a brother falls silent mid work, another can
pick up where it stopped. Named after the Horadrim, the brotherhood of
mages who carried the Cube.

Asked for on 2026-10-11 as Waypoints (seeing every machine's sessions),
grown on 2026-10-19 into the Brotherhood (moving sessions between them
and taking over from a brother that died). Waypoints' plan is steps 1
to 5 below, unchanged in substance.

Not the same as Horadrim, the written tongue recall's answers are
inscribed in. That name stays with memory.

## What it is and is not

**Is:** remote sessions (their tiles, phases and terminals), starting and
resuming them on any brother, handing a session from one brother to
another between turns, and taking over a brother's sessions when it goes
quiet.

**Is not, at first:** one machine. The files tile, the file viewer, the
browser pane, the git glance, the quest log and Warriv all assume a
local disk, so a remote project has none of them in the first version.
Promising "every device is one device" and then lacking files would be
worse than saying plainly what carries over. A session that is handed
over does arrive with its files, since the work moves with it.

Not part of this: the agent driving a phone it is testing an app on
(see IDEAS.md).

## Most of it exists already

Town Portal (see [PORTAL.md](../PORTAL.md)) lists sessions and projects,
streams a screen, takes input, starts, resumes and creates, all end to
end encrypted through the relay. A brother is a phone with a better
screen. What it lacks:

- **The stream is too poor for the stage.** `screen` is plain text
  without colour, at most 4 times a second, fine on a phone and second
  rate in a pane. A remote pane needs the raw output bytes, drawn by the
  same `alacritty_terminal` grid a local pane uses.
- **The client side is a web page.** The Windows UI has to become a
  client too: connect out as a phone does, keep remote sessions beside
  local ones, draw them.
- **Only Windows serves.** The PC side's crypto is CNG and its WebSocket
  is Windows only (`portal_crypto.rs`, `portal_link.rs`), and the serving
  lives in the UI. A VPS or a home server has neither a UI nor CNG.
- **Nothing moves.** A session is a `claude` process on one machine, its
  transcript in that machine's `~/.claude/projects`, its work in that
  machine's working tree. Handoff and takeover have to carry all three.

## Decisions

### Joining

- **A brother is a full member, a phone is not.** Brothers see each
  other's sessions, hold each other's snapshots and can take over from
  each other. A phone stays paired with one PC and sees that PC's
  sessions, as today.
- **Joining once joins everyone.** "Join a brotherhood..." in the tray
  menu. A Windows PC pastes a link that a brother shows ("Pair a PC"
  beside "Pair a phone"); a Linux node is added by its SSH destination,
  picked with the Fleets picker from `~/.ssh/config`, where the trust is
  the SSH key. The brother that let it in passes on the roster, so the
  newcomer links to every other brother directly, and every brother
  learns of it. Each pair still has a key of its own, so one brother can
  be cast out alone.
- **The roster.** Every brother keeps the same list: each brother's
  name, key and link, and each session's owner. Changes carry a number
  that only goes up, and the highest wins, so two brothers that were
  apart agree again when they meet.
- **Each brother has a name** ("Laptop", "vps") the human can change, and
  a mark drawn on its tiles' frames.
- **Cast out.** Any brother can cast another out ("Cast out Laptop..."),
  for a stolen or retired machine. Every brother drops its key and
  refuses it from then on. It needs the human's click on a brother, never
  a phone, and it cannot be undone except by joining again.
- **Each link can be view only.** "Can control this brother" is a switch
  per brother, on by default for one the human joined themselves. Off,
  the other sees tiles and screens but sends no input, start, resume,
  create, handoff or takeover. The reason is the blast radius: every
  machine typing into every other machine's shells means one stolen
  laptop opens all of them.

### Seeing

- **A brother serves only the sessions it owns.** Streams and input go
  straight to the owner, never through a third brother. The mesh is
  direct links, so nothing is relayed twice and no loop can form.
- **Two transports, one protocol.** Between Windows PCs, the relay, as
  phones use it today. To a Linux node, `ssh <host> horadric node
  --stdio`, the same messages framed on stdin and stdout. `ssh` does the
  encryption and the keys, and the Linux build needs no TLS, no AES and
  no WebSocket, so no new dependency. Horadric does not speak SSH, it
  runs `ssh.exe`, as SSH hosts do. A Linux node on the relay too (for a
  machine behind NAT with no way in) is a later step and a dependency
  decision of its own (`ring` or `aes-gcm`, and a WebSocket with TLS).
- **This reopens one thing in SSH hosts, by the human's ask.** There,
  running `claude` on the server was ruled out because its hooks post to
  the server's localhost. A node is a Horadric on that server, with its
  own listener, so that reason goes. The agent still stays local by
  default for SSH hosts; a node is for when the human wants agents living
  on that machine, logged in there.
- **Whose size wins.** A session watched in two stages at once has one
  pseudo terminal and one size. The viewer that last typed or clicked in
  it sets the size; the other draws the grid at that size, clipped or
  with a margin. Like tmux's `latest`. Phones never resize, as today.
- **A remote project is its own cluster.** Even when the same repository
  is checked out on both machines: they are two working trees with their
  own state. The cluster's title carries the brother ("horadric.dev ·
  vps") and its frame the brother's mark, so a remote tile is never
  mistaken for a local one.
- **Out of reach is shown, never hidden.** A brother out of reach keeps
  its tiles, dimmed, with "vps out of reach" as the second line and their
  last phase, and they come back when it does. Hiding them would make a
  dropped link look like finished work.
- **Waiting is waiting.** A remote session that turns waiting lights,
  chimes and counts like a local one. That is most of the point.

### Handing off

- **Between turns only.** "Hand off to ▸ Desktop" on a tile waits for the
  session to end its turn (idle or waiting on the human), then moves it.
  Mid turn the tile says "hands off when this turn ends"; "Now" beside
  it interrupts the turn first, as Esc would. A turn is never cut by a
  move the human did not ask to be immediate.
- **Hand off everything.** "Leaving: hand off to ▸ ..." in the tray menu
  moves every session the brother owns, one by one as each turn ends,
  and says when the last has gone, so the laptop can be shut.
- **What moves.** The bundle is:
  - the transcript, with its subagent files;
  - the session's record (name, project, agent, model, account, quest);
  - the work: every change in the working tree, untracked files
    included, as a commit on a private ref
    (`refs/horadric/handoff/<session>`, never a branch the human sees,
    never pushed anywhere), sent as a `git bundle` over the link. The
    bundle is cut against a commit both brothers have, found by asking
    the receiver; with none in common it is the whole history.
- **No remote needed.** The work goes over the brotherhood's own link,
  not through GitHub. A handoff never pushes, so it never publishes half
  done work and works for a repository with no remote at all.
- **Where it lands.** The receiver finds the project by the repository's
  remote URL, or by its first commit when it has none. Absent, it asks:
  clone it, or pick a folder. The session gets a worktree of its own
  there (as tombs and parallel quests do), so a handoff never disturbs
  the receiver's own checkout. Without git, the bundle is a plain copy
  of the folder's changed files into a folder the human picks.
- **The transcript is filed under the new path.** Claude Code keeps
  transcripts by the folder the session ran in, so `C:\Users\...` on the
  laptop and `/home/...` on the VPS are different keys. The receiver
  writes the transcript under its own path, then resumes with `claude
  --resume <id>`. Whether the `cwd` inside each line must be rewritten
  too is the first thing the spike settles.
- **The agent is told.** The resumed session's next prompt starts with
  one line: moved from Laptop to Desktop, the work is in this worktree,
  the last turn ended cleanly. Paths it remembers may be wrong now.
- **The sender lets go.** Its session ends, its worktree is kept until
  the receiver says the session runs, then removed. Its tile becomes "on
  Desktop" with Desktop's mark, on every brother, and keeps its place.
- **One owner, always.** The roster's owner of the session changes in the
  same step the receiver confirms, with the session's number one higher.
  Until then the sender still owns it, so a handoff that fails halfway
  leaves the session where it was.
- **Claude Code first.** Codex and Grok Build keep their transcripts
  elsewhere and resume differently. Their tiles show no "Hand off" until
  each is done the same way.

### Taking over

- **Keepers.** Each brother names who keeps it ("If Laptop falls silent,
  Desktop keeps its sessions"), one keeper or an ordered list. At every
  turn end the owner sends its keeper the turn's snapshot: what the
  transcript grew by and the work as a bundle against the last snapshot.
  So the keeper always holds the session as of its last finished turn.
  What was mid turn when the brother died is lost, and only that.
- **A sleeping brother is not a dead one.** A laptop with its lid closed
  looks exactly like a laptop that died. So a brother says "sleeping"
  when Windows tells it it is going to sleep, and "leaving" when it shuts
  down, and the others show its tiles as "Laptop asleep" rather than
  out of reach. A brother that falls silent without a word is "silent".
- **Taking over is the human's call, by default.** When a brother has
  been silent for two minutes, its tiles on every other brother and on
  the phone read "Laptop silent since 14:02" with "Take over on Desktop".
  A click takes over that session or all of them. Automatic takeover is a
  switch per brother ("Take over by itself after 10 minutes"), meant for
  quest runners and Warriv, which nobody is watching anyway. Never
  automatic for a brother that said "sleeping".
- **Only the first keeper takes over.** Automatic takeover is the first
  keeper on the list, or the next only once the first has been out of
  reach as long. Two brothers never race for one session.
- **Fenced.** Taking over raises the session's number in the roster. A
  brother that comes back asks the roster before its sessions run on,
  and a session whose number is behind is not resumed: it stays paused,
  its work kept as a branch `horadric/orphaned/<session>` so nothing is
  lost, and its tile says "taken over by Desktop at 14:12". The human
  can compare and keep either.
- **The agent is warned.** A taken over session's first prompt says so,
  and says the turn that was running on the dead brother may have done
  things not in the transcript: a push, a deploy, a file written. It
  checks before doing them again.
- **Logins are the receiver's.** A session runs on the receiver as the
  receiver's Claude account, and its `gh`, `git`, `npm` and Vercel
  logins are the receiver's own (see [identities.md](identities.md)).
  A brother missing a login a session needs blocks it with the wait for
  that login, rather than running it as someone else.

## Protocol v3

Added to PORTAL.md when built. A v2 phone or PC keeps working; a brother
that answers `v` 2 shows as "needs a newer Horadric".

- Hello gains `"kind":"phone"` or `"brother"`, and the answer
  `"brother":"<name>"` and `"control":true|false`.
- `{"t":"attach","id","cols","rows"}`: stream this session raw. The
  answer is the host's ring first, then live output, as
  `{"t":"out","id","seq","b64"}`, split to stay under the relay's 1 MB.
  `{"t":"detach","id"}` stops it. Several at once, since a stage shows a
  project's panes together.
- `{"t":"resize","id","cols","rows"}` from the viewer that owns the size.
  `{"t":"size","id","cols","rows"}` from the owner whenever the size
  changes, so every viewer draws at it.
- `{"t":"roster","n",...}`: the roster, sent on connect and on every
  change. The higher `n` wins.
- `{"t":"state","state":"awake"|"sleeping"|"leaving"}` and a heartbeat
  every 15 seconds.
- `{"t":"handoff","id","to"}` asks the owner to hand a session to
  `to`. The bundle goes from owner to receiver as `{"t":"bundle","id",
  "part","of","b64"}`, then `{"t":"took","id","n"}` from the receiver
  once the session runs.
- `{"t":"keep","id","n","b64"}`: a turn's snapshot to the keeper.
- `{"t":"takeover","id","n"}`: the roster change that moves a silent
  brother's session to its keeper.
- `input`, `start`, `resume`, `create`, `handoff` and `takeover` are
  refused with `error` "view only" on a link without control.

## Steps

Each step is useful alone and each is verified before the next starts.

1. **Raw streams from a Windows PC.** `attach`, `out`, `detach`,
   `resize`, `size` and the `kind` and `control` fields on the serving
   side, with the size rule as a pure function. Verified with a small
   script client against a dev instance first: attach, replay, type, see
   it on the dev stage.
2. **The Windows UI as a client of another PC.** Joining two PCs, the
   roster in `portal.json` beside phones, one link out per brother.
   Remote sessions in the registry with a `brother` field, never served
   on. Remote clusters with the brother in the title and its mark on the
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
5. **Linux nodes on the stage.** Joining with an SSH destination; the UI
   keeps `ssh <host> horadric node --stdio` running per node through
   `System32\OpenSSH\ssh.exe`, as SSH terminals do, and reconnects when
   it drops. Verified against a real VPS: a session started from the
   stage running there, its hooks lighting the tile here.
6. **The spike: does a moved transcript resume?** By hand, before any
   code: copy a finished session's transcript from this PC to the VPS,
   file it under the VPS's path, `claude --resume` it there, and see what
   it remembers and what breaks. Again Windows to Windows. Settles
   whether `cwd` lines need rewriting and whether subagent files and
   `ai-title` come along. Written down here before step 7.
7. **The mesh and the roster.** Joining once joins everyone, the roster
   with its number, session owners, cast out, view only per link,
   "asleep", "silent" and "out of reach" on the tiles. Pure and tested:
   merging two rosters, the owner rule, refusing a cast out key.
   Verified with three dev instances.
8. **Handoff between Windows PCs.** The bundle (transcript, record, work
   as a private ref and a `git bundle`), finding or cloning the project,
   a worktree for the session, the transcript filed under the new path,
   the resume with its first line, the sender letting go, the owner
   changing on `took`. "Hand off to" on a tile and "Leaving" in the tray.
   Verified with two dev instances on this machine, then between this PC
   and the laptop: a session mid task handed over, its next turn
   continuing the work with the files in place.
9. **Handoff to and from a Linux node.** The same over `--stdio`, and the
   path rewrite between Windows and Linux. Verified against the VPS both
   ways.
10. **Keepers.** Turn end snapshots to the first keeper, kept in its
    state folder, pruned when the session ends or moves. Pure and tested:
    a snapshot's bundle against the last, applying a chain of them.
11. **Takeover.** "Take over" on a silent brother's tiles, the fencing on
    return, the orphaned branch, the warning line, automatic takeover as
    a switch. Verified by pulling the network cable on the laptop mid
    quest: the desktop takes over from the last turn end, the laptop
    comes back and pauses its copy.
12. **Later, each its own decision.** Linux nodes on the relay; the Mac
    as a brother (its portal has no crypto yet either); remote files and
    the git glance over the same protocol, read only first; Codex and
    Grok Build handoff; a quest moving with its session, so the quest log
    on the receiver knows it.

## Pure and tested

The v3 messages both ways, splitting and joining `out` and `bundle`, the
size rule, refusing what a view only link sends, never serving a session
another brother owns, merging rosters, the owner and fencing rule, the
silent and asleep timers, picking the keeper, the dimmed tile's line, the
stdio framing, the systemd unit's text, the transcript path rewrite.

## Open questions

- Does a Linux node on the relay matter soon enough to take the crypto
  dependencies now, rather than SSH only? It does if a home server sits
  behind a router with no way in.
- Should a phone see the whole brotherhood through the PC it is paired
  with? The old answer was no: one stolen phone would reach every
  machine. A brotherhood makes the ask stronger. A per brother switch,
  off by default, is the likely answer.
- A quest's session handed to another brother: does the quest move with
  it, or stay on the brother whose quest log it is in? Warriv on two
  brothers working the same repository would need to agree on who runs
  what.
- How much disk does a keeper spend? A long session's transcript grows to
  tens of MB; snapshots are pruned when the session ends, but a keeper
  for five busy brothers may want a cap.
- Handing off a plain terminal: a shell has no transcript to resume, so
  probably never, only its folder.

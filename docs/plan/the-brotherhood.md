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
and 3 to 6 below, unchanged in substance. On 2026-10-23 it gained a
login (WorkOS), so joining is signing in, and a shared lore, so every
brother remembers what any of them found out. Teams are on the horizon,
and the login was picked with them in mind.

Not the same as Horadrim, the written tongue recall's answers are
inscribed in. That name stays with memory.

## What it is and is not

**Is:** a Horadric login, remote sessions (their tiles, phases and
terminals), starting and resuming them on any brother, handing a session
from one brother to another between turns, taking over a brother's
sessions when it goes quiet, and one lore shared by every brother.

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
- **Nobody is anyone.** Pairing is a secret per PC and a link per phone.
  There is no account, so a new machine cannot find its brothers without
  something pasted, and nothing could ever group machines into a team.
- **Each lore is one machine's.** The lore lives in that machine's state
  folder, and an earlier session found that two machines writing one
  memory file corrupt it. So it cannot simply sit in a synced folder.

## Decisions

### Signing in

- **WorkOS AuthKit, not accounts of our own.** No passwords, no user
  table, no reset mails to run. AuthKit signs in with Google, GitHub,
  Microsoft, an email link or a passkey, is free up to a million monthly
  users, and has organizations and SSO for when teams come. Chosen over
  Google sign in alone (which Drive backup already has) for those two
  reasons: more ways in, and teams without a second migration.
- **The login finds brothers, it never lets one in.** A stolen password
  must not open every shell the human has. Signing in shows a new machine
  its brotherhood; a brother already in must approve it ("Laptop wants to
  join. Let it in?") before it gets a key. Like linking a new device in
  Signal. The very first machine needs no approval.
- **Keys stay per device.** The account groups devices, the keys still
  encrypt end to end, one pair of keys per link as today. The relay sees
  who belongs together and nothing more.
- **In the app.** "Sign in..." in the tray menu opens the default browser
  (never the browser pane) on AuthKit, with PKCE and a loopback redirect
  on 127.0.0.1, as Google sign in for backup does. The refresh token goes
  in Credential Manager. WinHTTP and JSON only, so no new crate.
- **The relay checks the token.** A PC connects with its WorkOS access
  token beside its secret. The relay checks it against WorkOS's public
  keys and groups live connections by user. It still keeps nothing on
  disk: the roster lives on the brothers, the relay only knows who is
  connected now. Verifying a JWT in Node is the relay's own dependency
  decision (`jose`, or Node's `crypto` by hand).
- **When WorkOS is down,** nobody new can join. Brothers already joined
  keep working, since their keys and roster are local.
- **Drive backup keeps its own Google sign in.** WorkOS says who you are,
  it does not open your Drive.

### Joining

- **A brother is a full member, a phone is not.** Brothers see each
  other's sessions, hold each other's snapshots and can take over from
  each other. A phone stays paired with one PC and sees that PC's
  sessions, as today.
- **Joining once joins everyone.** Any machine, Windows PC or Linux
  node, joins by signing in and an approval on one brother. The brother
  that let it in introduces it to every other (see "Introductions"
  below), so the newcomer links to each directly with no further code,
  and every brother learns of it. Each pair still has a key of its own,
  so one brother can be cast out alone. Adding a Linux node by its SSH
  destination stays, as the way in for a machine that cannot sign in.
- **Introductions.** When A lets N in, A sends N, over their sealed
  link, every brother's id, name and public key, and sends each brother
  B N's in the same way. N and B then derive their link key from the
  ECDH of their long term keys, both public keys and both ids, and link
  with the usual sealed hello. No code is asked: each public key came
  over a link the human already vouched for, so the relay cannot slip
  its own in. An introduction is part of the roster and so carries its
  number; a brother that was away hears it when it relinks.
- **The roster.** Every brother keeps the same list: each brother's
  name, key and link, and each session's owner. Changes carry a number
  that only goes up, and the highest wins, so two brothers that were
  apart agree again when they meet.
- **Each brother has a name** ("Laptop", "vps") the human can change, and
  a mark drawn on its tiles' frames.
- **Cast out.** Any brother can cast another out ("Cast out Laptop..."),
  for a stolen or retired machine. Every brother drops its key and
  refuses it from then on. It needs the human's click on a brother, never
  a phone, and it cannot be undone except by joining again. Signing out
  of WorkOS everywhere does not cast anyone out; the keys are what count.
- **Each link can be view only.** "Can control this brother" is a switch
  per brother, on by default for one the human joined themselves. Off,
  the other sees tiles and screens but sends no input, start, resume,
  create, handoff or takeover. The reason is the blast radius: every
  machine typing into every other machine's shells means one stolen
  laptop opens all of them.
- **A six digit code proves no one sat between them** (built
  2026-10-24, in `horadric-core` since step 15: `joining.rs` and
  `brothers.rs`). The
  relay carries the handshake, so it could swap keys. Each PC keeps one
  ECDH P-256 key pair; the asker sends a commitment (a hash of its
  public key and a nonce) first, the giver answers with its key and
  nonce, then the asker reveals. Both work out the link key and a code
  from the ECDH secret, both keys and both nonces, and both ask their
  human whether the codes match, No by default. Each keeps the brother
  only after its own yes and the other's welcome (fixed 2026-10-24: at
  first only the giver asked, so a relay playing the giver was kept by
  the asker with full control). The commitment makes the relay pick its keys before it
  learns an honest side's nonce, so a forged pair of codes matches one
  time in a million rather than after a short search, which is why six
  digits are enough where Signal's safety numbers, compared at leisure,
  are sixty. The link key never crosses the relay. Rejected: a code from
  the keys alone, which a relay grinding keys could match in seconds.
- **The joined brothers live in `brothers.json`**, beside
  `portal.json` rather than in it, since the portal's config drops what
  it does not know when it saves. Each entry is the brother's id, name,
  public key and link key. This PC's own key pair is there too. A link
  comes back by itself: of two joined brothers the one with the lower id
  asks the relay for a channel when it sees the other listed, and a
  hello sealed with the link key, answered in kind, is the proof. Each
  hello carries a fresh nonce, and every frame after is sealed with a
  key of that link alone, bound to its way and a rising number, so the
  relay cannot echo a frame back, play one again, or bring one from an
  earlier link (a stale `state` heartbeat, an old `took`). The
  roster (`horadric-core/src/brotherhood.rs`) takes over from this file
  in step 3.

### Seeing

- **A brother serves only the sessions it owns.** Streams and input go
  straight to the owner, never through a third brother. The mesh is
  direct links, so nothing is relayed twice and no loop can form.
- **Two transports, one protocol.** Between brothers, the relay, as
  phones use it, Linux nodes included. To a Linux node joined by its
  SSH destination, `ssh <host> horadric node --stdio`, the same messages
  framed on stdin and stdout, with `ssh` doing the encryption. Horadric
  does not speak SSH, it runs `ssh.exe`, as SSH hosts do. A node that is
  both (joined by SSH, then signed in) is one brother: the relay link
  wins and its SSH entry is folded into it, never shown twice.

### Linux nodes on the relay

Decided 2026-10-24 by the human: a node must join by signing in, out of
the box, like a PC. Rejected: keeping nodes SSH only, which needs an SSH
key set up from every PC and cannot reach a home server behind a router.

- **The node speaks the PC's protocol to the relay.** Same `/pc`
  endpoint, same secret and id, same join handshake and sealed frames,
  so a Windows PC cannot tell a node from a PC except by the `kind` it
  says in hello. The relay changes nothing.
- **A crate of its own, `horadric-link`,** holds what Linux lacks that
  Windows has built in: ECDH P-256 (`p256`, since `ring` cannot load a
  stored private key), AES-256-GCM and random bytes (`ring`), TLS
  (`rustls` on `ring`, with `webpki-roots` so a minimal server needs no
  certificate store), the WebSocket (`tungstenite`, blocking, like the
  rest of Horadric's threads), and a small HTTPS client for WorkOS and
  model downloads. Windows keeps CNG and WinHTTP. A test on Windows
  runs both implementations over the same keys and frames, so the two
  can never drift apart.
- **The join logic leaves the UI.** The handshake, the sealed hello,
  the frame numbers, `brothers.json` and the relay glue move into
  `horadric-core` behind a small `Crypto` trait (random, ECDH, seal,
  open) and a `Human` trait (confirm a code, let one in), so the PC and
  the node run the same code.
- **Signing in a node: AuthKit's device code flow.** `horadric node
  signin` prints a short code and a link, polls WorkOS until the human
  confirms it in any browser, and keeps the refresh token in
  `workos.json` in the state folder, mode 0600 (no Credential Manager
  on a server). `install.sh` ends by running it when a terminal is
  attached.
- **A headless approval.** Signing in on a node then asks a brother at
  once: it lists the signed in brothers the relay shows, picks the one
  the human names or the only one, and shows the six digit code in the
  terminal. The human answers there, y or N, and on the PC, as a PC
  joining does. A node never lets anyone in by itself: approving a
  newcomer is a click on a PC, or `horadric node let-in` in a terminal
  on the node.
- **The node is a full brother.** It serves the roster, control,
  `state`, keeping and the lore like a PC, and can be a keeper.

### Lore on a node

- **The node holds a whole copy, vectors included,** and its agents get
  the lore tools (`horadric mcp`) as on Windows. Recall by meaning on a
  server is worth the build cost. So `horadric-memory`, the code index
  and the MCP server build for Linux, and the musl build carries their
  C (onig, tree-sitter's grammars), compiled with `musl-gcc` in CI.
  Models download over `horadric-link`'s HTTPS.
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
  writes `<id>.jsonl` under its own path's key, then resumes with
  `claude --resume <id>` from that folder. `--resume` looks only under
  the current folder's key, so filing it is required; rewriting the
  `cwd` in each line is not (step 7). The receiver rewrites `cwd` anyway,
  so the agent's history points where it works now, and
  brings `<id>/subagents/` and `<id>/tool-results/` along with the paths
  to them rewritten. The project's `memory/` folder beside them is the
  project's, not the session's, and does not move.
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

### Sharing the lore

- **Every brother keeps a whole copy.** Never one file in a synced folder,
  never a server holding the lore. Each brother writes only its own, and
  the copies meet over the mesh.
- **Entries are appended, never edited.** A note, a dialogue, a quest's
  summary each get an id of the brother that wrote it and a number that
  only goes up on that brother. A note that `replaces` another is a new
  entry, as today. So two copies merge by taking what the other has and
  this one lacks, with nothing to resolve.
- **Catching up.** When two brothers link they swap, per writing brother,
  the highest number each holds, and send what the other is missing. Then
  every new entry goes to every linked brother as it is written.
- **Vectors stay local.** A brother embeds what it receives itself, as a
  restore does. Only text crosses.
- **A project is found by its repository,** by the remote URL or the first
  commit, the same rule as handing off. Two checkouts of horadric.dev on
  two brothers share one lore. A project with no git shares nothing.
- **The code index is not shared.** It is built from each brother's own
  working tree and would be wrong anywhere else.
- **Forgetting crosses too.** Removing an entry is itself an entry, so a
  forgotten thing does not come back from a brother that was away.
- **Drive as the brother that never sleeps.** The encrypted Drive backup
  already holds each machine's lore. A brother that wakes with every
  other one silent can catch up from there. A later step, not the first.

## Protocol v3

Added to PORTAL.md when built. A v2 phone or PC keeps working; a brother
that answers `v` 2 shows as "needs a newer Horadric".

- The relay's `/pc` takes `token=<WorkOS access token>` beside the
  secret, and tells a signed in PC which of its user's brothers are
  connected (`{"brothers":[...]}`) and when one asks to join
  (`{"join":<ch>,"name"}`). A PC without a token is a v2 PC, served as
  today.
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
- `{"t":"lore","project","have":{"<brother>":n,...}}` on connect, then
  `{"t":"entries","project","b64"}` with what the other lacks, and the
  same for each new entry as it is written.
- `input`, `start`, `resume`, `create`, `handoff` and `takeover` are
  refused with `error` "view only" on a link without control.

## Steps

Each step is useful alone and each is verified before the next starts.

1. **Raw streams from a Windows PC.** `attach`, `out`, `detach`,
   `resize`, `size` and the `kind` and `control` fields on the serving
   side, with the size rule as a pure function. Verified with a small
   script client against a dev instance first: attach, replay, type, see
   it on the dev stage.
2. **Signing in.** A WorkOS project with AuthKit (Google, GitHub,
   Microsoft, email), "Sign in..." in the tray, PKCE in the default
   browser, the refresh token in Credential Manager. The relay checks
   the token, lists a user's connected brothers and passes a join
   request to them. The approval prompt on the brother. Verified with
   two dev instances on this machine and the real relay: sign in on
   both, the second asks, the first lets it in, both hold a key for the
   other.

   Done on 2026-10-24: "Alpha" and "Beta" (`HORADRIC_PC_NAME`), each
   with `APPDATA` in a scratch folder, both signed in, through
   `wss://portal.horadric.dev`. Alpha's tray, Brothers, "Beta, Ask to
   join...": Alpha's note and Beta's prompt both showed 587 941, Beta
   said yes, both `brothers.json` held the other's public key and the
   same link key. Beta restarted, then both: each time they linked
   again with no prompt, and the menu read "Beta, linked". A 4401 drops
   the access token in hand so the next try refreshes it; a 4503 waits
   30 seconds; a relay that closes within 10 seconds of connecting keeps
   the backoff growing.
3. **The Windows UI as a client of another PC.** Joining two PCs by step
   2, the roster in `portal.json` beside phones, one link out per brother.
   Remote sessions in the registry with a `brother` field, never served
   on. Remote clusters with the brother in the title and its mark on the
   frame, remote panes fed by `out` into a local grid, typing sent as
   `input`, new session and resume on a remote cluster sent as `start`
   and `resume`. Verified with two dev instances on this machine, the
   second with `HORADRIC_PORT=43119` and `APPDATA` in a scratch folder,
   both through the real relay: a tile on each for the other's sessions,
   a session started from one running on the other, the link cut and the
   tiles dimming, then coming back.

   The client side landed on 2026-10-24 (`horadric-ui/src/far.rs`, its
   pure parts in `horadric-core/src/afar.rs`). A brother's session is in
   the registry as `<id>@<brother>` with `brother` set and no folder, in
   the cluster `brother:<brother>/<project>`, and never in `consoles`, so
   nothing saves, stops or resumes it here. Its pane is a console with no
   host, fed by `out` and sending `input` and `resize`. The client does
   not care how the brother is reached: over a linked brother channel
   (asks told from answers by `n` and hello's `kind`, see
   `brothers.rs`), or through the phone door with a phone's key traded
   for a pairing ticket listed in `brother-phones.json`. Seen on screen
   with "Alpha" and "Beta" through `wss://portal.horadric.dev` by the
   phone door: Beta's "demo · Alpha" cluster with Alpha's mark, Alpha's
   cmd in Beta's pane taking typing, a session started from Beta running
   on Alpha and shown on Beta's stage, a waiting session lit with a toast
   on Beta, Alpha stopped and its tiles dimmed "Alpha out of reach",
   then back with the pane streaming again.

   Seen over the linked brother channel on 2026-10-24, joined by step 2
   and relinked after restarts: each of "Alpha" and "Beta" showed the
   other's cluster, a pane on each drew the other's cmd and took typing,
   "+" on Beta started a session on Alpha, a permission wait lit with a
   toast. A script phone on Alpha was served only Alpha's sessions, and
   its attach, watch, input and stop of Beta's session were refused.
   Alpha stopped: Beta's tiles dimmed "Alpha out of reach" and Beta's
   server closed Alpha's channel at once. Alpha told of a suspend: "Alpha
   asleep", then lit again on resume. Three things needed fixing for it:
   the roster carries `n`, so it was taken for a request (asks carry `s`
   too); the client dropped `state`; a channel down reached the server
   only at its next failed reply.
4. **The serving pulled out of the UI.** What `app/portal.rs` does for a
   phone (sessions, projects, folders, start, resume, create, screens,
   streams) moves behind a trait the Windows UI implements, so a headless
   node can implement it too. No behaviour change, checked by step 1 and
   3's runs still passing.
5. **`horadric node` on Linux.** A Linux build of `horadric-core`,
   `horadric-hooks`, the POSIX host the Mac port already has
   (`posix.rs`, `socket.rs`) and the serving from step 3, with no UI.
   `horadric node` runs the listener and the hosts; `horadric node
   install` writes a systemd user unit (with lingering, so it runs logged
   out) and the hooks into `~/.claude/settings.json`; `horadric node
   --stdio` is what `ssh` runs to speak to it. A static musl build in the
   release, and `install.sh` learns Linux. Tested in CI on Linux like the
   Mac's smoke run: a node, a fake session posted to its port, a shell
   session through it, read over `--stdio`.
6. **Linux nodes on the stage.** Joining with an SSH destination; the UI
   keeps `ssh <host> horadric node --stdio` running per node through
   `System32\OpenSSH\ssh.exe`, as SSH terminals do, and reconnects when
   it drops. Verified against a real VPS: a session started from the
   stage running there, its hooks lighting the tile here.

   Done on 2026-10-24 (`horadric-ui/src/far_node.rs`, its pure parts in
   `horadric-core/src/nodes.rs`). The nodes are in `nodes.json` in the
   state folder (name and ssh destination), joined from the tray's
   "Linux nodes" menu with the Fleets picker over `~/.ssh/config`, and
   forgotten there. Each runs `ssh.exe -T -o BatchMode=yes` with
   keepalives and `PATH=$HOME/.local/bin:$PATH exec horadric node
   --stdio`, since a command over `ssh` gets no login shell; an entry's
   `command` overrides that. The link is up once the preamble arrives;
   it is dialled again after 1, 2, 4 up to 30 seconds, from 1 again once
   a link lasted 10. The node now names a session's project by its
   folder's name, as a Windows PC does. Seen on screen against
   `exit1-eu`, the node installed for `agent` with a static musl build
   (cross built on Windows with `rust-lld` as the linker) and a drop-in
   setting `HORADRIC_AGENT=/bin/bash`: "horadric-demo · vps" with its
   mark, a session started from its "+" running bash on the VPS in the
   stage, typing echoed, fake hooks posted from that shell lighting the
   tile working and then "needs permission", the `ssh.exe` killed and
   the tiles dimmed "vps out of reach", then back with the pane replayed
   and taking typing again. The tray read "vps, linked".
7. **The spike: does a moved transcript resume?** By hand, before any
   code: copy a finished session's transcript from this PC to the VPS,
   file it under the VPS's path, `claude --resume` it there, and see what
   it remembers and what breaks. Again Windows to Windows. Settles
   whether `cwd` lines need rewriting and whether subagent files and
   `ai-title` come along. Written down here before step 10.

   Found on 2026-10-23, with Claude Code 2.1.309 on Windows and haiku:
   - **Windows to Windows resumes.** A `-p` session in repo A (wrote
     `notes.txt`, told the codeword HELIX), the repo copied to B, the
     transcript copied under B's key alone (the project's `memory/`
     left behind, so the answer could only come from the transcript),
     then `claude --resume <id> -p ...` from B: it answered HELIX and
     `notes.txt`. Same with every `cwd` rewritten to the new path.
   - **`cwd` need not be rewritten.** Both ways resume. The resumed turn
     gets a fresh environment attachment with the new working directory,
     and its new lines carry the new `cwd`. The old lines keep naming A
     in tool inputs and results (`spikeA\notes.txt`), so with A still on
     disk the agent could reach back into it. That is why the receiver
     rewrites anyway, and why the first line tells the agent it moved.
   - **The key must be right.** Run from a folder whose key does not
     hold the file and it fails at once: "No conversation found with
     session ID". It does not search other keys. The key is the full
     path with every character that is not a letter or digit turned into
     `-` (`C--Users-...-spikeA`, `-home-agent-...-repo1`).
   - **The id stays.** The resumed session keeps the same session id and
     appends to the copied file. The original under A is untouched.
   - **Subagents are not needed to resume.** They live in
     `<key>/<id>/subagents/agent-<agentId>.jsonl` with a `.meta.json`
     beside each. A session whose subagent invented a password resumed
     under the new key without that folder and still knew the password
     and the file: the subagent's result is in the main transcript as a
     tool result. The folder is only the subagent's own history, so it
     comes along for completeness, not for the resume.
   - **`tool-results/` holds paths.** Large tool output is kept in
     `<key>/<id>/tool-results/` and the main transcript names those files
     by absolute path, under the old key. They move with the session and
     the paths are rewritten, or a resumed agent that rereads one finds
     nothing.
   - **`ai-title` comes along.** It is a line in the main transcript
     (`{"type":"ai-title","aiTitle":...}`), written by interactive
     sessions (a `-p` session has none), so it moves with the file.
   - **The project `memory/` folder is separate.** Auto memory lives in
     `<key>/memory/` and belongs to the project, not the session. A
     moved session does not see the old project's memory unless the
     receiver's project already has its own.
   - **Hooks.** The resumed run's hooks posted to the installed Horadric
     with the same session id and the new `cwd` (Stop returned 200).
     Horadric's memory index read each copied file as new: the copy with
     rewritten `cwd` had its old turns indexed again under the new
     project, the unchanged copy had its new turn half filed under the
     old project. Step 10 tells the index a moved transcript is the same
     session, or skips the turns it already holds.
   - **Windows to Linux: filed, found, not answered.** On the Frankfurt
     VPS (`exit1-eu`, Claude Code 2.1.281) the transcript filed under
     `-home-agent-horadric-spike-repo1` (unchanged `cwd`) and `...repo2`
     (`cwd` rewritten to the Linux path) was found by `--resume` both
     ways: the older Claude Code read the newer transcript and appended
     a turn with the Linux `cwd`. Both then stopped at "OAuth session
     expired and could not be refreshed": the VPS's Claude login (agent
     and root alike) has lapsed. Not logged in again for the spike.
     Whether the model remembers after a Windows to Linux move is left
     for step 11, once the VPS is logged in; nothing found here suggests
     it would differ.
8. **The mesh and the roster.** Joining once joins everyone, the roster
   with its number, session owners, cast out, view only per link,
   "asleep", "silent" and "out of reach" on the tiles. Pure and tested:
   merging two rosters, the owner rule, refusing a cast out key.
   Verified with three dev instances.

   Done on 2026-10-25, its last parts by step 18 (below): "asleep",
   "silent" and "out of reach" and view only per link came with steps 3
   and 13, session owners with step 13, and joining once joins everyone,
   the roster merged and carried, owners in it and cast out travelling in
   it with step 18. Its three dev instances were two Windows dev
   instances and a dev node on `exit1-eu`.
9. **The shared lore.** Entries with a writing brother and a number,
   removal as an entry, the catch up on connect and the stream after,
   projects matched by repository, vectors embedded on arrival. Pure and
   tested: merging two copies in any order gives the same lore, a
   removal is not undone by a brother that was away. Verified with two
   dev instances: a note left on one is recalled on the other, also
   after the link was cut while it was written.

   Seen on 2026-10-24 with "Alpha" and "Beta" over their brother
   channel, each in a clone of one bare repository: a note left on
   either was recalled on the other within seconds, by meaning (the
   arrived note had its vector), and one left on Alpha while Beta was
   stopped arrived when they relinked. It needed the client side to say
   its own `lore` after hello and to take in the `entries` it hears;
   until then only the serving half existed. Not seen: dialogue passages
   crossing, since cmd.exe sessions leave no transcripts.
10. **Handoff between Windows PCs.** The bundle (transcript, record, work
   as a private ref and a `git bundle`), finding or cloning the project,
   a worktree for the session, the transcript filed under the new path,
   the resume with its first line, the sender letting go, the owner
   changing on `took`. "Hand off to" on a tile and "Leaving" in the tray.
   Verified with two dev instances on this machine, then between this PC
   and the laptop: a session mid task handed over, its next turn
   continuing the work with the files in place.

   Built on 2026-10-24: the steps in `horadric-core/src/handing.rs`
   (the sender as a state machine, packing from the repository's top,
   landing in a worktree beside the receiver's main tree on a branch
   named after the sender's), the glue in `horadric-ui/src/handing.rs`.
   On the wire, two changes to what is above: before cutting the bundle
   the sender asks `{"t":"bases","id","repo"}` and the receiver answers
   `{"t":"bases","id","commits","found"}`; and `took` carries the
   roster's number as `number`, since an answer with `n` on a link is
   taken for an ask. A refusal is `error` with `"for":"bundle"`. The
   first line goes in as the `UserPromptSubmit` hook's
   `additionalContext` of the session's next prompt. The memory skips
   the turns of a transcript read at a new path that it already holds
   dialogue of, by time, so a move indexes its old turns once.

   Seen with "Alpha" and "Beta" through `wss://portal.horadric.dev` by
   the phone door: a haiku session in a scratch repository, `app.py`
   changed and `notes.txt` untracked, handed from Alpha's tile menu.
   Beta answered `bases` and `took`, its worktree `repo-b.main-2` held
   both changes unstaged, the transcript was filed under that folder's
   key with every `cwd` moved, and `claude --resume` showed the
   conversation there. Alpha's tile went and came back in "repo-b ·
   Beta" with Beta's mark, "moved here from Alpha". The next prompt on
   Beta carried the moved line as hook context. The account had hit its
   session limit, so no model turn ran on either side: that the agent
   answers from the moved conversation rests on step 7.

   Not yet: the owner in the roster does not change on `took` (rosters
   keep no session owners until step 8); a project the receiver lacks is
   refused rather than cloned or asked for; a folder without git cannot
   be handed off; a session handed from the main checkout rather than a
   worktree leaves its changes there as well, since removing a human's
   uncommitted work is not the handoff's to do; `handoff` asked by a
   brother over the wire is served but no UI sends it yet.

   The tile keeps its place since 2026-10-24 (`afar::Left`,
   `far_sessions`): a tile that goes holds its cluster and slot for 180
   seconds, and the same session id arriving on another brother takes
   them, in whichever order the two lists come; its line reads "on
   Alpha: ..." and it wears Alpha's mark. Seen with "Alpha" and "Beta"
   (haiku, a scratch repository): handed from Beta to Alpha, Beta's tile
   stayed second in "b-repo" with Alpha's mark. The place lives in
   memory only, so a client started again shows it in the brother's own
   cluster. Brothers are keyed by id on the client (a PC's relay id, a
   node's `node-<name>`), shown by name, so two of one name keep apart.

   Seen with a model turn on 2026-10-24, "Alpha" and "Beta" over their
   linked brother channel, haiku in a scratch repository: turn 1 on
   Alpha took the codeword TANGERINE, changed `app.py` and left
   `notes.txt` untracked; handed from the tile between turns, Beta's
   next turn named the codeword and both files and appended to
   `notes.txt` in its worktree, the moved line in its prompt's context.
   Handed back, then to Beta again with "Now" during a 60 second ping:
   the turn was cut, and Beta's next turn named both codewords and
   finished the edit left undone. One `claude.exe` ran throughout.
   Alpha shows the moved tile in "repo-b · Beta" with Beta's mark. A
   review found and fixed on the way: a link that drops after the parts
   went no longer fails the handoff (only the 180 second limit does),
   the sender asks `bases` again once back and a receiver that runs the
   session answers `took`; a part's `of` is capped at 2048 and a brother
   at 4 half sent bundles, quiet ones dropped after 10 minutes; a first
   part starts its bundle over; a `prefix` that leads out of the
   worktree lands at its top.

   Gaps closed on 2026-10-24 (`horadric-core/src/placing.rs`, the
   receiver's half in `horadric-ui/src/handing.rs`). The `bases` ask
   carries `place` (the URL to clone, the folder's name, the sender's
   name, whether it is a folder without git). A receiver without a
   checkout answers `"placing":true` and asks its human: "Clone <url>
   into <folder>?" beside where most of its recent projects are (else
   `Documents\Github`, else the profile), or "Pick a folder"; with no
   remote only a folder, and the bundle is then the whole history. The
   sender waits up to 15 minutes, asking again every 3 seconds; a no
   comes back as `bases` with `text`. A picked folder holding the
   project is used, an empty one filled, any other gets a new folder
   inside it, so nothing there is written over. A folder without git
   travels as a plain copy of the whole folder rather than of the files
   changed since the session started: the receiver has nothing of it,
   files only read are as needed as those written, and a file's time
   says little. It leaves out `node_modules`, `target`, `.venv` and the
   like, carries at most 64 MB and 20000 files, leaves any file over 16
   MB behind and names those in the first line. A session that ran in
   the human's own checkout or a folder without git leaves its changes
   there, and the sender's toast says "The changes stay in <folder>
   too". A brother's tile offers "Hand off to ▸" another brother or
   "This PC", which sends `handoff` to its owner; the same ask for a
   silent brother's tile is a takeover by its keeper. The phone's
   sessions message carries those as `take` beside `list` (so a brother
   hearing the list does not take them for the PC's own), and the phone
   shows them under "Silent brothers" with "Take over", sending
   `handoff` to the PC.

   Seen with "Alpha" and "Beta" linked by the phone door, cmd.exe
   standing in for the agent and the hooks faked: on Beta's tile of
   Alpha's session, "Hand off to ▸ This PC (Beta)"; Beta, with no
   checkout, asked to clone the scratch remote into the folder beside
   its project, cloned it, and the session landed in `remote.main-2`
   with both changes, its tile "moved here from Alpha"; Alpha's
   checkout kept them. A folder without git went from Alpha's tile menu
   to Beta, picked as the scratch folder, and landed in a new
   `plain-a-2` beside the original; handed back through Alpha's tile of
   it, a dismissed question failed it on Beta as "Alpha says its human
   said no" and left it paused there, and the next try landed in
   `plain-a-2-2` with Beta's toast "It runs on Alpha now. The changes
   stay in ...\plain-a-2 too." Not seen: the phone's "Take over", which
   needs a keeper holding a silent brother's session.
11. **Handoff to and from a Linux node.** The same over `--stdio`, and the
    path rewrite between Windows and Linux. Verified against the VPS both
    ways.

    Done on 2026-10-24 (`horadric/src/node_handing.rs`, the node's state
    in `node_state.rs`, the Windows half in `horadric-ui/src/node_asks.rs`).
    A Windows PC reaches a node only as a viewer, so both ways go on that
    one ssh link. The PC's hello to a node carries `"name"` (its PC name),
    which is how the node knows whom a `handoff` is to; the node sends its
    `bases` ask and `bundle` parts to that viewer, and the PC tells them
    from answers by what they carry (`handing::asks_receiver`: a `bases`
    with `repo`, or a `bundle`), serves them as a brother's and sends the
    answers back stamped. `took` and a refusal reaching a server go to a
    new `Serve::handing_answer`, which only the node implements. The node
    lands in a worktree of its own checkout, found by repository identity
    among the folders it knows and the git checkouts up to two folders
    below home. It now keeps `node.json` (sessions with their names,
    arguments, conversations and worktrees, and the recent folders) in
    its state folder, so `resume`, `create` and `folders` work and a
    restart brings sessions back; the unit has `KillMode=process`, since
    systemd otherwise ended every session host with the node. Screens are
    still not served: a node keeps no terminal grid.

    Seen on screen with a dev instance ("L11PC", its own `APPDATA`)
    against `exit1-eu`, haiku in a scratch repository cloned on both
    sides from one bare remote under `agent`'s home: turn 1 on Windows
    took the codeword QUARTZLYNX, changed `app.py` and left `notes.txt`
    untracked; "Hand off to ▸ vps" moved it, the tile came back in
    "l11repo.main-2 · vps" with "moved here from L11PC", the worktree held
    both changes, every `cwd` read `/home/agent/l11/l11repo.main-2`, and
    turn 2 there named the codeword, appended to `notes.txt`, edited
    `app.py` and said it worked in the Linux folder, the moved line in
    its prompt's context. "Hand off to ▸ This PC" on that tile brought it
    back into a Windows worktree with the Linux edits, the node's
    worktree and branch gone, and turn 3 on Windows named the codeword
    and all three lines. A named session on the node kept its host and
    its name through `systemctl --user restart horadric`.
    Found on the way: under systemd the node's PATH lacks
    `~/.local/bin`, where Claude Code installs itself, so the node now
    looks there too; Claude Code 2.1.3 asks for trust in a folder below a
    trusted one, on Windows and Linux alike, so trust is written for the
    exact folder; a landing that cannot resume is undone, so the next try
    is not refused as a duplicate.

    Gaps closed on 2026-10-24. Nobody sits at a node to ask, so a node
    without a checkout clones the project's remote by itself, beside its
    other checkouts (else in home), answering `"placing":true` while the
    clone runs; with no remote it refuses: "no checkout of X here, and it
    has no remote to clone" (`placing::unattended`). A node handing to a
    PC sends `place` with its `bases` ask and waits while the PC's human
    decides, asking again every 3 seconds, as a Windows sender does. The
    PC's hello to a node also says `you`, the node's name in its list,
    which the node calls itself by toward that PC (else its host name),
    so both moved lines say "vps". Seen against `exit1-eu`: a haiku
    session in a scratch repository with no checkout on the VPS, handed
    to vps, was cloned into `~/l11g-one`, landed in `l11g-one.main-2`
    with both changes and its tile read "moved here from MORTEN"; a
    session started on the node in a repository the PC lacked, handed to
    the PC, showed the node "waiting for MORTEN to say where it goes",
    the PC's dialog "vps hands ... to this PC", and after Clone it landed
    as "moved here from vps".
12. **Keepers.** Turn end snapshots to the first keeper, kept in its
    state folder, pruned when the session ends or moves. Pure and tested:
    a snapshot's bundle against the last, applying a chain of them.
13. **Takeover.** "Take over" on a silent brother's tiles, the fencing on
    return, the orphaned branch, the warning line, automatic takeover as
    a switch. Verified by pulling the network cable on the laptop mid
    quest: the desktop takes over from the last turn end, the laptop
    comes back and pauses its copy.

    Steps 12 and 13 landed on 2026-10-24 (`horadric-core/src/keeping.rs`
    for the book and the holding, `keepers.rs` for snapshots,
    `horadric-ui/src/keeping.rs` for the glue). The keeper is picked in
    the tray ("Keeps my sessions", and "Take over by itself after 10
    minutes" once one is), kept in `keeping.json` beside `brothers.json`
    and folded into the served roster. A takeover lands as a handoff
    does (`handing::land`, `adopt_landed`): the keeper's bundle of the
    keep ref has its header renamed to the conversation's handoff ref.
    Seen with "Alpha" and "Beta" (dev, scratch `APPDATA`, the real
    relay, haiku): three turns in a scratch repo on Alpha, Beta held
    snapshot 3 (the first refused, the second whole, the third a 24 KB
    chain link). Alpha killed with its hosts; two minutes on, Beta's tile
    read "Alpha silent since 04:52" and offered "Take over on Beta",
    which landed the work in a worktree of Beta's clone and resumed it:
    its next turn named the codeword from Alpha's second turn and edited
    the landed files, its prompt opened by the takeover line. Alpha
    started again held the session, heard Beta's owners, kept it paused
    with "taken over by Beta at 04:54" (on Beta's view of its tile too)
    and its work, a file written after the last snapshot included, on
    `horadric/orphaned/<session>`. Found on the way: a keeper's
    repository deep in a long state folder passes 260 characters, so it
    runs with `core.longpaths` and writes no ref of its own for a
    takeover. Not yet seen: automatic takeover live (its rule is tested),
    a second keeper on the list, the phone offering "Take over", pulling a
    real cable between two PCs.

    Closed after: a snapshot carries the files of the session's side
    folder (`<claude id>/subagents`, `<claude id>/tool-results`) that are
    new or changed since the last one, whole, after the transcript's
    growth; the owner's book remembers each file's length the keeper
    holds, and a whole snapshot starts that over. The keeper files them
    under `side/` in the store, and a takeover lands them beside the
    transcript, so their paths are moved as a handoff's are. A store goes
    too once its brother has been linked ten minutes without listing the
    session (one the keeper never saw listed included), and any store a
    month old. What a keeper spends on disk is capped (below).

    Guarded since 56931e8: a keeper takes over only while its snapshot
    is still the session's newest word in the roster
    (`keeping::takeable`): its owner is the snapshot's brother under no
    higher number, or another under a lower one. A store that outlived a
    handoff (the keeper was restarting, or the owner fell silent within
    the prune look) is refused and dropped, by the tile, by the
    automatic takeover and by a node asked to take over, and asked
    again after landing and in the same change that takes the number,
    so a stale copy never fences the brother the session runs on now.
    A brother told `takeover` takes it only from the session's owner or
    a keeper that owner named (`mesh::may_claim`). Not yet guarded: a
    claim arriving in a served roster's sessions, which the merge still
    takes at the higher number. `horadric node keeper` asks the running
    node to change its book, so only one process writes `keeping.json`.
14. **The portable link.** `horadric-link`: ECDH, AES-GCM, random, the
    TLS WebSocket and HTTPS client, and a Windows test that CNG and it
    agree on keys, codes and sealed frames both ways.

    Done on 2026-10-24 (`crates/horadric-link`). `ecdh` keeps keys as
    CNG does (`04||x||y` public, `x||y||d` private) on `p256`, `crypto`
    seals as `portal_crypto` does on `ring`, `http` is `net.rs`'s
    `request` and `get` plus `post_form` and a `download` that streams to
    a `.part` file, following a GET's redirects across hosts without its
    `Authorization`. `ws::Socket` is `portal_link`'s `Dialled`: one thread
    blocks in `receive`, others `send`, `ping` or `close`, and
    `close_code` gives the relay's 4401 or 4503. TLS cannot be read and
    written at once, so the reader waits on a peek of the bare TCP socket
    holding no lock and reads under the lock with a 5 ms timeout, which
    is the most a send waits. `horadric-ui/src/link_interop.rs` runs CNG
    on one side and the link on the other, both ways: stored keys load,
    agreement, `joining::agree`'s code and key, sealed frames with and
    without data, a tampered or misbound frame refused. The pure half of
    WorkOS's device code flow is in `horadric-core/src/workos.rs`; both
    client ids hand out device codes with no dashboard change. Seen live,
    from Windows and from the static musl test binary run on `exit1-eu`:
    the relay as a v2 PC with no token answering `{"ping":1}` with a
    pong, a Hugging Face file through its relative redirect, the weights'
    redirect to the CDN with a `Range`, and a device code from WorkOS
    whose first poll says `authorization_pending`. The live tests are
    `#[ignore]`d: `cargo test -p horadric-link -- --ignored`.

    Cross building for musl now needs a C compiler, for ring. On Windows:
    `winget install zig.zig` and `cargo install cargo-zigbuild`, then
    `cargo zigbuild -p horadric-link --target x86_64-unknown-linux-musl`
    (zig compiles the C and links). CI's Linux job and the release use
    gcc and `musl-tools`.
15. **The join logic in core.** `joining.rs` and the state machine of
    `brothers.rs` behind `Crypto` and `Human`, the Windows UI unchanged
    to its human. Verified by step 2's run again with two dev instances.

    Done on 2026-10-24 (901f82b). `horadric-core/src/joining.rs` is the
    handshake, the link sessions and `brothers.json`, taking a `Crypto`
    (random, an ECDH pair as `x || y || d`, ECDH, `seal_bound` and
    `open_bound`). `horadric-core/src/brothers.rs` is the state over
    every channel of one relay link: `Brothers::new(id, name, dir,
    Hands)`, fed `connected`, `text` and `binary` by the link, driven by
    `ask`, `answer`, `cast_out` and `reply`, and working through `Hands`:
    the `Crypto`, a `Relay` (text and framed binary out), a `Human`
    (`let_in`, `confirm`, `tell`, answered later by `answer`) and a
    `Listener` (`Up`, `Heard`, `Asked`, `Down`). `standing.rs` moved too,
    for the cast out refusal. The UI keeps CNG, the message boxes and the
    relay link's sender. The derivations are pinned by vectors worked out
    in Python from `docs/PORTAL.md`, and a test runs the core handshake
    on CNG. Seen with "Alpha" and "Beta" through
    `wss://portal.horadric.dev`: the installed build joined them (656
    118) and the new build relinked from that `brothers.json` with no
    prompt; old and new relinked each way round; a fresh join on the new
    build showed 161 762 on both, No the default, and both kept the same
    key; after restarting both they relinked unasked and Beta showed
    "demo · Alpha"; cast out on Beta dropped the link, and Alpha linking
    again was closed with no prompt. Introductions (step 18) fit as one
    more sealed message `Brothers` handles before routing, and a key
    from static ECDH kept as a `Joined`, which the usual relink then
    uses.
16. **Lore on Linux.** `horadric-memory`, the code index and `horadric
    mcp` built for musl, CI's Linux job and the release building them,
    the node holding a lore and serving `lore_*`.

    Done on 2026-10-25 (805d560, 19bb50b, 9cb988a). The memory's thread
    and the sharing glue moved from horadric-ui into
    `horadric-memory/src/service.rs` and `sharing.rs` (`Lores` answers
    the three `lore_*` hooks), so the app and the node run one copy; the
    app hands in WinHTTP, the node `horadric_link::http::get`. The node
    keeps every session's transcript, answers recalls, starts `claude`
    with `--mcp-config` (lore tools only; a stand-in from
    `HORADRIC_AGENT` gets no flags) and lets its model go after ten idle
    minutes. A node links to no one, so the Windows side answers the
    node's `lore` with what it lacks (`App::far_lore`), Town Portal on
    or off, and a server tells its lore again when its projects change,
    so a project opened after the link came up shares at once. The musl
    binary went from 2.7 MB to 16.5 MB; the node idles at 3.5 MB and
    holds about 110 MB with the model (195 MB at the peak of a first
    catch up). Seen on `exit1-eu` with a dev instance linked over SSH,
    in a repository cloned on both sides: notes left on either were
    recalled on the other by `horadric mcp`, while linked within
    seconds and after the link was cut while they were written, in a
    project opened after the link too; the model came over
    horadric-link; Claude on the node answered from `lore_recall`.
    Built locally with zig (RELEASING.md, step 8).
17. **A node on the relay.** `horadric node signin` (device code), the
    node's secret, key pair and `brothers.json`, its relay link feeding
    relay channels into its one `Server`, the headless approval, and
    `roster`, `control`, `state` and keeping served. Verified against
    `exit1-eu` with no SSH entry: sign in on the VPS, approve on this
    PC, a session started from the stage running there.

    Done on 2026-10-25. The node's relay link is
    `horadric/src/node_relay.rs` (`horadric-link`'s crypto and WebSocket
    under core's `Brothers`, the ping, silence and backoff rules now in
    `horadric-core/src/relaying.rs` for both machines), sign in
    `node_signin.rs` (device code flow, `workos.json` owner only, refresh
    under a lock and a file lock), the human `node_control.rs` (a control
    socket the `signin`, `let-in`, `brothers`, `cast-out`, `view-only`
    and `keeper` commands speak to; with no terminal attached a join is
    refused), the node as a client of its brothers `node_kin.rs` (hello,
    `lore` both ways, answers to its handoffs), and keeping
    `node_keeping.rs`. What a PC and a node do alike moved to core: the
    served roster (`rostering.rs`), joining keep parts, filing a
    snapshot under the caps, cutting one, letting go and pruning
    (`keeper.rs`). Secrets and keys are written owner only
    (`portal::write_private`). Relay channels sit in the node's one
    `Server` beside its viewers, so a PC cannot tell a node from a PC.
    A PC's tile of a silent brother's session now also offers "Take over
    on <keeper>" when another brother keeps it, sent to that keeper as a
    `handoff`, since a node keeper has no tiles to click.

    Seen with a dev node "vps17" (a transient systemd unit, its own state
    folder) on `exit1-eu` and a dev instance "PC17" (scratch `APPDATA`),
    no SSH entry, through `wss://portal.horadric.dev`: `horadric node
    signin` printed the device link, confirmed in the browser pane with
    the staging user's Google account; the node asked PC17, both showed
    823 364, yes on both, and each kept the other. PC17 showed "demo17 ·
    vps17" with its mark; "+" there started bash on the VPS in the
    stage, typing reached it. Restarting either side relinked unasked; a
    SIGTERM showed "leaving" first. A haiku session handed from PC17 to
    vps17 by its tile landed in a worktree of the node's checkout, "on
    vps17: moved here from PC17", and its next turn named the codeword
    and edited there; handed back by "Hand off to ▸ This PC" it landed in
    a Windows worktree with the Linux edits. Lore went both ways: a note
    left on PC17 was recalled on vps17 and one left there recalled in a
    new PC17 session. Each kept the other's snapshots (`kept ... true`),
    and the node pruned its copy once the session left it. vps17 killed
    with its hosts: two minutes on, PC17's tile read "vps17 silent since
    01:07" with "Take over on PC17", which landed the node's session from
    its snapshot; the node started again heard the owners, stopped its
    copy with "taken over by PC17 at 23:09" (UTC) and kept a file written
    after the snapshot on `horadric/orphaned/<session>`. Cast out from
    PC17's tray (where vps17 sits under Brothers like a PC): the link
    dropped, and the node relinking was closed unasked.

    Not seen: a node keeper taking over, by itself or asked from a PC's
    tile; a node with a terminal on `let-in` while a PC asks it. Found on
    the way: a node started session (`horadric new`) still meets Claude
    Code's trust prompt, which only landings and `create` answer. Left
    for step 18: the node keeps a brother that cast it out until the
    roster carries casting out; a node joined by SSH and by signing in
    shows twice.
18. **The mesh.** Introductions, received rosters merged, cast out and
    owners carried by the roster, an SSH node that signs in folded into
    its relay brother. Verified with two dev instances and the VPS: the
    VPS let in by Alpha links to Beta with no code, cast out on Beta
    drops it on Alpha too.

    Done on 2026-10-25 (1b47045, 6175d31). `roster.json` in the state
    folder (`horadric-core/src/mesh.rs`) holds every roster heard,
    merged, and every session's owner: keeping's `owners` moved into it
    (an older `keeping.json` gives them up once), so handoffs, takeovers,
    `takeover` messages and rosters all write one place. A brother serves
    that, without anything said of itself, merged with its own entry put
    together afresh (numbered past every number it heard, as a clock that
    only goes up) and an entry numbered nought per brother it holds. A
    brother is an entry per key, so one cast out and let in again under
    its old id is two, the old one refused for good; a heard roster never
    speaks for the hearer. Introductions are the roster: an entry's
    `holds` says whom it holds a key for, and a brother takes a key for N
    when one it holds, under the key it holds, lists N (`Roster::
    introduced`), from `joining::introduced_key` (static ECDH, both keys
    and ids, lower id first, pinned against Python). The lower id dials;
    a hello that beats its introduction waits and is heard again. Core
    `Brothers` takes rosters and `cast_out` before routing them, so a PC
    and a node do it alike, and tells the app `Gone` (tiles go) and
    `Moved` (sessions fenced, unless being handed to that brother). Cast
    out: the outcast is told sealed (at once, or once its hello proves its
    old key) and lets every brother and its key pair go; asking to join
    under its old id or key is refused unasked, so it comes back only when
    a brother asks it, with a new key. A node's hello over `--stdio` says
    its relay id and key; a PC holding that brother under that key shows
    it once, the relay link preferred and `ssh` standing by. A session
    another brother owns now is not shown on a silent brother's kept list
    too. A name two brothers share is shown with the start of each key,
    and names one of them nowhere. A node's own sessions get Claude Code's
    trust written for their folder.

    Seen on 2026-10-25 with "Alpha" and "Beta" (scratch `APPDATA`, Beta on
    43119) and a dev node "vps18" on `exit1-eu` (its own state folder,
    staging sign in confirmed in the browser pane), through
    `wss://portal.horadric.dev`: Alpha and Beta joined (562 738); `horadric
    node signin --to Alpha` and Alpha's yes (032 492) let the node in, and
    within a second the node logged "Beta was introduced" and linked to
    Beta, nobody asked. Beta showed "demo18 · vps18" and "alphaproj ·
    Alpha"; all three `roster.json` agreed. Cast out on Beta's tray: the
    node was told and let both go, Alpha dropped it from Beta's roster and
    its tiles went; `signin --to Alpha` again was refused with no prompt.
    Alpha's "Ask to join" with `horadric node let-in` attached took it
    back (454 009) with a new key, Beta took it from Alpha's roster, and
    the old key stayed refused everywhere. Cast out again while the node
    was stopped: started, it relinked to Alpha under its old key and was
    told so. An SSH entry for the same node on Alpha folded into it: one
    "demo18 · vps18" cluster, the tray's "vps18 linked, ssh standby" and
    no line under Linux nodes; Town Portal off on Alpha, "goes on over
    ssh" and the tiles stayed lit, back on, the relay again. Keeper
    vps18: Alpha killed with its hosts, two minutes on Beta's tile read
    "Alpha silent since 02:13" with "Take over on vps18", which the node
    did into a worktree of its clone with the turn's edit; Alpha back
    fenced its copy onto `horadric/orphaned/<id>` from the roster alone.
    With the switch on, a second session was taken over by the node by
    itself ten minutes after Alpha went, `notes.txt` and all, both
    resumed with `claude --resume`. `horadric new` on the node wrote
    trust for its folder.

    Found on the way: Town Portal turned off left brother links counted
    up (the reader's down came after the brothers were gone); a node
    whose agent is a stand-in cannot resume a Claude session, so takeover
    tests run without `HORADRIC_AGENT`. Left open: a brother let in by one
    later cast out stays, when others met it meanwhile (cast it out too);
    a brother that lost `brothers.json` but kept its secret rejoins with a
    new key under its old id, and brothers holding its old key keep it
    until it is cast out; roster owners are never pruned.
19. **Later, each its own decision.** Teams: a WorkOS organization as a
    brotherhood of several people, each seeing the others' sessions view
    only by default, and the lore shared or not per project; catching
    the lore up from Drive; the Mac as a brother (its portal has no
    crypto yet either); remote files and the git glance over the same
    protocol, read only first; Codex and Grok Build handoff; a quest
    moving with its session, so the quest log on the receiver knows it.

## Pure and tested

The v3 messages both ways, splitting and joining `out` and `bundle`, the
size rule, refusing what a view only link sends, never serving a session
another brother owns, merging rosters, the owner and fencing rule, the
silent and asleep timers, picking the keeper, the dimmed tile's line, the
stdio framing, the systemd unit's text, the transcript path rewrite,
the PKCE pair and the sign in URL, merging two lores and what to send a
brother given its `have`, matching a project by repository.

## Open questions

- Should a phone see the whole brotherhood through the PC it is paired
  with? The old answer was no: one stolen phone would reach every
  machine. A brotherhood makes the ask stronger. A per brother switch,
  off by default, is the likely answer.
- A quest's session handed to another brother: does the quest move with
  it, or stay on the brother whose quest log it is in? Warriv on two
  brothers working the same repository would need to agree on who runs
  what.
- How much disk does a keeper spend? Answered: at most 2 GB for every
  session together and 512 MB for one, `keep_cap_bytes` and
  `session_cap_bytes` in the keeper's `keeping.json`. Past the total the
  oldest stores go first (by their last snapshot), never one whose owner
  is not awake right now, since that is the snapshot a takeover needs,
  and never the one that just came. A session past its own cap is
  dropped and marked full, and the keeper answers `kept` with
  `"full":true`, so its owner stops sending it there rather than sending
  it whole every turn.
- Handing off a plain terminal: a shell has no transcript to resume, so
  probably never, only its folder.
- In a team, should dialogue be shared at all? The lore holds
  conversations word for word. Notes and quest summaries are meant for
  others; a teammate's dialogue may not be. Likely notes and quests by
  default, dialogue only within one person's own brothers.
- Who pays when teams use SSO? AuthKit is free, enterprise SSO is priced
  per connection. Fine to defer until a team asks for it.

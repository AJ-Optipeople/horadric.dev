# Town Portal: the protocol

Horadric from a phone, through a relay. Why it is built this way is in
the lore (`lore_recall` "Town Portal"). This file is the contract between the
three parts:

- **The PC side**, in this repo: `horadric portal` and the UI's connection
  out to the relay.
- **The relay**, in `../horadric.portal`, a small Node server on our VPS
  at `portal.horadric.dev`. Anyone can run their own.
- **The page**, a PWA the relay serves at `https://portal.horadric.dev/`.

## Pairing

On the PC, `<data dir>/portal.json` holds:

```json
{ "enabled": true, "relay": "wss://portal.horadric.dev", "secret": "<b64url, 32 random bytes>",
  "phones": [{ "id": "<b64url, 8 random bytes>", "name": "iPhone, Safari", "key": "<b64url, 32 random bytes>", "paired": 1760000000, "seen": 1760000000 }],
  "ticket": { "key": "<b64url, 32 random bytes>", "until": 1760000300, "used": false } }
```

`<data dir>` is `%APPDATA%\Horadric`, or `%APPDATA%\Horadric-dev` for a dev
instance. b64url is base64url without padding. Times are Unix seconds.

- The PC's **id** is the first 32 hex characters (lowercase) of the
  SHA-256 of the secret string's UTF-8 bytes. The relay computes it the
  same way, so it needs no storage to know which PC is which, and nobody
  without the secret can pose as the PC.
- **Every phone has a key of its own.** So one phone can be forgotten
  without the others, and a key leaked from one phone opens nothing
  once that phone is forgotten. A phone not seen for 30 days is
  forgotten by itself.
- **The QR code holds a ticket, not a key.** A ticket works once, for 5
  minutes. A QR code seen over a shoulder, posted in a screenshot or
  left in a browser's history is dead by the time anyone tries it.
- The **pairing link** is
  `https://portal.horadric.dev/#r=<relay, URI-encoded>&id=<id>&p=<ticket key>&x=<until>&n=<PC name, URI-encoded>`.
  The fragment never reaches a server. The page stores it in
  `localStorage` (a list, one entry per PC, keyed by id), wipes it from
  the address bar, and trades the ticket for the phone's key at once.
- `https://portal.horadric.dev/pair.html#<same fragment>` shows the link as
  a QR code, drawn in the browser, for the phone to scan.
- `horadric portal` turns it on (making the secret if missing), opens a
  new ticket, prints the link, opens the QR page and waits, saying which
  phone paired or that the code ran out. `horadric portal phones` lists
  the phones, `horadric portal forget N` forgets one and `forget all`
  every one; a phone connected at that moment is cut off within 2
  seconds. `horadric portal off` turns it off. The UI looks at
  `portal.json` every 2 seconds and connects or disconnects to match.
- The stage's caption has a Town Portal key beside the window's keys: a
  rolled scroll while the portal is off, a blue portal once it is on,
  swirling while a paired phone is connected. Clicked while off, it does
  what `horadric portal` does without the wait; while on, it offers
  "Pair a phone..." and "Close the portal" (off, and the open ticket
  dropped).

Trading the ticket, sealed with the ticket's key:

- Phone to PC, `{"t":"pair","v":2,"name":"<what the phone calls itself>"}`.
- PC to phone, `{"t":"paired","phone":"<id>","key":"<the phone's key>"}`.
  From then on both ends use that key, and the phone says hello.
- Or `{"t":"error","code":"used"}` when another phone took the ticket
  first (the PC keeps a used ticket's key until it runs out, so it can
  say so), or `"code":"expired"`. A frame sealed with a ticket that ran
  out opens with no key the PC has, so it gets no answer at all, and
  the page says the code ran out by itself, from `x`.

Horadric 0.21 put one shared key in the link (`k=` instead of `p=`),
good until a rekey. The page still speaks to such a PC; a 0.22 PC
ignores that key, so its phones pair again.

## The relay

WebSocket endpoints, both on the relay's address:

- `/pc`, with the header `X-Horadric-Secret: <secret>`: a PC. The relay
  computes its id. A second PC connection with the same id replaces the
  first (a reconnect). The secret never goes in the address, where a
  proxy's access log keeps it; 0.32 and older send `/pc?secret=<secret>`,
  which the relay takes until 2027-01-01 when no header came. One address
  may hold 32 PCs open (429 past that). A phone's id is a hash of the
  secret, no credential, so `/phone?id=` keeps it in the address.
- `/phone?id=<id>`: a phone. It may connect while the PC is offline and
  waits.

Frames:

- **Relay to phone, text:** status, unencrypted JSON:
  `{"relay":"online"}` or `{"relay":"offline"}`, sent on connect and
  whenever the PC comes or goes.
- **Phone and relay, binary:** an encrypted payload, passed through as is.
- **Relay to PC, text:** control JSON: `{"open":<ch>}` when a phone
  connects (and for every waiting phone when the PC connects),
  `{"close":<ch>}` when it goes. `<ch>` is a u32 the relay picks, never
  0 and never 0xFFFFFFFF.
- **PC and relay, binary:** 4 bytes big endian channel, then the
  encrypted payload. From the PC, channel 0xFFFFFFFF goes to every phone
  of that PC.

Limits: 1 MB a frame, 8 phones a PC, a ping every 30 seconds both ways.
The relay keeps nothing on disk and logs no payloads.

## Encryption

Every payload is AES-256-GCM with the phone's key (or the ticket's,
while pairing): 12 random bytes of nonce, then the ciphertext with its
16 byte tag appended. The plaintext is UTF-8 JSON. The PC learns which
phone a channel is from the key that opens its first frame, and drops
any frame no key opens, so the key is also the proof a phone was
paired.

**No replays.** The PC's answer to hello carries `"s"`, 16 random bytes
b64url, new for every connection. Every later message from the phone
carries that `"s"` and `"n"`, a count from 1 that only goes up. The PC
drops a message with another `s` or an `n` it has passed, so the relay,
which sees every sealed frame, cannot play an old `input` back to the
PC, on this connection or another.

When a phone is forgotten while connected, the PC sends it
`{"t":"unpaired"}` and ignores it from then on; the page drops the key.
A page whose hello gets no answer in 10 seconds says the PC may have
forgotten it.

## Messages

Inside the encryption. Every message has `"t"`.

Phone to PC:

- `{"t":"hello","v":2}`: first thing after `open` (after `paired`, when
  pairing). 0.21 pages send `v` 1.
- `{"t":"watch","id":"<session id>"}`: stream this session's screen. One
  at a time per phone; a new watch replaces the old.
  `{"t":"unwatch"}` stops it.
- `{"t":"input","id":"<session id>","data":"<text>"}`: typed into the
  session as if at the keyboard. `\r` is Enter, `\u001b` is Escape,
  `\u001b[A` is Up, and so on.

PC to phone:

- `{"t":"hello","v":2,"pc":"<computer name>","s":"<connection id>"}`:
  the answer to hello. A 0.21 PC says `v` 1 and sends no `s`.
- `{"t":"sessions","list":[{"id","name","project","phase","line"}]}`: on
  hello and whenever the list or a phase changes. `phase` is `working`,
  `waiting` (needs the human: a permission, a question), `idle` or
  `ended`. `line` is what it waits for or last did, may be empty.
  A PC that keeps a silent brother adds `"take":[{"id","name","project",
  "line","to"}]`, that brother's sessions it can take over; the phone's
  "Take over" sends `{"t":"handoff","id","to"}` with them.
- `{"t":"screen","id","cols","rows","lines":["..."],"styles":[[[n,fg,bg,f]]],"cursor":[row,col]}`:
  the watched session's text, up to 500 lines of scrollback first, then
  the visible screen, each line without trailing spaces. On watch and
  whenever it changes, at most 4 times a second. `cursor` counts from
  the first line sent.
- `styles` has one list of runs per line. A run covers the next `n`
  characters (code points). `fg` and `bg` are `0xRRGGBB` numbers, or
  `null` for the terminal's own colours, which the page picks itself.
  `f` adds 1 bold, 2 italic, 4 underline, 8 strikethrough. Characters
  past a line's runs are plain, so a plain line has `[]`. Dim, inverse
  and hidden are already applied to the colours. A PC before 0.23 sends
  no `styles`, and its text draws plain.
- `{"t":"error","text":"..."}`.

A phone that gets a `v` it does not know tells the human to update
Horadric on the PC.

## Added in 0.21

Still `v: 1`: a page that does not know a message ignores it, and so
does a PC.

Starting and resuming:

- PC to phone, `{"t":"projects","list":[{"path","name"}]}`: on hello and
  whenever it changes. The projects the tiles show, the same order.
- Phone to PC, `{"t":"start","project":"<path>"}`: a new session in that
  project, started exactly like `horadric new` there. The PC answers
  `{"t":"started","project","id"}` once the session exists (it is then
  in `sessions` too), or `error`. The page then watches it.
- `sessions` entries gain `"paused": true` for a session with no live
  host (one Horadric paused or that ended its process). Phone to PC,
  `{"t":"resume","id"}` resumes it the way a click on its tile does;
  the screen follows once it is up. `watch` on a paused session answers
  `error` with "paused", and the page offers Resume.

Push, sent by the PC itself, so no server of ours holds subscriptions:

- PC hello gains `"vapid":"<b64url uncompressed P-256 public key, 65 bytes>"`.
  The PC makes the key pair once and keeps it in `portal.json`
  (`vapid_d`, `vapid_pub`).
- Phone to PC, `{"t":"push","sub":{"endpoint","keys":{"p256dh","auth"}}}`:
  the PC stores it in `<data dir>/portal-push.json`, one per endpoint.
  `{"t":"unpush","endpoint"}` removes it.
- When a session turns `waiting`, the PC POSTs to every stored endpoint:
  no body, `TTL: 3600`, `Urgency: high`, `Authorization: vapid t=<JWT>,
  k=<vapid_pub>`, the JWT ES256 with `aud` the endpoint's origin, `exp`
  12 hours on, `sub` `mailto:portal@horadric.dev`. At most one push a
  minute per PC. An endpoint answering 404 or 410 is dropped. With no
  body the push carries nothing, so the service worker shows "A session
  waits for you" and opens the page on a click.
- Each stored subscription notes the phone it came from, and a push
  goes only to phones still paired. `forget all` clears
  `portal-push.json`.

Liveness:

- PC to relay, text `{"ping":1}` every 25 seconds; the relay answers
  text `{"pong":1}`. A PC that has heard nothing from the relay for 70
  seconds drops the socket and reconnects. The page does the same with
  its own text ping, and reconnects when it comes back to the
  foreground.

## Added in 0.22: projects the PC has not opened

A project does not have to be on the tiles to start a session in it
from the phone, and a phone can make a new one.

- Phone to PC, `{"t":"folders"}`. The PC answers
  `{"t":"folders","roots":[{"path","name","list":[{"path","name","git"}]}]}`.
  The roots are the folders the human's projects live in: the parent of
  every project on the tiles, in the recent list and in session history,
  without duplicates. Each root lists its subfolders one level down,
  hidden ones and dot folders left out, most recently changed first, at
  most 300. `git` says whether it has a `.git`.
- `start` accepts any folder from that answer as well as a tile project.
  Anything else answers `error`. A session started this way puts the
  project on the tiles like any other.
- Phone to PC, `{"t":"create","root":"<root path>","name":"<folder name>"}`.
  The root must be one of the roots above. The name is 1 to 64
  characters of letters, digits, space, `.`, `-` and `_`, not starting
  with `.`, and must not exist yet. The PC makes the folder, runs `git
  init` in it (skipped if git is missing), starts a session there and
  answers `started` as for `start`, or `error` with the reason.

## Added in 0.24: what a waiting session asks

- `sessions` entries of a session that waits on the human (`phase`
  `waiting`, not paused, not a failed turn) gain `ask`:
  `{"kind","tool","detail","options":[{"label","keys"}],"since"}`.
  - `kind` is `permission` (may it run a tool), `question` (Claude
    Code's AskUserQuestion) or `input` (anything else, answered by
    typing).
  - `tool` is the tool's name, or `null`.
  - `detail` is what to decide on: the command for a shell, the path
    for an edit or a write, the address for a fetch, the plan for
    ExitPlanMode, the question's text, or the agent's own words. At
    most 2000 characters, cut with `…`.
  - `options` are the choices on the agent's prompt, each with the
    `keys` to send as `input` to pick it. A permission has `Yes` (`1`),
    `Always` (`2`, only when Claude Code offers one) and `No` (Escape).
    A question has its choices, `1` for the first and so on. Empty when
    the answer has to be given on the terminal: several questions at
    once, several answers to one, ExitPlanMode, `input`, and any agent
    but Claude Code, whose prompts number their choices differently.
  - `since` is when it began waiting, Unix seconds.
- A page that does not know `ask` shows the session as before.

## Added in 0.24: a photo from the phone

- Phone to PC, `{"t":"attach","id":"<session id>","name":"<file name>","data":"<b64url bytes>"}`.
  The page shrinks the picture to a JPEG of at most 700 KB first, so the
  sealed frame stays under the relay's 1 MB.
- The PC saves it as `<data dir>\portal-attach\<session id>\<unix
  time>-<name>`, the name cut to its last part and to letters, digits,
  `.`, `-` and `_`, and answers
  `{"t":"attached","id","name","path":"<the full path>"}`, or `error`
  when the session is not on the PC or the file cannot be written. The
  page puts the path into the message being typed, for the agent to
  read.
- Each attach first removes files in `portal-attach` older than a week,
  and the session folders that leaves empty.
- A PC before 0.24 ignores `attach`; the page gives up after 30 seconds
  and says the PC may need a newer Horadric.

## Added in 0.24: quests and Warriv

A phone sees each project's quest log and can add to it. Read and add
only: nothing drives Warriv or takes a quest from the phone yet.

- Phone to PC, `{"t":"quests"}`. The PC answers
  `{"t":"quests","list":[{"project","name","mode","warriv":{"on","drives","line"},"done","quests":[{"title","state","session","why","notes"}]}]}`,
  one entry per project on the tiles, in their order, and sends it to
  that phone again whenever it changes, for as long as it stays
  connected.
  - `mode` is the log's mode: `manual`, `review`, `warriv` or `auto`.
  - `warriv.on` says Warriv hears the project's events, `drives` that
    it drives it (starts and lands quests by itself), `line` what the
    quests tile's Warriv line says now, may be empty.
  - `done` counts the finished quests, which are not listed.
  - `quests` are the rest, in the log's order. `state` is `open`,
    `after` (waits for another quest), `tangled` (its `After:` lines
    name no quest, several, or itself), `working`, `review` or
    `blocked`. `session` is the id of the session that holds it, or
    `null`. `why` is what it waits for or why it is blocked, may be
    empty. `notes` are its first 12 notes lines.
- Phone to PC, `{"t":"quest_add","project":"<path>","title","notes"}`:
  adds an open quest at the end of that project's log, with the notes
  under it, as the quests tile's plus does. The project must be one of
  the tiles'. The title is put on one line, at most 200 characters;
  notes at most 8000, blank lines dropped. The PC answers
  `{"t":"quest_added","project","title"}` or `error`, and the new
  `quests` follows.

## Added in 0.24: long press a session

A long press on a session in the list, or on the watched session's
title, offers Rename, Copy last reply and Stop.

- Phone to PC, `{"t":"rename","id","name"}`: names the session as its
  tile menu's Rename does. The PC makes the name one line of at most 80
  characters; an empty name gives the naming back to Claude's own
  title. The new name arrives in `sessions`.
- Phone to PC, `{"t":"stop","id"}`: ends the session as closing its pane
  does: the agent's process, its tile and its saved place. The page asks
  first. The session leaves `sessions`.
- Phone to PC, `{"t":"reply","id"}`. The PC answers
  `{"t":"reply","id","text"}`: the agent's last message, as its last Stop
  hook carried it, or, when this Horadric was not up to hear that hook,
  the newest reply in the transcript's tail. `text` is `null` when the
  session has none yet. The page shows it and copies it.
- Any of the three for a session the PC does not know answers `error`.
  An older PC ignores them; the page says so when nothing changes in a
  few seconds.

## Added in 0.24: pushes that say what waits

The push still goes from the PC straight to the phone's push service, now
with a body only the phone can read.

- The body is the JSON `{"pc","id","project","title","body","waiting"}`,
  encrypted for each subscription apart as RFC 8291 says: `aes128gcm`
  (RFC 8188), one record of size 4096, a fresh ECDH P-256 key pair and
  salt per push, the subscription's `p256dh` and `auth`. The POST gains
  `Content-Encoding: aes128gcm` and `Content-Type:
  application/octet-stream`. The push service sees only ciphertext.
- `pc` is the PC's id, `id` the session that started waiting (of several
  at once, the least id), `project` its project's name. `title` is short
  enough for a lock screen: "Allow Bash?" for a permission, "Waiting for
  you" for a question or an idle prompt, "A dialog waits", "The turn
  failed". `body` is "project · session", then on a second line what it
  said when the title does not say it already. `waiting` counts every
  session of that PC waiting now. The JSON stays under 4079 bytes.
- The service worker shows `title` and `body` with the session as the
  tag, so a second push about the same session replaces the first, and
  sets the app badge to `waiting`. A click opens the page at
  `/#s=<session id>&pc=<pc id>`, or tells a page already open, and the
  page shows that session. While open, the page sets the badge to the
  sessions waiting and clears it at none.
- A push without a body (a PC before 0.24, or a subscription whose keys
  will not encrypt) shows "A session waits for you" as before. A page
  before 0.24 ignores the body and shows the same.

## Added in 0.24: answer from the notification

- The push's JSON gains `options`, at most two `{"label","keys"}` taken
  from the session's `ask`: Yes and No for a permission (never Always,
  which a lock screen tap should not grant), the first two choices for a
  question, none when the `ask` has none. Labels are cut to 40 bytes. It
  gains `since` too, the `ask`'s `since` in Unix seconds, 0 without one.
- Where the browser has notification actions (Android, desktop), the
  service worker shows the options as buttons. A tap opens the page at
  `/#s=<session>&pc=<pc>&a=<keys>&l=<label>&w=<since>`, or posts
  `{"t":"open","id","pc","answer":{"keys","label","since"}}` to a page
  already open.
- The page wipes the fragment, then sends the answer once as an `input`,
  when that PC's sessions list the session as still waiting, its `ask`'s
  `since` the same as `w` and its options holding the keys. Anything else
  drops the answer unsent, as does a wait of over five minutes for the
  list. Either way the page shows the session.
- iOS has no actions: there a tap opens the session as before. A PC that
  sends no `options` gets no buttons, and a page that does not know them
  ignores them.

## Added in 0.24: what a session changed

- Phone to PC, `{"t":"changes","id"}`. The PC answers
  `{"t":"changes","id","files":[{"path","added","removed"}]}`: each file
  the session changed, with the lines it added and removed, both `null`
  for a binary file. Paths are relative to the top of the session's
  tree, with forward slashes. Changes count from the session's last
  commit, or for a session in a worktree of its own from where its
  branch left the main tree, so its commits count too. Files git does
  not track yet count as all added. A session outside a git repository
  answers an empty list.
- Phone to PC, `{"t":"diff","id","path"}`, for a path from that answer.
  The PC answers `{"t":"diff","id","path","text","cut"}`: the file's
  `git diff` from the same commit, without colour, cut at the end of a
  line before 200 KB, `cut` true when it was. Any other path answers
  `error`, so a phone reads only what the session changed.
- Both run git away from the UI, so the answer may come after other
  messages. A PC before 0.24 ignores both, and the page shows no list.

## Protocol v3: raw streams

For another PC of the same human (a brother, see
`docs/plan/the-brotherhood.md`), which draws a session in a grid of its
own rather than reading a phone's screen text. A v2 phone is served
exactly as before.

- Hello gains `"kind":"phone"` or `"kind":"brother"`; a hello without
  `kind` is a phone. The PC answers in the client's own `v`, up to 3 and
  never below 2, so a page that sends `v` 2 still hears 2. The answer
  gains `"brother":"<computer name>"` and `"control":true|false`.
- `input`, `start`, `resume`, `create`, `handoff` and `takeover` on a
  link without control answer `{"t":"error","text":"view only","for":"<t>"}`
  and do nothing. Every link has control for now; a switch per brother
  comes with joining. (Since 0.33 the list is `NEEDS_CONTROL` in
  `horadric-core/src/brotherhood.rs`, and a `bases` ask carrying `place`
  is one too: it makes a node clone and a PC ask its human. A receiver
  clones only `https://`, `ssh://` and `user@host:path` remotes, with
  git told to allow no other protocol, and places at most two projects
  at once; one more is answered "placing" and asked again.)
- `{"t":"attach","id","cols","rows"}`, without `data` (with `data` it is
  the photo of 0.24): stream this session raw. The PC sends
  `{"t":"size","id","cols","rows"}`, the size the session's pseudo
  terminal has, then the output it keeps (the last MB, from the first
  line break once older output was dropped), then live output as it
  comes, each as `{"t":"out","id","seq","b64"}`. `b64` is the raw bytes,
  b64url without padding, at most 256 KB a message so the sealed frame
  stays under the relay's 1 MB. `seq` is the offset of the message's
  first byte in the session's output, so the next `seq` is this one plus
  its length. The first message of a replay carries `"reset":true` (and
  may have empty `b64`): the viewer starts a fresh terminal at the last
  `size` and draws into it. A replay comes again whenever the viewer
  fell too far behind or the session was resumed into a new console.
- Several attaches at once, one per session; `{"t":"detach","id"}` stops
  one, and a new hello stops them all. A session that is not running
  answers `{"t":"error","id","text"}` with "paused" or why, once, and the
  stream begins with a replay when it runs again.
- `{"t":"resize","id","cols","rows"}`: the size this viewer would draw
  the session at, sent whenever its pane changes. `size` follows
  whenever the session's size changes, to every viewer.
- Whose size wins: the viewer that last typed in the session, the PC's
  own stage included, like tmux's `latest`. An `input` with empty `data`
  counts as a click: it types nothing but takes the size. On a tie, or
  before anyone typed, the stage keeps it. A phone's attach, resize and
  input never move the size. Once the viewer that held the size
  detaches, or the stage types, the session goes back to the stage's
  size.
- `sessions` entries gain `"since"`: when the session took its phase,
  Unix seconds by the serving PC's clock. A brother's tile shows its age
  from it, so the age survives the client starting again. A client takes
  a time later than its own clock as now.

## Protocol v3: joining a brother

A PC signed in with WorkOS (`/pc?name=..` with the headers
`X-Horadric-Secret` and `Authorization: Bearer <access token>`, see the
relay's README, "Brothers"; one user may have 16 machines connected, and
one more is closed with 4429 and tries again later) is told which of the human's other PCs are
connected, and asks the relay for a channel to one with
`{"join":{"to":"<pc id>"}}`. The relay answers `{"link":<ch>,"id","name"}`
on the asker's side and `{"join":<ch>,"id","name"}` on the other's. The
ids and names below are always the relay's, never what a message says.

`<data dir>/brothers.json` holds this PC's ECDH P-256 key pair and every
brother it joined:

```json
{ "me": { "public": "<b64url, 65 byte point>", "private": "<b64url, x || y || d>" },
  "brothers": [{ "id": "<pc id>", "name": "Laptop", "public": "<b64url>", "key": "<b64url, 32 bytes>", "joined": 1760000000, "seen": 1760000000 }],
  "cast": [{ "id": "<pc id>", "...": "as in brothers" }] }
```

`cast` holds brothers cast out, with the link key each had, only so one
that comes back under it can be told so (see "Introductions and casting
out" below). It is absent while empty.

On a brother channel, until there is a key, frames are plain UTF-8 JSON:

- Asker: `{"t":"join","v":1,"name","commit":"<b64url>"}`, where `commit`
  is SHA-256 of `"horadric join commit v1"` then the asker's public key
  and its 16 byte nonce, each led by its length as a 4 byte big endian
  number.
- Giver: `{"t":"joining","name","pub","nonce"}`, its public key and nonce.
- Asker: `{"t":"reveal","pub","nonce"}`. The giver refuses one that does
  not hash to `commit`.
- Both: the ECDH secret `s` (the shared x coordinate), then the link key
  is SHA-256 of `"horadric brother key v1"` and the code SHA-256 of
  `"horadric join code v1"`, each over `s`, the asker's key and nonce,
  the giver's key and nonce, length led as above. The code is the first
  4 bytes big endian, mod 1,000,000, as six digits in two groups ("587
  941"). Both sides ask their human, No the default: the giver "Laptop
  wants to join. Let it in?", the asker "Does Desktop show the code 587
  941?". The asker must confirm too: otherwise a relay playing the giver
  (or listing a "Desktop" of its own) is kept with full control.
- Both, on yes: `{"t":"welcome","name"}` as a link frame (below), under
  the link's key from the asker's and giver's join nonces. A side keeps
  the brother only once its own human said yes and the other side's
  welcome opened; until then any other sealed frame is dropped. On no:
  plain `{"t":"refused"}`, and it closes.

A brother that joined before (or was introduced) links again. The ends
of the link are the asker's relay id and public key and the giver's, as
each side names them: its own, and the brother's as `brothers.json`
keeps it under the relay's id for it. The asker sends
`{"t":"hello","v":4,"kind":"brother","name","nonce"}` sealed with the
long term key and the additional data `"horadric link hello v2"`, `"a"`,
then, each led by its length as a 4 byte big endian number, the asker's
id, the asker's public key, the giver's id, the giver's public key and
an empty asker nonce. The giver answers with its own hello, a fresh
`nonce`, sealed with the same but `"g"` and the asker's nonce, so an old
answer does not open for a new hello. The asker then sends
`{"t":"ready"}` as the link's first frame, and the giver counts the link
up only once that opens: a hello played again gets an answer and nothing
more. A link key is one per pair and the same both ways, so without the
ends the relay could open a channel to a machine as if from its brother
and hand it its own hello: it would link to itself. Bound to the ends,
that hello does not open on the giving side (the ends come the other way
round), and a hello carrying a nonce the machine drew on any of its own
channels is not heard either. Of two joined brothers the one with the
lower id asks for the channel when it sees the other in `brothers`.

Up to 0.32 the hellos were `"v":3`, bound only to the way and the
asker's nonce (`"horadric link hello v1"`, the way byte, the nonce).
Nothing links on those any more. An asker on 0.33 sends that hello
first, then the bound one: a 0.32 giver can open only the first, and its
answer (which a 0.33 giver never sends) tells the asker the brother needs
a newer Horadric. A 0.33 giver that opens a 0.32 hello and no bound one
after it says the same. Either way the channel stays open and unlinked,
so it is not asked again and again, and the brother shows as "needs a
newer Horadric" in the tray's Brothers menu and in `horadric node
brothers`, until it updates.

Link frames, after the hellos, are sealed with the link's own key,
SHA-256 of `"horadric link session v2"` over the long term key, the
asker's id and public key, the giver's id and public key, the asker's
nonce and the giver's, length led as above, so nothing from an earlier
link, or from a link between other ends, opens. After a join's welcomes
they are sealed with SHA-256 of `"horadric link session v1"` over the
link key the join agreed and the two join nonces: that key already
binds both public keys in asker and giver order. A frame is its number, 8 bytes big
endian, then the AES-256-GCM payload as phone frames are, with the
additional data `"horadric link frame v1"`, the way (`"a"` from the
asker, `"g"` from the giver) and the number, 8 bytes big endian. Each
side numbers its frames from 0 up and refuses a frame from its own way
(one echoed back), one whose number is not above the last it opened
(one played again), or one that does not open. Brothers joined or
linked before this framing link again with no new join, since the long
term key is unchanged, once both run it; one joined when only the giver
confirmed the code should be joined again.

A refused token closes the PC's socket with 4401: the PC drops its
access token, refreshes it and tries again with the usual backoff. 4503
waits 30 seconds.

A Linux node joins the same way, through the same `/pc` with a secret
of its own (in its `portal.json`, owner only, with `enabled` false since
it pairs no phones) and its WorkOS access token, from AuthKit's device
code flow (`horadric node signin`), its refresh token in `workos.json`.
It runs the same handshake, link hellos and link frames, so a PC cannot
tell it from a PC. Over a link it is a brother both ways: it serves the
PC's asks, and as a client it says hello, `lore`, `takeover`, and its
own `bases`, `bundle` and `keep`, stamped as any client's. A phone that
opens a channel to a node is closed. Its human answers in a terminal
(`horadric node signin`, `horadric node let-in`); with none attached a
node refuses every join.

## Protocol v3: roster, state and lore

Sent to a client whose hello said `"kind":"brother"`, and to every linked
brother channel, which is served as a brother whatever its hello says. A
phone hears none of it, and its `lore` and `entries` are ignored.

- `{"t":"roster","n",...}` after the hello, then whenever it changes: the
  roster's own fields (`brothers`, `sessions`, `outcast`) beside `t`. A
  PC with no brothers sends none. A brother may act (`input`, `start` and
  the rest) when the roster gives it control; one the roster does not
  know only watches. See "The roster" below for what it holds and how two
  are merged.
- `{"t":"state","state":"awake"|"sleeping"|"leaving"}` after the hello, at
  once when it changes ("sleeping" as Windows suspends, "leaving" as it
  shuts down, "awake" after) and every 15 seconds as a heartbeat.
- `{"t":"lore","project","have"}` after the hello, one per tile project in
  git, `project` its repository (`{"remotes","roots"}`, as handing off
  matches one) and `have` the highest number held of each writing
  brother. A brother answers with its own `lore`; the PC then sends
  `{"t":"entries","project","b64"}` with what that `have` lacks, in as many
  messages as keep each under 384 KB of packed entries, and after that
  what is written as it is written. A project is the brother's when either
  repository names a remote of the other, or, with no remote, the same
  first commit. A server sends its `lore` again when its projects change;
  a client that hears it says its own `lore` again, and so does a client
  whose own projects change, so a project opened after the link came up
  is shared on that link (a server shares only what its client named).
- `entries` from a brother go into the machine's copy of the project's
  lore, which its agents read with full rights: so only from a brother
  that may control this machine (added in 0.33). A relay brother counts
  while this machine holds its key and its own "Can control this PC"
  switch for it is on; a PC reached through the phone door that it holds
  no key for never does; a node reached over `ssh` alone is the human's
  own by their ssh key and does. A client does not send `lore` to a
  brother whose entries it would not take, and drops any that come.
- A PC never serves on a session it only shows for a brother: not in
  `sessions`, not to `attach`, `watch`, `input` or `stop`.

## Protocol v3: keepers and takeover

Between linked brothers only; a phone's are ignored. Every ask carries
the connection's count as `n`, so the numbers here travel in other
fields.

- `{"t":"keep","id","part","of","b64"}`: a turn end's snapshot of session
  `id` from its owner to its first keeper (`keeping.json`'s `keepers`),
  split as `bundle` parts are. Part 0 also carries `holding`: the
  owner's name, its automatic takeover switch, and the session's record
  as a handoff sends it. Joined, `b64` is `horadric_core::keepers`'
  payload: a JSON line of the snapshot's meta (its number, the commit and
  the commit it is cut against, where the transcript's growth starts),
  the growth's length, the growth, then a `git bundle`.
- `{"t":"kept","id","snapshot","ok"}`: the keeper's answer to a whole
  snapshot. Not `ok`, the owner sends the next one whole.
- `{"t":"takeover","sessions":{"<id>":{"n","brother","since"}}}`: owners.
  Sent to every brother when a session is taken over, its number one
  higher, and whenever a link comes up, with the owners that are the
  sender's own. Only those are taken from it: a brother cannot hand a
  session to another this way. Since 0.33 the roster carries every owner
  too, and what `takeover` brings goes into the same roster, so the two
  never disagree. A brother whose session went elsewhere keeps it paused,
  its work on `horadric/orphaned/<id>`. Needs control, as `takeover`
  always did.

## Protocol v3: the roster

Added in 0.33. Every brother keeps `<data dir>/roster.json`: every roster
its brothers served it, merged, with every session's owner. What a
brother serves is that, without any entry for itself, merged with its
own entry put together afresh (its name, its key, `holds`, its keepers
and switch from `keeping.json`, the brothers it lets only watch and the
keys it cast out from `standing.json`) and an entry numbered `0` for
each brother in its `brothers.json`, so one whose own roster has not
come yet is still known by name.

```json
{ "t": "roster", "n": 1761000042,
  "brothers": [{ "n": 1761000042, "id": "<pc id>", "name": "Laptop", "key": "<b64url public key>",
                 "link": "relay", "view_only": ["<id>"], "keepers": ["<id>"], "auto_takeover": 600,
                 "cast_out": true, "holds": ["<id>", "<id>"] }],
  "sessions": { "<horadric id>": { "n": 2, "brother": "<pc id>", "since": 1761000000 } },
  "outcast": ["<b64url public key>"] }
```

- A brother is an entry per key it ever had: one cast out and let in
  again under its old id has two, the old one cast out. Where two rosters
  both have an entry of one id and key, the higher `n` wins, then the
  greater entry as a whole, so every brother picks the same. A session
  goes to the higher `n`. `outcast` is the union of both and never
  shrinks, and an entry whose key is in it is cast out. Merging is the
  same in any order and twice is once, so brothers that were apart agree
  again whichever rosters cross first.
- Nobody speaks for a brother but itself: a roster heard is taken without
  its entries for the hearer, whose own entry is only ever what it puts
  together itself.
- A brother's own entry gets a number past every one it has heard when
  it changes (kept in `roster-counted.json`), as a clock that only goes
  up, so its change wins wherever it is merged.
- `holds` lists the brothers it holds a link key for. It is what vouches
  for a brother another has not met (see below).
- `sessions` holds every owner: claimed with `n` 1 at a session's first
  snapshot, one higher with each handoff's `took` and each takeover. An
  older `keeping.json` that still holds `owners` has them moved here once.
  A brother hearing a roster in which a session it owned is another's,
  or its own under a higher number, keeps its copy paused, its work on
  `horadric/orphaned/<id>`, unless it is handing that session to that
  very brother, whose `took` then finishes the handoff.

## Protocol v3: introductions and casting out

Added in 0.33. Joining once joins everyone: the roster introduces.

- A brother B that holds a link key for X takes one for N when the roster
  it hears has N in good standing, N has no other key in good standing,
  B holds no key for N, and X's entry, under the very key B holds for X
  and in good standing, lists N in `holds`. So when A lets N in, A's
  roster brings N to every brother of A, and every brother of A to N.
- The key: SHA-256 of `"horadric introduced key v1"` over the ECDH
  secret of the two long term keys, the lower id's public key, the
  higher id's, the lower id and the higher id, each length led as the
  join's derivations are. Each side keeps the other in `brothers.json`
  as a brother joined, and the lower id links as usual: no code, since
  each public key came sealed from a brother the human vouched for, so
  the relay never had the chance to put its own in. A forged or replayed
  roster does not open (link frames), and one heard twice changes
  nothing.
- A hello that comes before its introduction (the two hear of each other
  in either order) is kept and heard again once the key comes.
- A brother away while N came hears it from whichever brother it links
  to first, since every roster carries every brother.
- A brother cast out vouches for nobody: its `holds` count for nothing.
  One it let in that others met meanwhile stays; cast that one out too.
- Casting out (the human, on any brother) writes the key into `outcast`.
  Every brother that hears it drops the brother's key into `cast`, closes
  its channels and its tiles go, first sending it
  `{"t":"cast_out"}` on any linked channel. A brother that opens
  `cast_out` from a brother it holds (or hears a roster with its own key
  in `outcast`) lets every brother go, its key pair and its roster with
  them, and tells its human.
- A brother cast out that relinks under its old key gets its hello
  answered, then `cast_out`, then the channel closes. One asking to join,
  under its old id or with a key in `outcast`, is refused unasked: no
  prompt for a stolen machine to wear the human down with. It comes back
  only when a brother's human asks it to join, with the new key it made;
  then its old id stands again everywhere the roster reaches, and its old
  key stays refused.

## Protocol v3: one machine, one brother

Added in 0.33. A Linux node joined by its SSH destination that signed in
too is one brother. Its hello answer over `--stdio` carries `"id"` (its
relay id) and `"key"` (its public key) once it holds a key pair. A PC that
holds that id under that very key shows the node as that brother: one
cluster, one tray line. The relay link is preferred; the `ssh` link waits
ready behind it and serves only while the relay link is down. Two
brothers that share a name are shown with the start of their keys.

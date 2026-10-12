# Town Portal: the protocol

Horadric from a phone, through a relay. Why it is built this way is in
[IDEAS.md](IDEAS.md), "Town Portal". This file is the contract between the
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

- `/pc?secret=<secret>`: a PC. The relay computes its id. A second PC
  connection with the same id replaces the first (a reconnect).
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

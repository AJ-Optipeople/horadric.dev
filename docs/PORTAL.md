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
{ "enabled": true, "relay": "wss://portal.horadric.dev", "secret": "<b64url, 32 random bytes>", "key": "<b64url, 32 random bytes>" }
```

`<data dir>` is `%APPDATA%\Horadric`, or `%APPDATA%\Horadric-dev` for a dev
instance. b64url is base64url without padding.

- The PC's **id** is the first 32 hex characters (lowercase) of the
  SHA-256 of the secret string's UTF-8 bytes. The relay computes it the
  same way, so it needs no storage to know which PC is which, and nobody
  without the secret can pose as the PC.
- The **pairing link** is
  `https://portal.horadric.dev/#r=<relay, URI-encoded>&id=<id>&k=<key>&n=<PC name, URI-encoded>`.
  The fragment never reaches a server. The page stores it in
  `localStorage` (a list, one entry per PC, keyed by id).
- `https://portal.horadric.dev/pair.html#<same fragment>` shows the link as
  a QR code, drawn in the browser, for the phone to scan.
- `horadric portal` turns it on (creating secret and key if missing),
  prints the link and opens the QR page. `horadric portal off` turns it
  off. `horadric portal rekey` makes a new key, which unpairs every phone.
  The UI looks at `portal.json` every 2 seconds and connects or
  disconnects to match.

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

Every payload is AES-256-GCM with the pairing key: 12 random bytes of
nonce, then the ciphertext with its 16 byte tag appended. The plaintext
is UTF-8 JSON. The PC drops any frame that does not decrypt, so the key
is also the proof a phone was paired.

## Messages

Inside the encryption. Every message has `"t"`.

Phone to PC:

- `{"t":"hello","v":1}`: first thing after `open`.
- `{"t":"watch","id":"<session id>"}`: stream this session's screen. One
  at a time per phone; a new watch replaces the old.
  `{"t":"unwatch"}` stops it.
- `{"t":"input","id":"<session id>","data":"<text>"}`: typed into the
  session as if at the keyboard. `\r` is Enter, `\u001b` is Escape,
  `\u001b[A` is Up, and so on.

PC to phone:

- `{"t":"hello","v":1,"pc":"<computer name>"}`: the answer to hello.
- `{"t":"sessions","list":[{"id","name","project","phase","line"}]}`: on
  hello and whenever the list or a phase changes. `phase` is `working`,
  `waiting` (needs the human: a permission, a question), `idle` or
  `ended`. `line` is what it waits for or last did, may be empty.
- `{"t":"screen","id","cols","rows","lines":["..."],"cursor":[row,col]}`:
  the watched session's text, up to 500 lines of scrollback first, then
  the visible screen, plain text without colour. On watch and whenever
  it changes, at most 4 times a second. `cursor` counts from the first
  line sent.
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
- `rekey` also clears `portal-push.json`.

Liveness:

- PC to relay, text `{"ping":1}` every 25 seconds; the relay answers
  text `{"pong":1}`. A PC that has heard nothing from the relay for 70
  seconds drops the socket and reconnects. The page does the same with
  its own text ping, and reconnects when it comes back to the
  foreground.

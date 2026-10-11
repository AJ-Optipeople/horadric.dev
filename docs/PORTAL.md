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

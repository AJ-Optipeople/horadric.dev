# The updater

The signing is built: `horadric release keygen` and `horadric release
sign`, the manifest in `horadric_core::release`, CNG in
`horadric_ui::update`. The signed bytes are `horadric release 1` and a
newline, then compact JSON of version, notes and files in that order,
rebuilt from the parsed fields, so the file's layout and key order do not
matter. The check is built: `horadric_ui::update::look` over WinHTTP
(`net.rs`), on the first tick after start, every 24 hours, and from "Check
for updates" in the tray. A verified newer version adds "Update to
<version>" and, once per release (`update_told` in the saved state), a
notification with the first paragraph of its notes
(`release::teaser`). The item and a click on the notification both show
all the notes, and install only on "Update now" (decided 2026-09-29:
the tray item alone told no one, and the signed notes were never shown).
A check that finds this build up to date takes the item away again; a
failed one leaves it. The install is built too:
`horadric_ui::update::download`, then the app's own `Reload`, as below.

Today a new build reaches this machine through
`reload`, from a checkout. The updater is for a machine with no checkout:
it fetches a release, checks it was signed by us, and hands it to the same
`reload`.

**What Purrch does.** `tauri-plugin-updater`: a `latest.json` manifest at
`github.com/<repo>/releases/latest/download/latest.json`, a minisign
(Ed25519) signature per installer, the public key in `tauri.conf.json`,
the private key in `%USERPROFILE%\.purrch\updater.key` and a CI secret. A
release is a draft until a human publishes it, and publishing is the moment
every install is offered it. The app never installs on its own: it looks
when asked, says what it found, and installs on a click. `RELEASING.md`
there says why the key must be backed up: lose it and every install is
stranded.

**What we take.** The trust model, the manifest at `releases/latest`, the
draft that a human publishes, and install only on a click. Horadric runs
agents with permission checks off too, so an unattended self replace is the
same bigger ask. Not the plugin: it is Tauri.

**What already exists.** `reload` is most of an updater. Given a folder
holding a new `horadric.exe` and `horadricw.exe`, it saves, runs `swap`
from that folder, moves the installed binaries aside, copies, rewrites the
hooks, starts `app --reload`, and rolls back if the new build is not up in
20 seconds. The sessions run on in their hosts throughout. So an update is:
download into a folder, verify, then post the same `Reload` request with
`exe` pointing into that folder. Nothing about the handover changes.

**The crypto, without a new dependency.** Minisign is Ed25519, and Windows
CNG has no Ed25519 signatures (its Curve25519 is key exchange only), so
minisign means adding `ed25519-dalek` and what it pulls in. CNG does have
ECDSA P-256 and SHA-256 (`BCryptVerifySignature`, `BCryptHash`), and
WinHTTP does HTTPS with redirects (GitHub's release links redirect to its
storage host). Both are in the `windows` crate behind feature flags. The
recommendation is P-256 through CNG and WinHTTP: no new dependency, and
the signature format is ours, so we do not need the minisign tool either.
What we lose is compatibility with `minisign` and `tauri signer`, which
nothing here uses.

**The release.** Three assets on a GitHub release of `Mopra/horadric.dev`
(public): `horadric.exe`, `horadricw.exe`, and `latest.json`:

```json
{ "version": "0.2.0", "notes": "...",
  "files": [ { "name": "horadric.exe", "sha256": "..." },
             { "name": "horadricw.exe", "sha256": "..." } ],
  "signature": "<base64 P-256 signature over the rest, canonical bytes>" }
```

One signature over the manifest, and a hash per file, so both binaries are
covered by one check and the version is inside what is signed (no replaying
an old manifest to downgrade). The URLs are derived from the tag, not
trusted from the file.

- **`horadric release keygen`** writes the private key to
  `%USERPROFILE%\.horadric\updater.key` and prints the public half, which
  goes into the source as a constant. A human backs the key up; that is not
  something an agent can do.
- **`horadric release sign <dir>`** hashes the two binaries in a release
  build folder, writes `latest.json` and signs it. Pure apart from the file
  reads and CNG, so the manifest bytes and the check are tested.
- **Publishing** is `gh release create v<version> --draft` with the three
  files, then a human publishes it after trying it. Local, not CI, for now:
  shipping already happens from this machine, and a key in a CI secret is
  one more place to guard. The workspace version is the release version
  and has to go up each release. 0.2.0 is the first. `RELEASING.md` has
  the steps.

**The check.** On start and once a day, and from a tray item "Check for
updates", fetch `latest.json`, verify the signature against the built in
public key, and compare its version with ours. A newer one shows in the tray
("Update to 0.2.0") and nowhere louder; a failed check (offline, a bad
signature) is logged, and said only when the check came from the tray.

**The install.** A click on "Update to 0.2.0" downloads both binaries into
`%LOCALAPPDATA%\Horadric\updates\0.2.0\`, checks each hash against the
verified manifest, and only then posts `Reload` with `exe` set to the
downloaded `horadric.exe`. Nothing downloaded is ever run before its hash
matches. From there it is `reload`: rollback included, sessions untouched.
The hosts need nothing, since the host binary is a copy of `horadric.exe`
per build already.

As built: only `horadric.exe` and `horadricw.exe` are fetched, whatever
else the manifest lists. The real ones come from
`releases/download/v<version>/`, by tag, so a release published mid
download can not mix two versions; a manifest from anywhere else has its
binaries beside it. Both are held and hashed in memory, and the version
folder is cleared and written only when both match, so nothing under
`updates` was ever not what the signed manifest names. A failure is said
in a notification and changes nothing.

**A dev instance** checks against `HORADRIC_UPDATE_URL` when set and
nothing otherwise, and never installs: `swap` under `HORADRIC_DEV` restarts
from its own folder and copies nothing, which would just restart the
downloaded build in place. Testing the install means the fake install
folder the reload test used (own `APPDATA`, `LOCALAPPDATA`, home and port)
with a local HTTP server serving a signed test release, signed by a test
key given through `HORADRIC_UPDATE_KEY` that only a debug build honours.
A debug build that is not a dev instance also honours
`HORADRIC_UPDATE_URL`, since the fake install is not one; a release build
never does. A dev instance downloads and verifies but does not reload.

Tested that way: a 0.1.0 debug build in the fake install, a 0.2.0 debug
build signed by a key made in the fake home and served by `python -m
http.server`. "Update to 0.2.0" fetched both, reloaded, and the install
folder held the 0.2.0 hashes. With the new build killed as it started,
`reload.log` said "rolled back" and the 0.1.0 build ran again. With one
served byte changed, both were fetched, nothing was written and nothing
reloaded. No session hosts, no `claude.exe`. The fake install's first
start pointed the real "Start with Windows" value at the fake folder
(it is in `HKCU`, shared with the installed Horadric), and it had to be
put back by hand. Now only a binary in `Programs\Horadric` under the real
`%LOCALAPPDATA%` (the known folder, not the variable a fake install sets)
turns it on at first start. A fake install test should still check that
value after.

**Not covered.** Authenticode. Downloads of the updater's own binaries do
not go through SmartScreen, so it is not needed to update; it is needed only
if people download Horadric by hand, the same point as NSIS above.

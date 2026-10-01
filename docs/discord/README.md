# Rich Presence art

What Horadric's Discord activity shows. All four are 512 by 512 PNG,
drawn by the app's own code (`crates/horadric-ui/src/icon.rs` for the
cube, `crates/horadric-ui/src/art.rs` for the lamps) and written here by

    cargo run -p horadric-ui --example discord_art

Run it again whenever the icon or the lamps change, and commit what it
writes.

## The keys

Discord takes an `https` URL for both `large_image` and `small_image`.
Checked on 2026-10-01 against the desktop client: a `SET_ACTIVITY` with a
URL in each came back with both turned into `mp:external/...` proxy keys.
So nothing is uploaded to the Discord application, and the key for each
image is its raw URL on `main`:

| Image | Shows | Key |
| --- | --- | --- |
| `horadric.png` | large, Horadric's own mark | `https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/horadric.png` |
| `waits.png` | small, a session waits for you | `https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/waits.png` |
| `working.png` | small, sessions working, none waits | `https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/working.png` |
| `idle.png` | small, nothing working or waiting | `https://raw.githubusercontent.com/Mopra/horadric.dev/main/docs/discord/idle.png` |

The URLs only answer once these files are on `main` on GitHub. Discord
caches what its proxy fetched, so a changed image may take a while to
show; a new file name shows at once.

## If URLs stop working

Discord could go back to keys for uploaded assets only. Then, in the
Discord developer portal:

1. Open the "Horadric" application (id `1555242626897416212`).
2. Rich Presence, then Art Assets, then Add Image(s).
3. Upload the four files above. The asset's name is the file name without
   `.png`: `horadric`, `waits`, `working` and `idle`. Those names become
   the keys.
4. Save. New assets can take some minutes before Discord shows them.

The code then sends the names instead of the URLs.

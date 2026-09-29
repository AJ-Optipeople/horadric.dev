# Releasing Horadric

Two signatures could matter, and only one is set up:

1. **The updater signature** (ECDSA P-256 through Windows CNG). Set up.
   Without the matching private key you cannot ship an update that existing
   installs will accept.
2. **Authenticode code signing** (a real certificate). **Not set up**, and
   not needed yet. The updater downloads its binaries itself, so they never
   meet SmartScreen. It becomes needed the day people download Horadric by
   hand, together with an installer.

## 1. The updater key: set up, back it up

`horadric release keygen` made the key pair once:

```
%USERPROFILE%\.horadric\updater.key    <- private. back this up.
```

The public half is built into the source as `PUBLIC_KEY` in
`crates/horadric-core/src/release.rs`. It has no password. `keygen` refuses
to overwrite an existing key, on purpose.

**Back the private key up somewhere you will still have in two years.** If
it is lost, every installed Horadric carrying that public key can never be
updated again. It refuses anything signed by another key, and the only way
left is installing a new build by hand on every machine. The public key is
already in shipped binaries, so it cannot be changed later.

The key never goes into the repository, a CI secret or a release. Signing
happens on this machine.

## 2. Cutting a release

1. Bump `version` under `[workspace.package]` in `Cargo.toml`. That is the
   release's version, and every install compares its own against it, so
   it must go up each time or no one is offered the release.
2. Run the three checks, as before any commit:

   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

3. Build and sign:

   ```sh
   cargo build --release
   target/release/horadric.exe release sign target/release --notes "What changed"
   ```

   This hashes `horadric.exe` and `horadricw.exe` and writes
   `target/release/latest.json`: the version, the notes, a SHA-256 per
   binary and one signature over all of it. The notes are what a user
   reads when deciding whether to update, so write them for that user,
   not for a developer. The first paragraph goes in the notification,
   cut to about 120 characters, so make it the one line that matters.
   All of it shows in the dialog before the update. A blank line
   separates paragraphs; `--notes` takes one argument, so quote it.
4. Commit the version bump, tag it and push:

   ```sh
   git tag v0.2.0
   git push origin main v0.2.0
   ```

5. Create a **draft** release with the three files, and the same notes
   as the manifest, so the release page and the app say the same thing:

   ```sh
   gh release create v0.2.0 --draft --title "Horadric 0.2.0" --notes "What changed" \
     target/release/horadric.exe target/release/horadricw.exe target/release/latest.json
   ```

6. Try the build before publishing. Download the two binaries from the
   draft, or use the ones in `target/release`, and run them as a dev
   instance (see CLAUDE.md). Tests prove it compiles and signs; they do
   not prove the tiles appear.
7. Publish the draft (below).

## 3. Publishing is shipping

Every install checks
`https://github.com/Mopra/horadric.dev/releases/latest/download/latest.json`
on start, once a day, and from "Check for updates" in the tray. A draft is
not `latest`, so it is invisible to them. That is the point of the draft.

**Publishing the draft is the moment every install is offered the
release.** Each one shows a notification, "Horadric 0.2.0 is out", with
the first paragraph of the notes, once per release (the state remembers
it was told). A click on it, or on "Update to 0.2.0" in the tray, shows
all the notes with "Update now" and "Not now". Nothing installs on its
own: "Update now" downloads both binaries into
`%LOCALAPPDATA%\Horadric\updates\<version>\`, checks each hash against the
signed manifest, and only then hands over through `reload`, the same way
`horadric reload` does from a checkout. Sessions keep running in their
hosts, and a build that does not come up in 20 seconds is rolled back by
itself. What happened is in `%APPDATA%\Horadric\reload.log`.

A bad release cannot be taken back from installs that already took it.
The fix is a new release with a higher version. Deleting or turning a
published release back into a draft only stops installs that have not
updated yet.

## 4. This machine

The Horadric on this machine ships from the checkout with `horadric
reload`, as CLAUDE.md says, and does not need the release. After a
release it is at the same version, so its tray offers nothing. If it is
behind, its tray offers the published release like any other install.

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

This is what "ship public" means (see CLAUDE.md). Plain "ship" skips it
and only reloads this machine.

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

5. Zip the two binaries for winget. A winget installer entry is one URL,
   and `horadric install` needs `horadricw.exe` next to `horadric.exe`, so
   winget installs both from one archive. The updater ignores the zip.

   ```powershell
   Compress-Archive -Force -DestinationPath target/release/horadric-x64.zip `
     -Path target/release/horadric.exe, target/release/horadricw.exe
   ```

6. Create a **draft** release with the four files, and the same notes
   as the manifest, so the release page and the app say the same thing:

   ```sh
   gh release create v0.2.0 --draft --title "Horadric 0.2.0" --notes "What changed" \
     target/release/horadric.exe target/release/horadricw.exe target/release/latest.json \
     target/release/horadric-x64.zip
   ```

   Once it is published, the winget manifest (`Mopra.Horadric` in
   microsoft/winget-pkgs) needs the new version, URL and the zip's
   SHA-256: `(Get-FileHash target/release/horadric-x64.zip).Hash`.

7. Sign the Mac build. CI builds `Horadric-macos.tar.gz` (the universal
   `Horadric.app`, packed with tar) and attaches it to the draft. The
   key stays here, so the Mac manifest is signed here too: download the
   archive from the draft, sign it with the same build as above, and
   upload the manifest to the draft.

   ```sh
   gh release download v0.2.0 --pattern Horadric-macos.tar.gz --dir target/mac
   target/release/horadric.exe release sign-mac target/mac/Horadric-macos.tar.gz --notes "What changed"
   gh release upload v0.2.0 target/mac/latest-macos.json
   ```

   This writes `latest-macos.json` beside the archive: the same format,
   version and key as `latest.json`, with one file, the archive and its
   SHA-256. A Mac checks
   `https://github.com/Mopra/horadric.dev/releases/latest/download/latest-macos.json`,
   downloads the archive into
   `~/Library/Caches/Horadric/updates/<version>/`, checks its hash
   against the signed manifest before unpacking it, and hands over to
   the new `Horadric.app` the way `reload` does. A release published
   without `latest-macos.json` offers Macs nothing; one with a manifest
   but no archive offers an update that fails to download, so upload
   both before publishing.
8. Try the build before publishing. Download the two binaries from the
   draft, or use the ones in `target/release`, and run them as a dev
   instance (see CLAUDE.md). Tests prove it compiles and signs; they do
   not prove the tiles appear.
9. Publish the draft (below).
10. Bump the Chocolatey package (below).
11. Deploy the Town Portal page (below).

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

## 4. Chocolatey

Horadric is on Chocolatey as
[`horadric`](https://community.chocolatey.org/packages/horadric). The
package lives in the socialsandbox repo, in
`products/horadric/chocolatey/horadric`, and downloads
`horadric-x64.zip` from the release. After each published release:

1. In `horadric.nuspec`, set `version` and the tag in `releaseNotes`.
2. In `tools/chocolateyInstall.ps1`, set `url64bit` to the new tag and
   `checksum64` to the zip's SHA-256:

   ```powershell
   gh release download v0.2.0 --repo Mopra/horadric.dev --pattern horadric-x64.zip --dir $env:TEMP\choco
   (Get-FileHash $env:TEMP\choco\horadric-x64.zip -Algorithm SHA256).Hash
   ```

3. From the package folder, pack and push:

   ```powershell
   choco pack
   choco push horadric.0.2.0.nupkg --source https://push.chocolatey.org/
   ```

Every version is moderated, so it shows on Chocolatey hours or days
after the release. Installs from Chocolatey update themselves like any
other, which is why the README says the app can run ahead of the
Chocolatey version. A skipped bump costs nothing but a staler first
install.

## 5. This machine

The Horadric on this machine ships from the checkout with `horadric
reload`, as CLAUDE.md says, and does not need the release. After a
release it is at the same version, so its tray offers nothing. If it is
behind, its tray offers the published release like any other install.

## 6. The Town Portal page

The phone page at `portal.horadric.dev` lives in `../horadric.portal` and
ships on its own, so a PC feature for phones (colours, the inbox, push
buttons) does nothing until the page is deployed too. Every ship, local
or public, ends by checking it:

```sh
cd ../horadric.portal
git log --oneline origin/main..main
curl -s https://portal.horadric.dev/sw.js | grep "CACHE ="
grep "CACHE =" public/sw.js
```

If there are unpushed commits or the live `CACHE` differs from the repo's,
run `npm test`, then `sh deploy.sh`, then `git push`. Saying "ship" is the
go ahead for this deploy, since the page only adds what the PC sends and
still works against an older PC.

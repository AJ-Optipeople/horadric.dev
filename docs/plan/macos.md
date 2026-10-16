# macOS

Asked for on 2026-10-07: Horadric on a Mac, downloadable by real users
from GitHub and horadric.dev, tested so we know it works.

**The shape.** The settled rule holds on the Mac too: native, no toolkit,
no web view for Horadric's own UI. Win32 becomes AppKit through the
`objc2` family of crates (`objc2`, `objc2-foundation`, `objc2-app-kit`,
`objc2-core-foundation`, `objc2-core-graphics`, `objc2-core-text`,
`block2`), the macOS counterpart of the `windows` crate: generated
bindings and nothing in between. ConPTY becomes a POSIX pseudo terminal
through `libc` (`forkpty`, `ioctl`, `waitpid`), already in the tree under
`alacritty_terminal`. Those two are this port's dependency decisions.

The Windows renderer calls Direct2D on every line, so the Mac gets its
own front end in `horadric-ui/src/mac/`, drawn with Core Graphics and
Core Text. It shares everything that is not drawing or windows:
`horadric-core`, `horadric-hooks`, and the pure halves of `horadric-ui`
(`layout`, `columns`, `theme`, `palette`, `anim`, `motion`, `keys`,
`viewport`, `frame`). Refactoring the Windows renderer behind a trait was
weighed and turned down: 47k lines that work and are verified on screen
would all move for no gain on Windows.

**What carries over, mapped.**

| Windows | Mac |
|---|---|
| Cluster window, `WS_EX_NOACTIVATE` | Borderless `NSPanel`, non activating, not in the Dock or cmd-tab |
| The stage | One `NSWindow`, the app's main window, in the Dock and cmd-tab |
| Tray icon and menu | `NSStatusItem` in the menu bar |
| ConPTY in `horadric-host-*.exe`, named pipe | `forkpty` in `horadric host`, a Unix socket in `/tmp/horadric-<uid>/` |
| Messages to a hidden window | `dispatch_async_f` on the main queue |
| `%APPDATA%\Horadric` | `~/Library/Application Support/Horadric` |
| Start with Windows | A LaunchAgent in `~/Library/LaunchAgents` |
| Segoe Fluent Icons | SF Symbols |
| Cascadia Mono | SF Mono, then Menlo |
| Ctrl shortcuts | Cmd shortcuts; Ctrl goes to the terminal |

Socket paths on a Mac are capped at 104 bytes, which a path under
Application Support with a long session id overruns, so hosts listen
under `/tmp`. A reboot clears it, and takes the hosts with it anyway.

An app started from Finder or a LaunchAgent gets launchd's bare `PATH`,
not the shell's, and would never find `claude` in `~/.local/bin` or a
Homebrew prefix. The app asks the login shell once at start
(`$SHELL -l -c 'printf %s "$PATH"'`) and uses that.

**The first Mac release.** The core loop, whole: tiles grouped by
project down the left of the screen with phases from hooks, the stage
with each session a pane and a real terminal in it, sessions in hosts
that outlive the app and are attached again on start, resume after a
quit, plain terminals, the menu bar menu, `horadric install`, the
updater. Left for later, each its own step: the browser pane (a
`WKWebView`), the quest log runner and Warriv, the files tile and file
viewer, the usage window and accounts, Discord, the Settings window.
The Mac release says so in its notes and on the site, rather than
showing features that do nothing.

**Distribution.** There is no Apple Developer ID, so nothing is
notarized. CI builds a universal `Horadric.app` (arm64 and x86_64 with
`lipo`), signs it ad hoc, and packs `Horadric-macos.tar.gz`. The way in
is `curl -fsSL https://horadric.dev/install.sh | sh`: curl does not set
the quarantine flag, so Gatekeeper never asks. A browser download does,
and the site and README say the one `xattr` line that clears it. The
updater on a Mac reads `latest-macos.json`, signed with the same updater
key on the Windows machine as `latest.json` is, so the private key still
never meets CI. The signature is checked with the Security framework.
Its `RFC4754` algorithms take the raw `r||s` CNG writes but need macOS
14, and a symbol missing at load would stop the app on 11 to 13, so the
updater turns `r||s` into DER and uses the X9.62 one, there since 10.12.

**Built, 2026-10-07, released in 0.17.0.** Everything above the line
"Left for later" is in. What it took, where it is not obvious from the
code:

- The console (`horadric-ui/src/console.rs`) is shared: it tells a
  `Notify`, a window handle on Windows and the main queue on a Mac. The
  session host (`horadric-pty/src/host.rs`) is shared too, over a `Pty`
  and a `Pipe` per platform (`posix.rs`, `socket.rs`).
- The tiles are the Windows faceplate ported call for call
  (`mac/look.rs` on `mac/paint.rs`), stepped by the same `anim::Tiles`.
  The terminal grid is built by the same `frame::build` and drawn as
  Core Text glyph runs. Glyph positions are in text space, which the
  flipped text matrix turns too, so each y is given negated.
- Keys go through `mac_keys` (pure, tested); plain typing goes to
  AppKit's text input, so dead keys and input methods work. Option is
  never Meta: it types `@` on a Danish keyboard.
- A pty's line discipline wants Return as `\r`: a `\n` written to the
  master never ended the line in CI's shell.
- Everything that changes the app is an `Input`, queued, so a menu or an
  alert running its own loop never re-enters the app.
- Waiting sessions show as a count on the Dock icon and a bounce when
  you are elsewhere; Cmd+J goes to the one waiting longest.
  Ctrl+Option+Space is macOS's next input source, so it is not taken.
- The Dock's Quit and a logout quit at once and keep the sessions;
  only Cmd+Q asks. A question would hold a logout up.
- The updater verifies with the X9.62 algorithm and the signature
  turned to DER, since the RFC 4754 one needs macOS 14 and the app runs
  on 11. An update replaces the whole bundle, Info.plist and signature
  included, and is rolled back like on Windows.
- `.github/mac-smoke.sh` is the Mac's on screen check: it runs a dev
  instance, a zsh session through `horadric new`, types into it with
  System Events, kills the app to see the host outlive it, builds the
  bundle and installs through `install.sh`. The PNGs and each pane's
  text are the run's artifacts.

**Testing without a Mac on the desk.** The work is done on Windows, where
`cargo clippy --target aarch64-apple-darwin` checks the Mac code without
linking. A `macos` job in CI runs clippy and the tests on a real Mac,
including a test that runs `/bin/sh` in a real pseudo terminal through a
host. Then a smoke run: the app started as a dev instance, fake sessions
posted to its port, a session started with a shell as its agent, and
`HORADRIC_SNAPSHOT` makes the app draw each of its windows into a PNG
(no screen recording permission needed). The PNGs are uploaded as CI
artifacts and looked at, which is the Mac's "verified on screen".

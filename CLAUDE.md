# Working on Horadric

Horadric shows every coding agent session you have running as a tile on your
Windows desktop, grouped by project. Read [README.md](README.md) for what it
does and [docs/PLAN.md](docs/PLAN.md) for what comes next. Read the
plan's index before starting anything, then only the parts in
`docs/plan/` that your work touches. What was decided about anything
already built is in the lore, not the plan: ask `lore_recall`.

The original concept lives outside this repo at
`../ideas.repo/glance/glance-concept.md`. It holds the problem statement and
the reasoning. The plan supersedes it wherever the two disagree.

## The decisions that are settled

Do not reopen these without asking. They were argued through and chosen.

- **Pure Rust, no toolkit, no web view for Horadric's own UI.** Win32
  through the `windows` crate, Direct2D and DirectWrite for drawing.
  Performance is the reason and so is control: the window manager fight
  is the whole project, and a layer in between makes it unwinnable. The
  one web view is the browser pane, which shows web pages, not Horadric
  (decided 2026-09-29, the lore has why).
- **One window per project cluster, not per tile.** Tiles are drawn inside
  the cluster window. Forty tiles must not mean forty windows.
- **One terminal window for all sessions, the stage.** It shows one
  project at a time, each of its sessions a pane in a grid, and a tile
  click switches the project. No session gets a window of its own. Ten
  sessions must not mean ten terminals piled up.
- **State comes from hook events, never from parsing terminal output.**
  The one exception is OSC 7501, a status the program sends on purpose
  (decided 2026-10-11, the lore has why).
  Hooks win where both speak.
- **Dependencies are justified one at a time.** Today: `serde`, `serde_json`,
  `windows`, `windows-numerics`, `alacritty_terminal` for the terminal
  grid (writing a VT parser is not the project), `syntect` for the
  file viewer's colours (writing grammars is not either),
  `webview2-com` for the browser pane (the WebView2 COM bindings, on the
  same `windows` version), and `tree-sitter` with one grammar crate per
  language (Rust, TypeScript and JavaScript, Python) for the lore's code
  index (parsing languages is not the project either, and it finds the
  calls a regex outline misses), and in `horadric-memory` `candle-core`,
  `candle-nn` and `candle-transformers` with `tokenizers` (its `onig`
  feature only) to run the embedding model that finds memory by meaning:
  Hugging Face's pure Rust ML crates, no DLL to ship and no service that
  must be running, and the tokenizer is the one the models were trained
  with (writing either is not the project), and in `horadric-link`, for
  a Linux node on the relay, `ring`, `p256`, `rustls`, `webpki-roots`
  and `tungstenite`: the AES-GCM, ECDH, TLS and WebSocket that Windows
  has built in and Linux does not (writing crypto is not the project
  either). Windows keeps CNG and WinHTTP; the crate is used there only
  by a test that holds the two to the same bytes. `p256` has its `ecdsa`
  feature too, for the signatures a brother's word carries (decided
  2026-10-25), which brings `ecdsa`, `rfc6979`, `sha2` and `signature`.
  Not a crate but vendored: `conpty.dll` and `OpenConsole.exe` from
  Microsoft's ConPTY package, the pseudo console that passes `CSI c`
  through so OSC 7501 works (see
  `crates/horadric-pty/conpty/README.md`). Adding one is a decision, not a reflex.

## Conventions

- No em dashes, en dashes or double hyphens anywhere, including code comments
  and commit messages. Use a full stop, a comma, a colon or parentheses.
- Comments say why, not what. If a line needs a comment to say what it does,
  rewrite the line.
- Every pure function gets a test. Anything touching Win32 gets verified on
  screen instead, see below.
- `cargo fmt --all`, then `cargo clippy --workspace --all-targets -- -D
  warnings`, then `cargo test --workspace`. All three before every commit.
- The Rust version is pinned in `rust-toolchain.toml`, for this machine
  and CI alike. Moving it is a commit of its own, with whatever new
  clippy lints it brings fixed in the same commit.
- Commit messages: one line saying what changed, a blank line, then why.
- Work lands on `main`. This repository is in trunk mode: sessions share
  the main checkout and commit on `main` directly, small and often,
  staging only their own files by name. Other agents may be editing
  beside you, so never `git add -A`, `git stash` or `git checkout --` a
  file you did not change. A worktree only when the human asks for one.
- When you are in a worktree anyway (you were asked, or it is a tomb or
  a parallel task), finish by merging it into `main`: merge `main` into
  the branch first if it has moved, run the three checks again, then
  `git -C <main checkout> merge --ff-only <branch>`. No branch is left
  with work `main` lacks.

## Verifying Windows code

Tests cannot tell you a window looks right. The loop that works:

1. Always test as a dev instance: `HORADRIC_DEV=1`. It listens on 43118,
   keeps its state in `%APPDATA%\Horadric-dev`, never turns on autostart, has
   a red lit tray icon, and refuses `install`, `uninstall` and hook or Explorer
   changes. The installed Horadric keeps running beside it.
2. Stop only the dev build, never every `horadric.exe`. You may be running
   inside a terminal of the installed one, and killing it kills you.
   `Get-Process horadric | ? Path -like '*\target\*' | Stop-Process -Force`,
   then build. The installed copy lives in `%LOCALAPPDATA%\Programs\Horadric`,
   so it never locks `target`.
   A dev instance leaves the quest log of every project the installed
   Horadric knows (its recent list) to the installed one: its runner and
   Warriv skip them, so trying a build never takes a real quest and
   strands it when the dev instance stops. Set `HORADRIC_DEV_QUESTS=1`
   to test the runner itself, or use a folder only the dev instance knows.
3. Start the tiles with `target\debug\horadric.exe app` in the background
   (plain `horadric` starts it hidden and returns). Use `Start-Process
   -NoNewWindow`, never `-WindowStyle Hidden`: Windows turns the first
   window the app shows into a hidden one, and the stage never appears.
   Then `horadric new` or post
   fake sessions at port 43118 with a short Python script. `HORADRIC_AGENT=cmd.exe`
   puts a shell in the terminals instead of `claude`.
4. Never leave a test running unwatched that can start agents. A resume bug
   once started 167 `claude` processes in a minute. Count `claude.exe`
   children of the dev instance's `horadric-host-*.exe` processes after any
   change to how sessions start.
5. Each session runs in a host process, `horadric-host-<build>.exe` in
   `%LOCALAPPDATA%\Horadric-dev\hosts`, which outlives the dev UI on
   purpose. When a test is done, stop those too: `Get-Process
   horadric-host* | ? Path -like '*\Horadric-dev\hosts\*' | Stop-Process
   -Force`. The installed Horadric's hosts are in `Horadric\hosts`; leave
   them alone, one of them runs you.
6. Screenshot the top right corner with PowerShell and `CopyFromScreen`, then
   read the image. For a window that is behind another, `PrintWindow` with
   flag 2 captures it anyway.
7. `HORADRIC_DEBUG=1` makes the app log cluster positions, sizes and paints.

Two bugs found this way that tests would never have caught: a window born
with its final layout never resized past 10 pixels, and a window created off
screen never painted after being moved into view.

## Testing against a real agent

`horadric run --name x` in a project starts a tagged `claude`. For a
non-interactive check, add `-- -p "Reply with pong" --model
claude-haiku-4-5-20251001`. The hooks are in `~/.claude/settings.json` and a
backup of the pre-Horadric file sits beside it. A `claude` started by a dev
instance still posts to the installed Horadric, which passes the event on to
the dev one by the `X-Horadric-Port` header.

## Developing Horadric from inside Horadric

The agent working on Horadric runs in a terminal of the installed Horadric. It
builds and tests dev instances as above and never touches the installed
one, with one exception: shipping, and only when the human says to ship.
There are two kinds, and the human's words pick one.

- **"Ship" or "ship local"** updates only this machine. Nothing goes to
  GitHub.
- **"Ship public"** cuts a release every install is offered: bump the
  version, sign, tag, push, draft the GitHub release with notes, try the
  draft's build as a dev instance, publish it, then ship local. The steps
  are in [RELEASING.md](RELEASING.md). Saying "ship public" is the
  human's go ahead to publish, which is the moment every install sees
  it, so the agent does not stop at the draft to ask again. It picks the
  version (patch for fixes, minor for new features), writes the notes
  for users from the commits since the last tag, and says both before
  publishing.

Two things besides the human's words are a go ahead. The first is the
"Warriv drives" switch. Flipping it on is the human's go ahead for
Warriv to ship local by itself: a round casts "Ship Local" when quests landed and the checks
passed on `main`. "And ships public", the second switch, is the go
ahead for public releases too, cast when the landed work is worth one
to users. A build that rolled back, or checks red on two landings in a
row, holds shipping ("shipping held" on the switch) until the fix-up
quest Horadric files lands on green.

The second is an errand. A stone with `"every"` runs only once the
human has armed it in the Runetome, after reading its steps and its
schedule. Arming an errand that publishes (a push, a release, a post,
"ship public") is the human's standing go ahead for it: an errand session
told to ship public does so without the human saying it that day. A
stone whose steps or mode change is disarmed until the human arms it
again, so that go ahead covers only what they read.

Shipping local is merging into `main` as above. Then the three checks run on
that merged code, once, each through `horadric heavy`: `horadric heavy -- cargo fmt
--all --check`, `horadric heavy -- cargo clippy --workspace --all-targets --
-D warnings`, and `horadric heavy -- cargo test --workspace`. A red check stops
the ship. Do not build or reload. Report the failing output (in the quest's
blocked line or the final message) so the human sees why. Checks that
already passed on this exact commit (a ship public that just ran them,
or a merge you just checked) are not run again. Only when all three
are green, run `cargo build --release` from the merged code, then
`target\release\horadric.exe reload`. Before `reload`, deploy the Town Portal page if it is behind
(RELEASING.md, "The Town Portal page"), for ship local and ship public
alike. The installed Horadric hands over to the new build at once, and
the new build attaches to every session's host, including the agent's
own, which keeps running through it. An installed Horadric from before
session hosts waits until no session is mid turn and resumes them
instead. Either way the agent says what it shipped before it runs
`reload`, ends its turn, and does nothing after. A build that does not
come up is rolled back to the old binaries by itself. What happened is in
`%APPDATA%\Horadric\reload.log`.

`reload` with `HORADRIC_DEV=1` restarts the dev instance the same way, from
its own build, which is how to test a change to reloading itself.

The first time, and whenever the installed Horadric is older than `reload`,
it has to be installed by hand from a terminal outside Horadric: Quit from
the tray (sessions pause), `cargo build --release`,
`target\release\horadric.exe install`, then click each tile to resume.

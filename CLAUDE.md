# Working on Horadric

Horadric shows every coding agent session you have running as a tile on your
Windows desktop, grouped by project. Read [README.md](README.md) for what it
does and [docs/PLAN.md](docs/PLAN.md) for where the work is up to and what
comes next. Read the plan before starting anything.

The original concept lives outside this repo at
`../ideas.repo/glance/glance-concept.md`. It holds the problem statement and
the reasoning. The plan supersedes it wherever the two disagree.

## The decisions that are settled

Do not reopen these without asking. They were argued through and chosen.

- **Pure Rust, no toolkit, no web view.** Win32 through the `windows` crate,
  Direct2D and DirectWrite for drawing. Performance is the reason and so is
  control: the window manager fight is the whole project, and a layer in
  between makes it unwinnable.
- **One window per project cluster, not per tile.** Tiles are drawn inside
  the cluster window. Forty tiles must not mean forty windows.
- **One terminal window for all sessions, the stage.** It shows one
  project at a time, each of its sessions a pane in a grid, and a tile
  click switches the project. No session gets a window of its own. Ten
  sessions must not mean ten terminals piled up.
- **State comes from hook events, never from parsing terminal output.**
- **Dependencies are justified one at a time.** Today: `serde`, `serde_json`,
  `windows`, `windows-numerics`, `alacritty_terminal` for the terminal
  grid (writing a VT parser is not the project), and `syntect` for the
  file viewer's colours (writing grammars is not either). Adding one is a
  decision, not a reflex.

## Conventions

- No em dashes, en dashes or double hyphens anywhere, including code comments
  and commit messages. Use a full stop, a comma, a colon or parentheses.
- Comments say why, not what. If a line needs a comment to say what it does,
  rewrite the line.
- Every pure function gets a test. Anything touching Win32 gets verified on
  screen instead, see below.
- `cargo fmt --all`, then `cargo clippy --workspace --all-targets -- -D
  warnings`, then `cargo test --workspace`. All three before every commit.
- Commit messages: one line saying what changed, a blank line, then why.
- Work lands on `main`. When a piece of work is finished, and always
  before shipping, merge its branch into `main`. No branch is left with
  work `main` lacks. From a worktree: merge `main` into the branch first
  if it has moved, run the three checks again, then `git -C
  <main checkout> merge --ff-only <branch>`.

## Verifying Windows code

Tests cannot tell you a window looks right. The loop that works:

1. Always test as a dev instance: `HORADRIC_DEV=1`. It listens on 43118,
   keeps its state in `%APPDATA%\Horadric-dev`, never turns on autostart, has
   a red tray icon, and refuses `install`, `uninstall` and hook or Explorer
   changes. The installed Horadric keeps running beside it.
2. Stop only the dev build, never every `horadric.exe`. You may be running
   inside a terminal of the installed one, and killing it kills you.
   `Get-Process horadric | ? Path -like '*\target\*' | Stop-Process -Force`,
   then build. The installed copy lives in `%LOCALAPPDATA%\Programs\Horadric`,
   so it never locks `target`.
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

Shipping is merging into `main` as above, `cargo build --release` from
the merged code, then `target\release\horadric.exe
reload`. The installed Horadric hands over to the new build at once, and
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

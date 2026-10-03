# Horadric tasks

The order is the work order. Each item points at its section of
docs/PLAN.md, which has the reasoning; read it before starting.

## SSH hosts

- [x] SSH: read hosts from config.json and open an SSH terminal from the project menu @ssh-read-hosts-from-config-json-and-open-53027
  See "SSH hosts" in docs/PLAN.md. `"hosts": [...]` in `.horadric/config.json`, each anything `ssh` takes.
  "SSH to <host>" in the project menu, one entry a host, starts a plain terminal running the Windows `ssh <host>`.
  Own glyph, host as the second line until the remote sets a title. Exit 0 closes it; 255 keeps the pane with the error and pauses the tile, a click reconnects.
  Pure and tested: reading hosts, the command line, the exit code rule. Verify on screen against localhost or a real server.
- [x] SSH: tell every session in a project with hosts about them @ssh-tell-every-session-in-a-project-with-53408
  See "Telling the agent" in docs/PLAN.md. Through `--append-system-prompt` on start and resume, joined with the task list's prompt into one.
  Say to use `ssh <host> '<command>'` with `-o BatchMode=yes`. Check on screen which `ssh` the agent's Git Bash runs and whether it reaches the Windows ssh-agent; say which to use in the prompt if it matters.
  Verify with `horadric run -- -p "Run uptime on <host>" --model claude-haiku-4-5-20251001`.
- [x] SSH: "Add host" in the project menu with suggestions from ~/.ssh/config @ssh-add-host-in-the-project-menu-with-53612
  Asks with the app themed input (see the task list item), and offers the `Host` names in `~/.ssh/config` that have no wildcard (skip `Match` blocks). The parser is pure and tested.

## The task list

- [x] Replace the Win32 text dialog with an app themed input @replace-the-win32-text-dialog-with-an-app-53837
  `ask::text` in `crates/horadric-ui/src/ask.rs` is a stock Win32 dialog (system font, system chrome, OK and Cancel), and it looks nothing like the rest of Horadric. The tasks tile's plus uses it for a new task, and the tile menu uses it to rename a session.
  Draw our own in Direct2D and DirectWrite with the theme's colours and font: an input in place (in the tasks tile for a new item, on the tile for a rename) or a small borderless popup beside it. Enter confirms, Esc cancels, clicking outside cancels.
  A new task can take notes as well as a title, since the notes go to the agent. Build it once and use it everywhere `ask::text` is called, including "Add host" from the SSH items. Verify on screen in dark and light.
- [x] Show a project's tasks tile when it has a list but no session @show-a-project-s-tasks-tile-when-it-has-a-54571
  Today the tile only exists while the project has a cluster, so a list with nothing running is invisible and its runner does not run. See "The task list", Not done yet.
- [x] Let the runner wait for the five hour limit to reset and go on @let-the-runner-wait-for-the-five-hour-54875
  The usage window already knows the limit. When it runs out mid list, hold the runner and start the next item after the reset instead of stopping.

## Step 4: worktrees and the git glance

- [x] Resolve the project key to the parent repository @resolve-the-project-key-to-the-parent-55611
  The key is the working directory today, so every worktree would become its own cluster. Use `git rev-parse --git-common-dir`. Sessions outside a repo keep working. This comes before worktrees.
- [x] Give each session its own git worktree, optional per project @give-each-session-its-own-git-worktree-55753
  See "Step 4" in docs/PLAN.md. Branch named from the session name. `setup` commands in `.horadric/config.json` run in each new worktree (gitignored files do not come along), and a port range per worktree so dev servers do not collide.
  A project can turn it off and keep the shared working tree.
- [x] Show changed files with plus and minus counts per worktree @show-changed-files-with-plus-and-minus-56503
  `git diff --numstat`, uncommitted versus committed, plus a button that opens the worktree in VS Code.
- [x] Let the runner hold several items at once, one worktree each @let-the-runner-hold-several-items-at-once-57140
  `parallel` in `config.json`. The list then lives in the main working tree only. Keep the fuses: at most one start per project every 10 seconds, never resume a paused session.

## Before daily use

- [x] Find claude.cmd from an npm install, not only claude.exe @find-claude-cmd-from-an-npm-install-not-57557
  Needs `cmd.exe /c` and its own quoting rules. See "Not in any step yet" in docs/PLAN.md.
- [x] Propose how sessions can survive Horadric quitting or crashing @propose-how-sessions-can-survive-horadric-57680
  The consoles live in the Horadric process, so a crash ends every agent. Surviving it means the consoles in a separate process, which is a real decision.
  Do not build it. Write the options and a recommendation into docs/PLAN.md, then report blocked so the human decides.
- [x] Resume the running sessions after a crash @resume-the-running-sessions-after-a-crash-73061
  Option A in "Sessions that outlive Horadric" in docs/PLAN.md. Keep `running` in state.json all the time, and on a start after an unclean exit resume those sessions as `app --reload` does. Keep the fuses: once each, never a session already live.
- [x] Run each session's console in its own host process @run-each-session-s-console-in-its-own-73278
  Option B in "Sessions that outlive Horadric" in docs/PLAN.md, with every point listed there: named pipe per session, output ring and replay, versioned five message protocol, breakaway job, orphans, dev pipe names, and reload that no longer waits.
  Tray Quit asks whether to keep sessions running, "keep running" the default when any is mid turn. Verify on screen: kill the dev UI, start it again, every session is where it was. Count `claude.exe` processes after.
- [x] Hear commits when the repository root is above the project folder @hear-commits-when-the-repository-root-is-74668
  A session started in a subfolder of a repo does not see commits, since the index is outside what the files tile watches. See "The files tile", Not done yet.
- [x] Let the columns stand on a monitor other than the primary one @let-the-columns-stand-on-a-monitor-other-74815
  They use `SPI_GETWORKAREA` of the primary screen. Needs a way to pick the screen, kept in state.json.

## Terminal and viewer

- [x] Place the IME composition window at the terminal cursor @place-the-ime-composition-window-at-the-75048
- [x] Mouse reporting to programs in the terminal @mouse-reporting-to-programs-in-the-75357
- [x] Cursor blink and a configurable font family in the terminal @cursor-blink-and-a-configurable-font-75607
- [x] Diff gutter and search in the file viewer @diff-gutter-and-search-in-the-file-viewer-76121
  Mark the changed lines of a modified file in the gutter, and a search like the terminal's Ctrl+Shift+F. See "The file viewer", Not done yet.

## Step 5

- [x] Start with Windows from the tray, and a way to show collapsed terminals @start-with-windows-from-the-tray-and-a-76703
  Autostart must stay off for dev instances.
- [x] Signed updater, lifted from Purrch @signed-updater-lifted-from-purrch-76788
  See `../purrch.fun/src-tauri`. Plan it against the existing `reload` hand over before building.
- [x] Updater: sign and verify a release manifest with CNG P-256 @updater-sign-and-verify-a-release-76903
  See "The updater" in docs/PLAN.md. `latest.json` with version, notes, a SHA-256 per binary and one signature over canonical bytes. `BCryptHash` and `BCryptVerifySignature` through the `windows` crate, no new dependency.
  `horadric release keygen` and `horadric release sign <dir>`. Tests: manifest bytes, a good signature passes, a flipped byte or a changed version fails, an older version is not an update. Tests use a key made in the test, never the real one.
- [x] Updater: generate the real key and have the human back it up @updater-generate-the-real-key-and-have-77187
  Run `horadric release keygen`, put the public half into the source, commit. Then report blocked: the human must back up `%USERPROFILE%\.horadric\updater.key` before any build with that public key ships, since losing it strands every install.
- [x] Updater: check for a newer release over WinHTTP and offer it in the tray @updater-check-for-a-newer-release-over-77448
  See "The check" in docs/PLAN.md. On start, daily, and a tray item "Check for updates". Verified manifest, newer version, then a tray item "Update to <version>". Failures logged, said only when the check came from the tray. Dev instances check `HORADRIC_UPDATE_URL` only.
- [x] Updater: download, verify the hashes, and hand over through reload @updater-download-verify-the-hashes-and-77909
  See "The install" in docs/PLAN.md. Into `%LOCALAPPDATA%\Horadric\updates\<version>\`, both hashes checked before anything runs, then the same `Reload` request `horadric reload` posts.
  Verify with the fake install folder from the reload test and a local server serving a release signed by a test key (`HORADRIC_UPDATE_KEY`, debug builds only). Check the rollback too, and count `claude.exe` after.
- [x] Updater: publish the first release and write RELEASING.md @updater-publish-the-first-release-and-78563
  Bump the workspace version, `cargo build --release`, `horadric release sign`, `gh release create v<version> --draft` with the three files. Write RELEASING.md like Purrch's: the key, cutting a release, publishing is shipping. Report blocked for the human to publish the draft.

## Ship

- [x] Commit everything to main and ship a new build @commit-everything-to-main-and-ship-a-new-79061
  Always the last item. Merge every finished branch and worktree into main, and commit anything left in the working tree that belongs there. Leave nothing half done on main: if an item's work is not finished, say so and report blocked instead of shipping.
  Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. All three must pass. Push main to origin.
  Then ship as CLAUDE.md says, under "Developing Horadric from inside Horadric": `cargo build --release`, say what is being shipped, run `horadric task done`, and run `target\release\horadric.exe reload` as the very last command. Do nothing after it. Afterwards, `%APPDATA%\Horadric\reload.log` says whether the new build came up.
- [x] Skip a UTF-8 BOM when reading tasks.md and config.json (PowerShell 5 writes one, and the first item and the mode are lost) @skip-a-utf-8-bom-when-reading-tasks-md-79250
- [x] Offer to merge a finished parallel item's branch into main @offer-to-merge-a-finished-parallel-item-s-79399
- [x] Check the tray Quit question on screen: Yes keeps the hosts running, No stops them, Enter picks keep only when a session is mid turn @check-the-tray-quit-question-on-screen-80560
- [x] Check "Tiles on screen" in the tray menu on screen: picking each screen moves the columns, and the stage stays unless covered @check-tiles-on-screen-in-the-tray-menu-on-80944
- [x] Check Show terminal in the tray menu on screen: close the stage with its cross, pick it, the same project comes back @check-show-terminal-in-the-tray-menu-on-81134
- [x] Keep a fake install (own APPDATA, LOCALAPPDATA and port) from pointing the real Start with Windows value at itself on its first start @keep-a-fake-install-own-appdata-81272
- [x] Merge release-0.2.0 into main before shipping: it already merges every finished branch (conflicts resolved) and bumps to 0.2.0 @merge-release-0-2-0-into-main-before-81393
- [x] Stay a while and listen: a catch-up of what happened while you were away (see "Stay a while and listen" in docs/PLAN.md) @stay-a-while-and-listen-a-catch-up-of-50523

## Animations

The small niceties that make the app feel alive, worked by one session in
order. Each one respects Windows' animation setting, costs nothing at rest,
and is written up under "The look", Motion, in docs/PLAN.md.

- [x] A finished turn lands with weight: the key bounces once and a loot beam rises from it
  "Loot drops" in docs/IDEAS.md. The beam is light rising off the key, fading as it goes, once per finished turn. It pairs with Identify: the lamp stays lit until read, the beam is the moment it lands. No sound yet.
- [x] A waiting tile grows more urgent the longer it waits: the breath quickens after 1, 5 and 15 minutes
  The breath's period shortens in steps, never faster than a calm pulse. Pure and tested.
- [x] A resumed tile wakes up: its key unlatches and rises, its lamp warms from dark glass to its colour
- [x] An ending session powers down: its lamp goes out like a CRT, its key sinks and fades, the tiles below close the gap
  A tile that leaves stays drawn as a ghost until it has gone, and the cluster keeps its height until then.
- [x] Subagents as sparks orbiting the working lamp, one per running subagent
  Count SubagentStart and SubagentStop per session (not saved). The state machine still ignores them for the phase.
- [x] The tool icon turns over when the tool changes
- [x] The activity trace scrolls smoothly instead of stepping
- [x] The context bar fills like liquid, with a shimmer past 75 %
- [x] A finished task strikes through left to right, then its row folds away
- [x] A task taken by a session sends a light from its row to the new tile
- [x] A cluster's accent glows faintly while any of its sessions works
- [x] A new cluster rises into place and fades in
- [x] While a cluster is dragged, the others in its column make room for it @horadric.dev-42672
- [x] Keep a dev instance from sweeping worktrees it did not make (a test session in a worktree of horadric.dev removed other agents' clean merged worktrees) @keep-a-dev-instance-from-sweeping-52869


## Leftovers from the plan

- [x] Scroll the file viewer sideways instead of wrapping long lines @scroll-the-file-viewer-sideways-instead-57720
  See "The file viewer" in docs/PLAN.md, "Not done yet". Shift+wheel and a horizontal wheel scroll; the diff gutter and search marks stay in place. Verify on screen.
- [x] Round the stage's panes themselves, not only the glass inside them @round-the-stage-s-panes-themselves-not-58381
  See "The look" in docs/PLAN.md, "Not done". Panes are square child windows today. Verify on screen in dark and light, and that nothing clips the terminal text.
- [x] Stay a while and listen: list commits made under a finished item @stay-a-while-and-listen-list-commits-made-58738
  See "Stay a while and listen" in docs/PLAN.md, "Not done yet". The journal does not hear them today.
- [x] Stay a while and listen: a mark of its own for a background session's lines, and scrolling past "and N more" @stay-a-while-and-listen-a-mark-of-its-own-59170
  Same section as above. Both small; the scroll works like the tasks tile's wheel.
- [x] Kitty keyboard protocol in the terminal @kitty-keyboard-protocol-in-the-terminal-59815
  See "Not in any step yet" in docs/PLAN.md. Progressive enhancement flags, at least disambiguate and report event types. Check what alacritty_terminal already parses before writing any of it. Pure encoding, tested.

## Ideas

Everything in docs/IDEAS.md not yet built, asked for by the human. For each one: write its design into docs/PLAN.md first (a section under Next, with the reasoning), change its entry in IDEAS.md to say it is planned, then build it. The rule in IDEAS.md holds: the theme earns its place by making something clearer or quicker.

- [x] Item rarity colours: the key's name shows how a session ended @item-rarity-colours-the-key-s-name-shows-60762
  "Item rarity colours" in docs/IDEAS.md. White changed nothing, blue changed files, yellow changed files and tests passed, gold landed on main, green for a batch. Must not read as a phase: phase is light, project is accent. Pure rules for the rarity, tested.
- [x] Loot drop sounds: a short sound with the beam, a higher chime when work lands on main @loot-drop-sounds-a-short-sound-with-the-61500
  "Loot drops" in docs/IDEAS.md. Our own sounds, never Blizzard's: synthesise them, no new dependency. Off by default or a tray toggle, silent under Windows' focus assist.
- [x] The stash: a three by three grid for sessions kept for later @the-stash-a-three-by-three-grid-for-62201
  "The stash" in docs/IDEAS.md. A stashed session is paused and out of the columns; a click brings it back as it was. Kept in state.json.
- [x] Tal Rasha's tombs: start one task in several worktrees at once and pick the winner @tal-rasha-s-tombs-start-one-task-in-63068
  "Tal Rasha's tombs" in docs/IDEAS.md. Builds on parallel items and worktrees in Step 4. Picking one merges or keeps its branch; the others fade out and their worktrees are removed. Keep the runner's fuses: never more than 8, one start per 10 seconds.
- [x] Transmute: the cube and its first recipes @transmute-the-cube-and-its-first-recipes-64088
  "Transmute" in docs/IDEAS.md. Drop tiles on a cube in the column or stage. Recipes: two finished sessions start a reviewer on both diffs; a session and main merges its branch; three idle sessions close with a one-line summary each. Recipes are small named actions, pure matching and tested.
- [x] The transmute animation: tiles swirl into the cube and something comes out @the-transmute-animation-tiles-swirl-into-65627
  "The transmute animation" in docs/IDEAS.md. Respects Windows' animation setting, costs nothing at rest, like the other motion in "The look".
- [x] Runewords: named sequences a session can be given, such as test, review, merge @runewords-named-sequences-a-session-can-66193
  "Runewords" in docs/IDEAS.md. Decide in the plan whether recipes and runewords are one system, and build on Transmute.
- [x] Experience: XP per merged commit and a level in the tray menu @experience-xp-per-merged-commit-and-a-67192
  "Experience" in docs/IDEAS.md. Counted from git, not kept by hand, so it survives a reinstall. Pure and tested.
- [x] The Cow Level: a hidden easter egg @the-cow-level-a-hidden-easter-egg-67499
  "The Cow Level" in docs/IDEAS.md. A hidden way in and a portal that opens. It must never start agents or get in the way of reading a tile.
- [x] Quests: rename Tasks to Quests, with the old names still read @quests-rename-tasks-to-quests-with-the-67925
  "Quests" in docs/IDEAS.md. The tile, the command (`horadric quest done`, with `task` still accepted) and the file (`.horadric/quests.md`, with `tasks.md` still read). Last of the ideas, since every item above runs from this list: make sure the list that is running keeps working through the rename.
- [x] Play the transmute animation when a batch closes @play-the-transmute-animation-when-a-batch-68131
- [x] Check a runeword with a real claude: test and review runes, the reviewer writing its file, and a merge that conflicts @check-a-runeword-with-a-real-claude-test-68350
- [x] Let a session read its review without a permission prompt: allow the reviews folder in the settings Horadric passes (Read for the session, Write for the reviewer) @let-a-session-read-its-review-without-a-69740
- [x] Keep state.json when it fails to parse: set the bad file aside and drop only what is bad, instead of losing every session and overwriting it @keep-state-json-when-it-fails-to-parse-69853
- [x] A told rune whose turn the human interrupts never ends: decide whether the runeword waits, re-tells, or stops (see Runewords in docs/PLAN.md) @a-told-rune-whose-turn-the-human-69943

## Codex and Grok Build

ChatGPT and xAI subscriptions used from Horadric like a Claude one. See "Codex and Grok Build beside Claude Code" under Next in docs/PLAN.md, which has the table of what each agent gives and the reasoning; read it before starting. Claude sessions must behave exactly as before at every step.

- [x] Spike: install the Codex CLI and answer the plan's open questions about Codex and Grok Build @spike-install-the-codex-cli-and-answer-72784
  Step 1 of the plan. Test with a dev instance. Answer: does a Codex command hook see `HORADRIC_SESSION` from the parent; does Codex read `auth.json` again while running and write it back on refresh; does `codex resume <id>` take a session started elsewhere; does a Grok `http` hook expand `$HORADRIC_SESSION` in its URL on Windows; which Grok event means waiting on you, and does `Stop` come at every turn's end.
  Also check what the installed Horadric receives from Grok today: Grok reads the hooks in `~/.claude/settings.json`, so every `grok` already posts to the Claude hook. Does it send the header?
  Never switch or copy the real logins in `~/.codex` or `~/.grok`: read them, do not write them. Write the answers into the plan's table and steps, and change the steps below if an answer calls for it. No code beyond throwaway scripts.
- [x] Add an Agent to the core model: Claude, Codex or Grok, saved on every session @add-an-agent-to-the-core-model-claude-73414
  The seam in "The shape". Missing reads as Claude, so old state loads. It answers the program, resume arguments and default flags per agent; the registry, phases, tiles and journal keep working on `HookEvent` and never learn the agent's name. Pure and tested. Nothing changes for Claude.
- [x] Sign in to Codex and Grok again, then finish the spike's live checks @sign-in-to-codex-and-grok-again-then-73593
  Needs the human: both logins here have expired. Run `codex login`, then `grok update` (0.2.22 is installed, 1.0.44 is out) and `grok login`. Then, with throwaway hooks and `-p` or `exec` turns: Codex's `Stop`, `PermissionRequest` and `Interrupt` on real turns; whether Grok 1.x still refuses an `http` hook to 127.0.0.1; which Grok event means waiting on you; whether `stop` comes at every turn's end; whether Grok reads a changed `auth.json` without a restart. Write the answers into the plan's table and step 1.
- [x] `horadric hook`: the command hook Codex and Grok both post through @horadric-hook-the-command-hook-codex-and-16517
  Step 2. Neither can post over HTTP: Codex has command hooks only and Grok refuses loopback URLs. The command reads the event on stdin and posts it with the tag from its environment and the agent's name, like `horadric status`. It is the one process per event Horadric spawns, so it starts fast, has a short timeout, fails open, and does nothing without `HORADRIC_SESSION`. Turns a Codex or Grok payload into a `HookEvent`. Pure and tested.
- [x] Codex: pass its hooks on the command line and read its events @codex-pass-its-hooks-on-the-command-line-16670
  Step 3. `-c hooks.<Event>=[...]` for each event plus `--dangerously-bypass-hook-trust` when Horadric starts `codex`, so `~/.codex` is never written and a `codex` outside Horadric runs no Horadric hook. `PermissionRequest` is waiting, `Stop` done, `Interrupt` idle, a `SessionEnd` after a turn with no `Stop` is a failed turn. Pure and tested.
- [x] Codex: start, resume and pick defaults for a Codex session @codex-start-resume-and-pick-defaults-for-16984
  Step 5 for Codex. `codex resume <id>` for a paused tile (it takes Codex Desktop sessions too), `-m` and `-c model_reasoning_effort=` for the defaults, History from `~/.codex/sessions`. Verify on screen with a real `codex`, and count its processes.
- [x] Grok: install its command hook and read its events @grok-install-its-command-hook-and-read-17542
  Step 4. `~/.grok/hooks/horadric.json`, a `command` hook calling `horadric hook`, added by `install` and removed by `uninstall`, never by a dev instance. Phases from the live checks above. If Grok 1.x accepts a loopback URL after all, the listener drops a Grok payload that arrives on the Claude path by its shape. Pure and tested.
- [x] Grok: start, resume and pick defaults for a Grok session @grok-start-resume-and-pick-defaults-for-a-17845
  Step 5 for Grok. The plus button and `horadric new --agent grok` start it when `grok` is found, `--resume <id>` carries a paused tile on, the model and effort defaults pass as `-m` and `--effort`, a tile shows a small mark for its agent, and History lists Grok's own conversations from `~/.grok/sessions`. Verify on screen with a real `grok`, and count its processes after any change to how sessions start.
- [x] Limits per provider in the usage window, Codex's from its transcripts @limits-per-provider-in-the-usage-window-18547
  Step 6. A named screen per provider in use (Claude, ChatGPT, Grok). Codex's limits from the newest `token_count` in the transcript its hooks point at, read on each `Stop`: `rate_limits.primary` and `secondary`, `used_percent` and `resets_at`. The settings follow the agent, each with its own models and efforts. Grok shows no limits until a source turns up. Verify on screen.
- [x] Switch ChatGPT and xAI accounts the way Claude accounts switch @switch-chatgpt-and-xai-accounts-the-way-19232
  Step 7, built on "Accounts" in docs/PLAN.md. Codex: the whole `auth.json` is the login, `account_id` the account, the email from the `id_token`; force the file store, and stop and resume its sessions, since a running Codex treats another account on disk as a permanent error. Grok: its `auth.json` entry is the login, and Grok reads it again by itself, so a switch is only the write. A switch touches one provider's sessions. Test with a fake `CODEX_HOME` and `GROK_HOME`, never the real logins.
- [x] Resume Claude sessions without resending the prompt they were started with @resume-claude-sessions-without-resending-20088
- [x] Keep Grok from showing an SSRF error for the Claude hook it borrows @keep-grok-from-showing-an-ssrf-error-for-20174
  Grok runs the `http` hooks in `~/.claude/settings.json` and prints "blocked by SSRF protection" in its TUI at every event. `[compat.claude] hooks = false` in Grok's config stops it, but that is the user's file; look for a flag or environment variable Horadric can pass when it starts `grok` instead.
- [x] Mouse-over tooltips @mouse-over-tooltips-42172
  Mouse-over for all items in Horadric, describing what they do.
- [x] Tooltips on a pane's header: stash, zoom, close, the browser bar and its tabs, through tip.rs once the browser tabs work in pane.rs is committed @tooltips-on-a-pane-s-header-stash-zoom-71623
- [x] Quest Log @quest-log-43924
  Quests my ikke auto-start når du venstre-klikke på dem i listen, man skal se quest loggen først for at få en ide om hvad questen går ud på inden man accepter den.
- [x] Quest Giver @quest-giver-43923
  Knap ved Tasks listen der selv giver bud på hvad der kunne laves af tasks for det aktuelle projekt.
- [x] Ship public: release everything on main to every install @ship-public-release-everything-on-main-to-20289
  Always the last item. Everything finished on main goes out, not only the Codex and Grok work. Follow RELEASING.md and "Ship public" in CLAUDE.md.

## Discord Activity

Rust was installed on this machine after Horadric started, so a session's shell may not find `cargo`: put `%USERPROFILE%\.cargo\bin` first on `PATH` (Git Bash: `export PATH="$HOME/.cargo/bin:$PATH"`). Commits stay local on `main`; this account cannot push, and the work goes up as one PR when the section is done.

Horadric on the human's Discord profile through Rich Presence. See "Discord Activity" under Next in docs/PLAN.md, which has the protocol, the rules and the reasoning; read it before starting. The first four run side by side in the shared tree, so commit small, stage only your own files, and keep to your own new files where you can. Only the tray quest runs a dev instance on 43118; the others test with `cargo test` or a small check of their own, never a second dev instance.

- [x] Discord: a client for the Rich Presence pipe @discord-pipe-56616
  The frame codec (opcode and length, little endian, then JSON), the handshake, `SET_ACTIVITY` and clearing, all pure and tested, in a module of their own. Then the client on its own thread: try `\.\pipe\discord-ipc-0` to `-9`, handshake and wait for `READY`, keep only the latest presence, retry every 30 s while Discord is closed and say nothing about it, at most one update every 4 s, never send an unchanged one, clear before closing.
  Its interface is a handle the app keeps with `set(Option<Activity>)` and a `stop` that clears; the `Activity` type is shared with the presence quest, so define it first, commit it at once, and say in its doc comment that the other quest builds on it.
  The client id is the constant `1555242626897416212` (the human's "Horadric" application), overridden by `HORADRIC_DISCORD_CLIENT_ID`. Discord runs on this machine: check it against the real client with a small example or test binary that sets and clears an activity.
- [x] Discord: what the presence says @discord-presence-56628
  The pure function from the sessions and the setting to an `Option<Activity>`, as "What it says" in the plan: the counts, the project only when names are allowed, the small image by the most urgent state, a start time that holds while any session works, None with nothing running. Tested for each. Wait for the client quest's `Activity` type to be on main (`git log`), or agree on it by reading its commit; do not make a second one.
- [x] Discord: the art for the presence @discord-art-56640
  First find out whether Rich Presence takes an `https` URL for `large_image` and `small_image` today; if it does, they live in the repository and the keys are their URLs. Make the large image (Horadric's own mark) and one small image per state (waits, working, idle) in the lamps' colours, 512 by 512 PNG, drawn the way the app draws them so they match. Put them in `docs/discord/` with a short note of the key for each, and the steps for the human to upload them if URLs do not work.
- [x] Discord: the "Show on Discord" setting in the tray @discord-tray-56652
  Off (the default), "Without project names", "With project names", checked as chosen, kept with the other app settings so a dev instance has its own. Nothing is sent yet: expose the choice where the wiring quest can read it and hear it change. Verify the menu on screen with a dev instance.
- [x] Discord: wire the presence into the app @discord-wire-57355
  Needs the four above on main. Keep a client while the setting is on, hand it the presence whenever the registry or the setting changes (it drops what did not change), and clear on Off, quit and reload as the plan says. Count that the UI thread never waits on the pipe. Check with a real Discord and a dev instance, the installed one left off.
- [x] Discord: check it on screen with a real Discord and write it into the README @discord-check-57759
  With a dev instance: each setting, a session working, waiting and done, the elapsed time holding across turns, names hidden and shown, quit and reload clearing it. Screenshot the profile. Add a short part to README.md and mark the plan section done.
- [x] Discord: keep the clock of a run of work through a reload @discord-keep-the-clock-of-a-run-of-work-58450
- [x] Lock Tiles @lock-tiles-71913
  Lav en lille padlock ikon på session Tilen, som forhindrer den i at scrolle når man klikker paa den.
- [x] Theme Switcher @theme-switcher-72599
  Branch out and create 5 disctint themes for the app, changable via the tray icon menu.
- [x] Padlock not working @padlock-not-working-75760
  Padlock on session usage tile doesnt work. Its supposed to lock the tile at the very top and allow the tiles under it to continue scrolling from its bottom. Currently it does nothing.
- [x] Performance pass two: the leftovers in the plan @performance-pass-two-the-leftovers-in-the-71878
  See Performance in docs/PLAN.md, 'Left for later'. Each pane paint makes a new layer and geometry for its glass (glyphs.rs); characters missing from the terminal font get a DrawText each; the pane caption repaints on every spinner frame of the title; each cluster paint clones its sessions twice. Measure the UI thread on a dev instance before and after, as the first pass did.
- [x] Keep the browser page through a reload of Horadric @keep-the-browser-page-through-a-reload-of-72055
  See Browser pane in docs/PLAN.md: the page outlives its pane but is not yet kept over a reload, so every ship drops open pages. Save each project's address (and history position if WebView2 allows) before the handover and open them again in the new build. Verify with reload under HORADRIC_DEV=1.
- [x] Give Grok sessions the browser tools @give-grok-sessions-the-browser-tools-72801
  Claude gets horadric mcp through --mcp-config and Codex through -c mcp_servers; Grok has none because it keeps MCP servers in its own config (see Browser pane in docs/PLAN.md). Find a per-session way in (flag, env var, a config it reads), or else an entry Horadric owns in Grok's config. Check with a real grok session that the browser_* tools reach its project's pane.

## The Runetome

Runewords as rune stones in a tile of their own. See "The Runetome" under Next in docs/PLAN.md, which has the reasoning; read it and "Runewords" under Done before starting. The two quests at the top build in parallel; the tile needs both on main.

- [x] Runetome engine: say, keys and run steps, stones that need no session, and the global file @runetome-engine-say-keys-and-run-steps-73961
  See "The Runetome" in docs/PLAN.md: Steps, Casting, Where stones live. In `horadric_core::runeword`: the object form `{"steps": [...]}` beside today's list form, `say`, `keys` (with `runeword::keys` parsing `"Esc"`, `"Ctrl+C"`, `"/clear{Enter}"`), `run` (with `"show": true`) and the three runes; `%APPDATA%\Horadric\runewords.json` read beside `.horadric/config.json`, both read again when they change; a stone that does not parse kept with its reason. Casting: `keys` written into the session's terminal and done at once, `run` by `cmd /c` in the project folder or the session's worktree, a non zero exit stopping it with a toast of its last line, and a runeword of only `run` steps cast on the project with no session, saved in `state.json` beside the sessions so it goes on through a reload. Leave the tile menu's Runeword submenu working until the tome replaces it. Pure and tested; check a `keys` and a `run` step on a dev instance with `cmd.exe` as the agent.
- [x] Rune stones and the Runesmith's pieces: the carved glyph, the generated name, the smith prompt and `horadric runeword list` @rune-stones-and-the-runesmith-s-pieces-73971
  See "The Runetome" in docs/PLAN.md: A stone, The empty stone. `runeword::name` (two to four of the 33 rune names from a hash of the label) and `runeword::carve` (glyph strokes from the same hash), pure and tested. A function in horadric-ui that draws one stone in Direct2D at a given size: a rough rounded slab lit from the top left like the cube, the glyph cut into it, a gold glow while it runs, cracked when it does not parse, and an empty stone. `runeword::smith_prompt` (tested), saying what a stone is, the step kinds, both files and their shape, and to ask the human what it should do and for which projects, then check with `horadric runeword list`. The `horadric runeword list` command printing every stone a project has, with its name, label, steps and any parse error. No tile yet: check the drawing on screen however is quickest, in dark and light.
- [x] The Runetome tile: stones in a project's cluster, cast by click or drag, and the empty stone starting a Runesmith @the-runetome-tile-stones-in-a-project-s-73981
  Needs the two quests above on main. If either is not there yet, mark this one blocked naming which, so it can be accepted again once it is. See "The Runetome" in docs/PLAN.md. A tile in each project's cluster beside the quest log, built in stones first, then the project's, then the global ones, then the empty stone. Tooltip (tip.rs) with the runeword name and the steps. A click casts on the focused stage session when it is this project's, else asks which session; a stone of only `run` steps casts at once; dragging a stone onto a tile or pane casts on that one. While running the stone glows with its step and a click offers Stop. The empty stone starts a session named "Runesmith" on the stage with the smith prompt, as the quest giver does. The changed since last cast mark on project stones. Drop the tile menu's Runeword submenu, keep its Stop. Check on screen with a dev instance: each step kind, a sessionless stone, a drag, and a real `claude` Runesmith (Haiku) making a stone that appears on the tile. Count `claude.exe` processes after. Then add the Runetome to README.md and mark the plan section built.
- [x] Point horadric runeword list at runeword::parse @point-horadric-runeword-list-at-runeword-77455
  Once the Runetome engine is on main, have crates/horadric/src/runeword.rs list stones through the engine's runeword::parse (and its global file path) instead of its own reading of the two forms, so the list and the tile always agree on what cracks.
- [x] Blocked quests resume by themselves once what blocked them is resolved @blocked-quests-resume-by-themselves-once-75845
  Today a [!] quest waits on the human forever, and the runner stops the whole list at it (Next::Stuck in tasks.rs). Let a blocked quest say what it waits on, in a form Horadric can check, and have the runner check it on each look. Kinds worth having: another quest done (by title, the common case, see the Runetome tile quest), a commit or branch on main, a file existing, a command exiting 0, a time. Something like horadric quest blocked "why" --on-quest "title" | --on-file path | --on-cmd "..." | --until time, kept on the item line or in its notes so the file stays the state. When the condition holds: if the holding session is alive, send it a go on message the way go_on does after a usage limit reset and mark the quest [/] again; if the session is gone, start the quest again (start_again). A blocked quest whose condition is not met yet should not stop the list: the runner skips past it to the next open quest, but only for machine checkable waits; a plain why with no condition still needs the human and still stops the list. Update the system prompt so agents use the conditional form when they can, and the board row to show what it waits on. Tests for the parsing, the condition check and the runner choice (pure parts in horadric-core). Check on screen with a dev instance: block one fake quest on another, finish the other, watch the first resume. Count claude.exe after, since this starts agents by itself.
- [x] Let a stone of only keys be cast on a session that is casting another runeword, so a permission prompt can be answered from the tome without stopping it @let-a-stone-of-only-keys-be-cast-on-a-77466

- [x] Privacy run @privacy-run-73133
  Branch out and spawn agents to check for privacy concerns in Horadric. Does anything transmit back to a server somewhere, can the app screenshot silently without the users conscent and send the screen back to a someone else etc.
- [/] Gate the browser pane's DevTools passthrough: allowlist CDP methods, ask before cookie and storage reads (docs/PRIVACY.md item 1) @gate-the-browser-pane-s-devtools-36954
- [ ] Show on the tile when an agent is driving the browser pane, even when its project is off stage (docs/PRIVACY.md item 2)
- [ ] Require a per user secret on the listener's command paths, reload, new, tasks, browser (docs/PRIVACY.md item 3)
- [ ] Drop the WebView2 remote debugging port and the devtools-port file (docs/PRIVACY.md item 4)
- [ ] Make session ids random instead of name plus seconds since midnight (docs/PRIVACY.md item 5)
- [ ] Bind the hook listener with SO_EXCLUSIVEADDRUSE and check whether port squatting works (docs/PRIVACY.md item 6)
- [ ] Add an off switch for the daily update check and say it in the README (docs/PRIVACY.md item 7)
- [ ] Refuse Origin headers on /horadric/hook too, and check host pipe owners before attaching (docs/PRIVACY.md items 8 and 9)
- [?] Quest log - Horadric @quest-log-horadric-33953
  Branch the release out to any amount of agents you need and  Build a quest log. My isssue: When i run a quest and it auto-disappears from the QUESTS window, i have no way of going back and checking what the session did or how it ended up, without aasking the main session again. The quest log could de designed as a new window which shows a tree of every quest this tile has ever undertaken, which quests lead to which other quests and where quests converged into main sessions again with a branching diagram. Clicking each quest opens its old quest log, and a ultra-short summary of what the quest achieved and the result of the session. Ship local afterwards, so i can test the app.

## Runetome feedback

From the human on 2026-10-01, after using the tome.

- [x] Remove a stone by right click on it @horadric.dev-78569
  Today there is no way to remove a runeword short of editing the JSON. A right click on a project or global stone offers "Remove <label>" (with a confirm), which deletes it from `.horadric/config.json` or `runewords.json`, keeping the rest of either file as it was. Built in stones cannot be removed, so the menu says so or leaves the item out.
- [x] Better built in stones that show what a stone can do @horadric.dev-78569
  "Test, merge", "Test, review, merge" and "Review, merge" are not useful. Replace them with a few that each show off a step kind and are worth a click on day one: keys, say chains, a run command.
- [x] Ask before casting, with "Do not ask again" @horadric.dev-78569
  A click on a stone asks whether to cast it, showing what it will do, with a "Do not ask again" check kept in state.json.
- [x] Review the runeword and Runetome implementation for other quality of life fixes @horadric.dev-78569
  Read the tile, casting and the engine end to end and fix what gets in the way. List what was found and done in the plan.
- [?] Fold the History menu into the Quest Log window @fold-the-history-menu-into-the-quest-log-36361
  History (project menu, tray, start window right click; see History in docs/PLAN.md) lists a project's newest Claude Code conversations and resumes one, quests or not. The Quest Log now does the same for quests. Put the conversations no quest holds (transcript::history, same skip rules) on the Quest Log's own timeline, in time order between the quests, as rows on the main line itself (they are the main sessions quests grow from and converge into), drawn with a dot of their own colour and a faint row background so they read apart from quests at a glance. A quest added by one of them branches from its dot. Their detail shows the conversation's title, when it was last touched, and Read the session and Carry it on. Keep the All conversations picker in the window. Asked for by the human on 2026-10-03. Then the project menu, tray and start window offer Quest log... for that project instead of their History submenus, and the History submenu code goes. Check on screen with a dev instance, including a project with no open cluster from the tray.
- [?] Keep a new quest's text when the input loses focus @keep-a-new-quest-s-text-when-the-input-37605
  The themed input from the quests tile's + (ask.rs, see The task list and Replace the Win32 text dialog in docs/PLAN.md) cancels on a click outside, so clicking away to look something up loses the title and notes typed so far. Asked for by the human on 2026-10-03. A click outside should no longer throw the text away: either keep the input open (only Esc and its own cancel close it), or keep the draft per project and fill it back in the next time + is clicked, cleared once the quest is added. Same for Edit quest. Check on screen with a dev instance: type, click another window, come back, the text is there.

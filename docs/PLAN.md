# Implementation plan

Where Horadric is, what comes next, and what was decided along the way. It
is the handover document: someone picking the project up cold should need
nothing else.

This page is the index. Each part of the plan is a file of its own in
[plan/](plan/), with its reasoning and decisions. Read this page, then open
only the parts your work touches. When a step lands or a decision changes,
update that part's file. A new part is a new file in `plan/` and a line
here under Next, moved to Done when it is built.

Ideas that are not planned yet, most of them from the Diablo name, are in
[IDEAS.md](IDEAS.md).

## Shape of the thing

Five crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `horadric-core` | Session, Phase, Registry, the state machine | any |
| `horadric-hooks` | The localhost listener, its client, the settings installer | any |
| `horadric-pty` | ConPTY pseudo consoles, the session host and its pipe protocol | Windows |
| `horadric-ui` | Cluster and terminal windows, layout, drawing | Windows |
| `horadric` | The command line, `horadricw` for Explorer, the wiring | Windows |

`horadric-core` and the pure halves of `horadric-ui` (`layout`, `columns`, `theme`,
`palette`, `keys`, `frame`, `files`, `viewer`, `highlight`, `shell`, `board`) have no
I/O and
are tested. `horadric-pty` has a
test that runs `cmd.exe` in a real pseudo console. Everything else is
verified on screen.

Threads: the listener thread owns the socket, a feeder thread applies events
to the registry, the UI thread owns every window and never blocks. Each
console adds two: a reader that parses what its session host sends into
the grid and notices the exit, and a writer that owns the pipe's sending
side so typing never blocks the UI. The host process has its own, see
"Sessions that outlive Horadric". Everything off the UI thread reaches it as a message to one
hidden message-only window, never a thread message: thread messages are
dropped while Windows runs a modal loop, and dragging a terminal's edge is
one. The registry is an `Arc<Mutex<Registry>>`, each console's grid an
`Arc<Console>` with the terminal behind a mutex.

## Done

- [Step 1: hook receiver and state machine](plan/step-1-hook-receiver-and-state-machine.md): Hooks post lifecycle events to the listener; the session state machine.
- [Step 2: cluster windows](plan/step-2-cluster-windows.md): One frameless Direct2D window per project and the Win32 rules that make it feel native.
- [Step 3: the terminal window](plan/step-3-the-terminal-window.md): A tile click opens a real ConPTY terminal running `claude`.
- [Launchers](plan/launchers.md): Starting sessions without a terminal: Explorer, folder picker, `horadric new`.
- [Persistence](plan/persistence.md): Sessions come back after a quit, crash or restart via `claude --resume`.
- [History](plan/history.md): Ended sessions kept and resumable.
- [Install](plan/install.md): `horadric install`, per user, no admin.
- [Reload](plan/reload.md): `horadric reload`: hand over to a new build without losing sessions, rollback.
- [Dev instances](plan/dev-instances.md): `HORADRIC_DEV=1`: port, state folder and limits of a test build.
- [The stage](plan/the-stage.md): One terminal window for all sessions, one project at a time.
- [The project grid](plan/the-project-grid.md): A project's sessions as panes in a grid on the stage.
- [The files tile](plan/the-files-tile.md): Project tree with git changes in each cluster.
- [The file viewer](plan/the-file-viewer.md): Read only file view on the stage with syntect colours.
- [Browser windows](plan/browser-windows.md): Placing agent-started browser windows (superseded by the browser pane).
- [Browser pane](plan/browser-pane.md): WebView2 pane per project, the `browser_*` MCP tools, tabs per session.
- [Plain terminals](plan/plain-terminals.md): Non-agent terminals beside the sessions.
- [The columns](plan/the-columns.md): How clusters are stacked into columns down the screen edge.
- [The usage window](plan/the-usage-window.md): Usage and limits per provider.
- [Accounts](plan/accounts.md): Several subscriptions logged in, switched while sessions run.
- [The look](plan/the-look.md): Visual identity: colour rules, type, motion, dark and light.
- [No Windows chrome](plan/no-windows-chrome.md): Menus, dialogs, toasts and title bars drawn in Horadric's own look.
- [Quality of life](plan/quality-of-life.md): Seven small daily-use fixes.
- [The task list](plan/the-task-list.md): Per-project work list agents take items from (became the quest log).
- [Background sessions](plan/background-sessions.md): Claude Code's `--bg` sessions shown as tiles.
- [Identify](plan/identify.md): Finished turns stay unidentified until looked at.
- [Stay a while and listen](plan/stay-a-while-and-listen.md): Deckard Cain's catch-up of what happened while away.
- [The stash](plan/the-stash.md): Sessions put away by hand and brought back as they were.
- [Tal Rasha's tombs](plan/tal-rasha-s-tombs.md): One quest worked by several sessions in worktrees, keep the best.
- [Transmute](plan/transmute.md): The Horadric Cube: drop tiles in, run a recipe.
- [Runewords](plan/runewords.md): Named sequences of actions given to a session one per turn.
- [Experience](plan/experience.md): XP per landed commit and a level in the tray.
- [The Cow Level](plan/the-cow-level.md): A secret cube recipe.
- [Quests](plan/quests.md): The task list renamed to a quest log, and its file format.
- [The quest log window](plan/the-quest-log-window.md): Window showing finished quests and how they ended.
- [Waits a blocked quest can name](plan/waits-a-blocked-quest-can-name.md): Blocked quests that wait on another quest instead of the human.
- [Performance](plan/performance.md): UI thread performance pass and what is left.
- [The Runetome](plan/the-runetome.md): Runewords as rune stones in a tile, errands with schedules.

## Next

- [macOS](plan/macos.md): Horadric on a Mac.
- [Codex and Grok Build beside Claude Code](plan/codex-and-grok-build-beside-claude-code.md): ChatGPT and xAI agents used like Claude, step by step.
- [Discord Activity](plan/discord-activity.md): Rich Presence on the human's Discord profile.
- [Orchestration](plan/orchestration.md): Quests run by the dozen without the human; Warriv. Built 2026-10-05.
- [Warriv in the quest log](plan/warriv-in-the-quest-log.md): Making Warriv's wakes visible in the quest log.
- [The caravan moves on its own](plan/the-caravan-moves-on-its-own.md): Warriv proposing and planning work when the log runs dry.
- [Warriv drives](plan/warriv-drives.md): The autonomous loop: Warriv drives, ships local and public by switch.
- [The agent's cursor](plan/the-agent-s-cursor.md): Agents testing without taking the human's mouse. Proposed.
- [Step 4: worktrees and the git glance](plan/step-4-worktrees-and-the-git-glance.md): Worktree per session, setup commands, ports, git status on tiles.
- [SSH hosts](plan/ssh-hosts.md): A project's servers as terminals and known to agents.
- [Fleets](plan/fleets.md): Hundreds of SSH devices per project.
- [Waypoints](plan/waypoints.md): Several machines with Horadric, each seeing the others' sessions.
- [Secrets](plan/secrets.md): Tokens for quests that push, call APIs or deploy.
- [Sessions that outlive Horadric](plan/sessions-that-outlive-horadric.md): Session host processes that survive the UI.
- [Step 5: inbox, installer, updater](plan/step-5-inbox-installer-updater.md): What became of the inbox, installer and updater.
- [The updater](plan/the-updater.md): Signed manifest, update check and install.
- [The Settings window](plan/the-settings-window.md): Settings moved out of the tray menu into a window.
- [Claude Code 2.1.287 to 2.1.296](plan/claude-code-2-1-287-to-2-1-296.md): Using what is new in recent Claude Code, incl. OSC 7501 and cloud tiles.
- [Town Portal on the phone, round two](plan/town-portal-on-the-phone-round-two.md): The phone page as an inbox, not a squeezed terminal.
- [Lore](plan/lore.md): Fewer tokens per quest: spend counts, context packs, shared notes, a code outline and index, a shorter quest log.

## Not in any step yet, but needed before daily use

- **Expanding from a synthetic click can open behind other windows.** Windows
  only lets a process take the foreground after real input. A real click on
  a tile is real input, so this only bites scripted tests.
- **New tray icons start hidden.** Windows 11 puts them behind the `^`
  overflow until the user drags them out or turns them on in Settings.

## Open questions

Carried from the concept, with what is known now.

- **Can a tile light up without stealing focus?** Yes, so far.
  `WS_EX_NOACTIVATE` plus `SWP_NOACTIVATE` holds through raising.
- **How does a session get named?** Claude Code writes its own title for
  the conversation into the transcript (`ai-title`, or `custom-title` after
  `/rename`). The listener reads the transcript's tail on `SessionStart`,
  `UserPromptSubmit` and `Stop` and the tile shows that title. A name given
  with `--name` beats Claude's title but not a `/rename`. The folder name is
  the fallback until the first turn ends.
- **What about the VS Code extension's Claude?** Still unanswered. Probably
  the answer is to stop using it.

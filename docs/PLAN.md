# Implementation plan

Where Horadric is, what comes next, and what was decided along the way. It
is the handover document: someone picking the project up cold should need
nothing else.

This page is the index of work still to do. Each part is a file of its
own in [plan/](plan/), with its reasoning and decisions. Read this page,
then open only the parts your work touches. A new part is a new file in
`plan/` and a line here under Next.

When a part is built, its decisions, traps and untested corners go into
the lore as notes (`lore_note`, naming the code files), and its file and
line here are deleted. So what was decided about anything already built
is found with `lore_recall`, by question or by file, and the old plan
files are in git history (removed 2026-10-19).

Ideas that are not planned yet, most of them from the Diablo name, are in
[IDEAS.md](IDEAS.md).

## Shape of the thing

Six crates, one binary.

| Crate | Owns | Platform |
|---|---|---|
| `horadric-core` | Session, Phase, Registry, the state machine | any |
| `horadric-hooks` | The localhost listener, its client, the settings installer | any |
| `horadric-pty` | ConPTY pseudo consoles, the session host and its pipe protocol | Windows |
| `horadric-ui` | Cluster and terminal windows, layout, drawing | Windows |
| `horadric-memory` | Embeddings, the memory store, recall | any |
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
side so typing never blocks the UI. The host process has its own. Everything off the UI thread reaches it as a message to one
hidden message-only window, never a thread message: thread messages are
dropped while Windows runs a modal loop, and dragging a terminal's edge is
one. The registry is an `Arc<Mutex<Registry>>`, each console's grid an
`Arc<Console>` with the terminal behind a mutex.

## Next

- [macOS](plan/macos.md): what is left of Horadric on a Mac: the browser pane, quests and Warriv, files, usage, Discord, Settings.
- [Orchestration](plan/orchestration.md): a Warriv session restored after a restart, and which model Warriv runs on.
- [Claude Code 2.1.287 to 2.1.296](plan/claude-code-2-1-287-to-2-1-296.md): cloud session tiles, waiting on `claude agents --json`.
- [Lore](plan/lore.md): the read guard, compaction at the right moment, the bench still to run.
- [Memory](plan/memory.md): the small gaps left in long term memory.
- [Performance](plan/performance.md): what is left of the UI thread performance pass.
- [The agent's cursor](plan/the-agent-s-cursor.md): agents testing without taking the human's mouse. Proposed.
- [Waypoints](plan/waypoints.md): several machines with Horadric, each seeing the others' sessions.
- [Secrets](plan/secrets.md): tokens for quests that push, call APIs or deploy.
- [Identities](plan/identities.md): each project's gh, git, npm and Vercel logins, so no session switches another's account.

## Loose ends

Small things left over from parts that are built.

- **Browser pane:** point at part of a page and send it to a session
  (screenshot, address, element). And protect the DevTools port: it is
  on loopback only, so any program on the machine can drive the logged
  in browser.
- **Tristram, open questions:** browser tabs as tiles in project clusters
  too, one town or several (work and private), whether a project session
  can ask in Tristram, and the town's folder on macOS.
- **Stay a while and listen:** a sentence per session on what it did,
  from a small model's summary of the run.
- **The Arcane Sanctuary:** hover showing who touched a file last.
- **Keys:** the Kitty keyboard protocol's gaps: the modifier keys
  themselves under "report all keys", the lock modifiers, the keypad's
  own codes.
- **Code signing:** the binaries are unsigned. The updater does not need
  it, but a hand download does, and so does the Windows 11 short context
  menu (Windows refused the unsigned package, 2026-10-13). An NSIS
  installer would go with it.
- **Never tried for real:** a second Claude, Codex or Grok subscription,
  SSH hosts and fleets against a real server, an IME. The lore notes on
  each part say what was only unit tested.

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

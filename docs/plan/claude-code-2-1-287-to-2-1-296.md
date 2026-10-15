# Claude Code 2.1.287 to 2.1.296

Asked for on 2026-10-11 after reading the changelog through 2.1.296:
use what is new. Each part is a quest in the log.

- **OSC 7501, the Program Status Protocol** (2.1.295). Claude Code,
  Codex, Crush, Terraform and Homebrew write their state into the
  terminal stream: idle, working, blocked, done or error, with progress
  and a message. Find the spec itself before building; the coverage on
  2026-10-11 was secondhand. This is **an exception to "state comes from
  hook events, never from parsing terminal output"**, decided
  2026-10-11: OSC 7501 is a status the program sends on purpose, in a
  fixed format, not scraping what it shows. Hooks still win where both
  speak. It gives a phase to sessions that have no hooks (an agent
  without them, a plain terminal running a long build) and a second
  opinion when a hook is lost. The parser is pure and tested; it reads
  the sequence where the console already sees the bytes, and the grid
  never has to understand it.

  **Built 2026-10-11.** The spec is revision 0.4 of 2026-10-10, at
  superlogical.com/rex/docs/build/program-status (announced by Mitchell
  Hashimoto on 2026-10-06; 0.1 was 2026-09-28). Its format:

  > Report: `OSC 7501 ; pairs ST`. Feature detection: `OSC 7501 ; ? ST`.
  > `pairs := pair (":" pair)*`, `pair := key "=" value`,
  > `key := [a-z]+`, `value := [A-Za-z0-9_.,+/=-]*`. ST is `ESC \` or BEL.
  > Keys: `state` (required: `idle`, `working`, `done`, `blocked`,
  > `error` or `clear`; an unknown value discards the report), `id`
  > (slash separated, omitted for the root record), `kind` (with
  > `blocked` only: `permission`, `question` or `auth`), `progress`
  > (0 to 100, for `working` or `blocked`), `app` (up to 32 characters),
  > `title` and `msg` (base64 of one line of UTF-8, padding optional).
  > Unknown keys are ignored; a repeated key's last value wins; each
  > report replaces its record whole. `clear` removes the record and
  > every record beneath it, or every record without an `id`. On process
  > exit or a new prompt (OSC 133 A) `working` and `blocked` are
  > dropped; `done` and `error` persist. A sequence is at most 4096
  > bytes. A terminal that supports it answers `OSC 7501 ; ? ST`.

  `program_status.rs` in core holds the scanner (finds the sequences in
  output cut anywhere into chunks), the parser and the records, and maps
  the most urgent record (blocked, error, working, done, idle) to a
  phase: `blocked` by kind to permission, question or dialog, `error` to
  the error wait. The console scans each chunk before the grid parses
  it, answers the query, and keeps the records. The session takes the
  phase at once when it has no hooks (`Session::hooked` is when a hook
  from the agent itself was last heard). With hooks it takes it only to
  end a turn its hooks still call working, after 10 seconds of the
  program saying so with no hook since: a lost hook, or Claude Code's
  Esc, which no hook reports. A query in the replay of a reattached host
  is not answered, as the answer would be typed into today's prompt.

  Verified on screen with a cmd.exe terminal running a script: the tile
  went working with "Building the thing (40%)", lit as "needs
  permission, Apply 3 to add?", turned done with its message, and went
  idle on `clear`. **Claude Code on Windows mostly does not turn it on.**
  It sends its status only after the terminal answers `?`, and it fences
  the question with `CSI c`, taking an answer to the fence first as no.
  The pseudo console answers `CSI c` itself, at once, while the query
  has to go out through its output to Horadric and the answer back in,
  so the fence wins nearly every time. Worse, the pseudo console drops
  any OSC written to its input, so the answer is typed instead, as
  win32-input-mode key presses, which carry each byte through. Claude
  Code 2.1.296 heard yes in 4 of 12 starts in a bare pseudo console
  harness, and in none of 3 through a dev instance, whose answer takes
  longer.
  On a Mac there is no pseudo console, and answering before the grid
  puts the answer ahead of the fence's.

  **Windows fixed 2026-10-11 by bundling the newer pseudo console.**
  `PSEUDOCONSOLE_PASSTHROUGH_MODE` does nothing in the console host of
  Windows 11 build 26200 (it still answers `CSI c` itself), and the
  ConPTY rewrite in Windows Terminal 1.22 dropped the flag. Claude Code
  2.1.296 has no switch either: `programStatus` starts pending and only
  a reply ahead of the fence's settles it yes. But OpenConsole from the
  `Microsoft.Windows.Console.ConPTY` NuGet package (1.25.260930003, MIT)
  passes `CSI c` on to the terminal, and an OSC typed into its input
  reaches the program. Both files sit in `crates/horadric-pty/conpty`,
  embedded in the binary; the UI unpacks them to `hosts\conpty-<version>`
  beside the host copies, and a host loads `conpty.dll` from beside
  itself, falling back to Windows' own when it is missing or
  `HORADRIC_CONPTY=inbox`. Claude Code then said `programStatus=yes` in
  3 of 3 starts in a harness and through a dev instance, against no
  behind Windows' own. Since the grid now sees `CSI c` and colour
  queries, a reattached host's replay is parsed with the grid's answers
  muted, as its OSC 7501 queries already were.
  **Codex and Grok checked 2026-10-13.** Codex CLI 0.159.0 and Grok Build
  1.0.46 (the installed builds) hold no OSC 7501 in their binaries at all,
  so neither sends it yet, and nothing in Horadric needs to change for
  them. The console scans every agent's output the same way, a session
  without hooks (an unreviewed Codex hook) takes the program's phase at
  once, and one with hooks takes it to end a turn its hooks still call
  working, which is the gap they leave (Codex sends no `Stop` for a failed
  turn). When either ships the protocol this works with no further code.
  Recheck with `grep -a 7501` on the binary after an upgrade.
- **Claude Mods** (2.1.287). Plugins can run TypeScript inside a
  session. Horadric ships a plugin with a mod, installed and removed with
  the hooks and refused by a dev instance as the hooks are. The mod
  reports what hooks can not: `$.agent.list()` with each subagent and
  teammate idle or waiting, `agent.spawn` with the agent type, workflow
  agents with their run. Through the band above the prompt it can tell
  Claude and the human what Horadric knows: another session editing the
  same file (`overlap.rs`), the quest this session holds.
  **Built (2026-10-11), the reporting half.** The mod's source is
  `crates/horadric-hooks/mod`, embedded in the binary and written to
  `~/.claude/horadric-mod` by `horadric_hooks::install::install`, which
  every install, `hooks install` and ship local calls; `uninstall` takes
  it away. It is loaded by naming that folder in `CLAUDE_CODE_PLUGIN_DIRS`
  in the settings' `env`, after any folders of the user's own, since a
  marketplace would need `claude plugin install`. The folder is rewritten
  only when its files changed, because every running session reloads the
  mod on a write. Without `HORADRIC_SESSION` and `HORADRIC_OWNER_PORT` it
  does nothing; with them it posts to the owner's `/horadric/hook` with
  the tag: `HoradricSpawn` (`agent_id`, `agent_type`, `model`,
  `workflow_run`) as a subagent starts, and `HoradricAgents` (the whole
  `$.agent.list()`, each `agent_id`, `agent_type`, `status`) whenever it
  changes, looked at every 2 seconds. Neither moves a phase yet.
  `claude plugin test crates/horadric-hooks/mod` runs its tests.
  **The band, built (2026-10-11).** Every 3 seconds the mod posts to the
  owner's `/horadric/know` with the tag (refused without one or from a web
  page). The listener adds the files the session shares with others from
  its claims (`Claims::shared`, the same claims the `PostToolUse` reply
  warns from), the app adds the quest the session holds and names the
  others as their tiles are, and answers `{quest, overlaps: [{file, path,
  others}]}`, empty when there is nothing to say. The mod keeps it in
  `$.state` and draws it above the prompt ("Quest ...", "overlap.rs also
  changed by alpha, not committed"), nothing when it is empty or
  Horadric is closed. A shared file also goes to Claude as context on the
  next prompt, once per change. Verified on screen with two sessions on
  one file. The theme colours (`warning`, `subtle`) come as truecolour
  and Horadric's terminal draws them. They looked plain only because the
  dev instance was started from Claude Code's PowerShell tool, which sets
  `NO_COLOR=1`, and every tile inherited it: Claude's whole screen was
  colourless. A Horadric running under Claude Code (`CLAUDECODE` set) now
  drops `NO_COLOR` from its terminals with the parent session's
  variables; a person's own `NO_COLOR` stays (2026-10-11).
- **Subagents on the tile.** Today a tile counts subagents by `agent_id`
  and calls one finished after 30 quiet seconds. With the mod's reports
  the tile can show each by type, and when it really ends. **Built (2026-10-11).** `Session::subagent_types` uses the mod roster (running, waiting or pending) where it has reported this turn, and the 30 second quiet guess only for ids the roster lacks. A working tile's last line lists the types, "Explore x2, Plan".
- **What a waiting tile waits on** (2.1.295). A session waiting on a
  permission prompt now names the MCP tool. The tile says "waiting on
  browser_click", not just waiting.
- **Workflow agents' model** (2.1.296). `CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL`
  sets one model for every workflow agent. A Sessions default beside
  model and effort, passed into every session's environment.
- **`onFailure: "block"`** (2.1.295) on a hook makes a failed hook block
  the action. Horadric's hooks must never carry it: a closed Horadric
  would stop every prompt on the machine. The installer strips it and a
  test says so.
- **Cloud sessions.** Claude Code runs on its own cloud machine now
  (blog, 2026-10-06). Tiles for them beside the local ones are a new
  kind of session with no terminal here. Proposed first, not built.
  The proposal is below, under Cloud session tiles.

## Cloud session tiles (proposal, 2026-10-11)

Not built. **Decided 2026-10-13: option 4, wait.** The human chose to
build nothing until Claude Code itself lists cloud sessions on this
machine. When `claude agents --json` reports them (a `kind` such as
`cloud`), the feeder that finds background sessions picks them up, and
the tile work starts from there. Until then, no private APIs, no
attach-only tiles and no hooks in the cloud.

**What this machine can see today**, checked with Claude Code 2.1.296:

- `claude agents --json --all` lists only local sessions: `kind` is
  `interactive` or `background`, nothing else. No cloud session appears
  and no flag asks for them. Horadric already polls this list for
  background sessions (see Background sessions above).
- `claude --cloud [description|session_id|url]` creates a cloud session
  or attaches this terminal to one, by id or by its claude.ai/code URL.
  `--environment <ccpool_...>` runs a new one on a self-hosted pool.
  `--teleport [session]` pulls a cloud session down to run here.
- `claude --remote-control [name]` makes a local session reachable from
  claude.ai and the phone. It goes the other way: it publishes this
  machine's sessions, it does not list the cloud's.
- Claude Code's own agent listing (the one a session uses to message
  peers) can show "your Claude sessions running in the cloud" and Remote
  Control sessions on other machines, so the account does expose such a
  list. It is not a documented command or API that Horadric can call.
- `claude -p "msg" --cloud <id> --output-format json` queues a message
  to a cloud session and exits with `{ok, session_id, url}`. It does not
  wait for the reply. So a tile could answer a cloud session without a
  pane, but not follow it.
- The docs (code.claude.com, agent-view) say outright that cloud
  sessions are not in `claude agents`. They document no command, local
  API or event stream that lists cloud sessions or their state.
- HTTP APIs, per platform.claude.com: routines' `/fire` starts a run and
  returns its id, nothing more. The Managed Agents Sessions API (list,
  get, stream) is for API key agents and does not say it covers
  claude.ai/code sessions. The Compliance API lists remote sessions with
  a status, but needs an org admin's key. None is the user's own list
  under their claude.ai login.
- Hooks run inside the cloud machine, and only those committed in the
  repository's `.claude/settings.json` (not `~/.claude/settings.json`,
  where Horadric's live). They would post to the cloud machine's
  localhost. To reach anything outside, the environment's network must
  be Full or list the host under Custom; the default, Trusted, answers
  `403 host_not_allowed`.

**Options.**

1. **Attach on click, list nothing.** A cloud session the human starts
   or attaches from Horadric (`claude --cloud <id or URL>` in a pane,
   `Run::Cloud`, like `Run::Attach`) gets a tile with a cloud mark. Its
   phase comes from OSC 7501 in the attached pane, since hooks do not
   reach us. Closing the pane leaves the tile dimmed, "in the cloud",
   until clicked again. Cheap, uses only documented commands, and needs
   the OSC 7501 parser from the quest beside this one. It misses
   sessions started on the web or the phone until the human pastes
   their URL ("Follow a cloud session..." in the tray).
2. **Poll the account's session list.** Call whatever the CLI calls to
   list cloud sessions, with the CLI's own login, every minute. Sees
   every cloud session, including ones started elsewhere, with a phase.
   But it is undocumented, would mean reading Claude Code's OAuth token
   and an HTTPS client (a new dependency, or WinHTTP by hand), and can
   break on any release. If Claude Code adds `kind: "cloud"` to `claude
   agents --json`, this becomes free and safe, and Horadric already
   reads that list.
3. **Hooks out of the cloud.** The repository's committed
   `.claude/settings.json` carries a hook that posts to Horadric through
   the Town Portal relay. Real hook phases, same as local. But it means
   committing Horadric hooks into every repository, setting each cloud
   environment's network to reach the relay, and a key on a machine
   Horadric does not own. Most work, most moving parts.
4. **Wait.** Do nothing until Claude Code lists cloud sessions locally.

**Recommendation: 1 now, 2 only through `claude agents`.** Option 1 is
small, uses documented commands, and the OSC 7501 parser lands anyway.
It gives the tile, the stage pane and a phase while attached. For the
list, do not reach into private APIs: file a request upstream for cloud
sessions in `claude agents --json`, and when they appear there, the
feeder that finds background sessions picks them up with a new `kind`.
Option 3 stays a note for if a phase is wanted while nothing is attached.

**Questions for the human.** Is a cloud tile only worth having if it
shows sessions started on the web or phone (then option 1 is not
enough, and the choice is between 2 and waiting)? And does attaching
a pane cost anything on the cloud side (it keeps a connection open, but
the session runs either way)?

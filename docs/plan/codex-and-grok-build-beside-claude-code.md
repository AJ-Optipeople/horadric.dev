# Codex and Grok Build beside Claude Code

Asked for: a ChatGPT subscription and an xAI subscription used from
Horadric the way a Claude one is, tiles, stage, limits and account
switching included. API keys wait. The concept left other agents out of
v1 because hooks were Claude Code's alone. That has changed: both now have
hooks close to Claude Code's, and one of them even reads Claude Code's.

**What each gives**, read from their docs, their source and this machine.
The spike (2026-09-29) installed Codex CLI 0.159.0 from npm and read
Grok Build 0.2.22. The live checks (2026-09-30) ran real turns on both
after the human signed in again: Codex 0.159.0 on a free ChatGPT plan,
Grok Build 1.0.44 on a free xAI plan, with throwaway logging hooks, `exec`
and `-p` turns, and each TUI driven through a pseudo console. "Seen"
means it happened on one of those turns.

| | Claude Code | Codex CLI (ChatGPT) | Grok Build (xAI) |
|---|---|---|---|
| Program | `claude` | `codex`, from npm `@openai/codex` | `grok`, in `~/.grok/bin` |
| Login | `.credentials.json` + `.claude.json` | `$CODEX_HOME/auth.json`: `auth_mode`, `tokens` (`id_token`, `access_token`, `refresh_token`, `account_id`), `last_refresh` | `$GROK_HOME/auth.json`: one entry per `issuer::client_id`, with `email`, `key`, `refresh_token`, `expires_at` |
| Login read again while running | no | only on a 401 and before a refresh, never watched; refreshed tokens are written back; a different `account_id` on disk is a permanent error (from the source) | read at session start; a running session did not notice the file emptied or its key changed, and kept its held login (seen). The binary has reload code ("auth.json changed but token key is identical") that did not fire |
| Hooks | `http`, headers from env | `command` only, in `~/.codex/hooks.json`, `config.toml` or `-c hooks.<Event>=[...]`; the parent's environment reaches it (seen); an unreviewed hook is skipped silently unless trusted in `/hooks` or run with `--dangerously-bypass-hook-trust` (seen) | `command` and `http`, in `$GROK_HOME/hooks/*.json` (always trusted; a project's `.grok/hooks` needs folder trust); the `http` fields are `type`, `url`, `timeout`, `env`, `matcher`, no headers; every `http://` URL is refused, `127.0.0.1` and `localhost` alike ("SSRF protection: only https:// URLs are allowed"), while `https://127.0.0.1` is tried (seen on 1.0.44); a command hook gets the parent's environment (seen); `[compat.claude] hooks = false` stops it running Claude's hooks, though `grok inspect` still lists them |
| Events | the set Horadric uses | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`, `Interrupt`, `SessionEnd` and more. Seen: `PermissionRequest` when the TUI asks; `Interrupt` on Esc mid turn or at an approval, with no `Stop` after it; `Stop` at every completed turn; no `Stop` for a turn that failed; `SessionEnd` with `reason: "other"` for a failed and a good session alike. `exec` forces approval to never, so it never asks | `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied`, `Notification`, `Stop`, `StopFailure`, `StopCancelled`, `SessionEnd`, subagent and compact events. Seen: `Notification` `permission_prompt` when it asks ("Tool permission requested", "Plan approval requested"); `Stop` with `reason: "end_turn"` at every completed turn; `StopCancelled` instead of `Stop` for a rejected prompt (`permission_rejected`, after `PermissionDenied`) and for Ctrl+C (`user_interrupt`); `Notification` `idle_prompt` 60 s after a turn; at quit, `SessionEnd` and then one more `Stop` with `reason: "shutdown"` |
| Payload | `session_id`, `cwd` | `session_id`, `hook_event_name` (Pascal case), `cwd`, `transcript_path`, `model`, `permission_mode`, plus `turn_id` and `prompt` on a turn, `source` on start (seen) | both spellings: `sessionId` and `session_id`, `hookEventName` (snake case) and `hook_event_name` (Pascal case); plus `cwd`, `workspaceRoot`, `transcriptPath`, `timestamp`, `permissionMode`, `promptId` on a turn, `reason` on `Stop` (seen) |
| Resume | `--resume <id>` | `codex resume <id>` and `codex exec resume <id>`, any session in `$CODEX_HOME/sessions`, the desktop app's too and from another cwd (seen) | `--resume <id>`, `-c` |
| Limits | status line JSON | `token_count` events in `sessions/**/rollout-*.jsonl`: `rate_limits.primary` (300 min) and `secondary` (weekly), `used_percent`, `resets_at` | none found; `/usage` shows credits in the TUI |
| Model, effort, permissions | `--model`, `--effort`, `--permission-mode` | `-m`, `-c model_reasoning_effort=`, `--ask-for-approval`, `--sandbox` | `-m`, `--effort`, `--permission-mode`, `--always-approve`; the default is `[ui] permission_mode`, which Shift+Tab in the TUI rewrites (seen); `--permission-mode default` still reported `auto` (seen) |

**The shape.** An `Agent` in `horadric-core`, Claude, Codex or Grok,
saved on every session (missing reads as Claude, so old state loads). It
answers every question that is Claude specific today: the program, the
tag it gets, how its hooks are installed, how its events become a
`HookEvent`, its resume arguments, its flags for the defaults, its login
files and its limits. Everything downstream, the registry, phases, tiles,
stage and journal, keeps working on `HookEvent` and does not learn the
agent's name. No new dependency: the Grok hook file is JSON, Codex's
hooks go on its command line, so Horadric never has to write a TOML
file, and a JWT's payload is base64 and JSON.

**Steps**, each landing on its own:

1. **Spike, done 2026-09-29, live checks 2026-09-30.** What it found
   is in the table. The answers to the questions it was given:
   - A Codex command hook sees the parent's environment:
     `HORADRIC_SESSION` and `HORADRIC_OWNER_PORT` reached it.
   - Codex reads `auth.json` again only when a request gets a 401 and
     before it refreshes, and writes the refreshed tokens back. If the
     file now holds another `account_id`, it stops with a permanent
     error rather than use it. A failed refresh leaves the file alone.
   - `codex resume <id>` takes a session started elsewhere: a Codex
     Desktop session (originator "Codex Desktop", another cwd) loaded,
     and an unknown id says "no rollout found".
   - A Grok `http` hook cannot reach Horadric. 1.0.44 still refuses
     every `http://` URL, loopback included ("only https:// URLs are
     allowed for HTTP hooks"). It does try `https://127.0.0.1`, so only
     a TLS listener with a certificate Grok trusts would work, which is
     not worth it. Grok needs a command hook, the same `horadric hook`
     Codex uses. Its command hooks see the parent's environment.
   - The Claude hook Grok borrows from `~/.claude/settings.json` is an
     `http` hook to `127.0.0.1`, so Grok refuses it and posts nothing.
     There is no header question: nothing arrives. Grok also loads the
     hooks of installed Claude plugins.
   - Grok is waiting on you at a `Notification` whose
     `notificationType` is `permission_prompt`, for a tool ("Tool
     permission requested") and for a plan ("Plan approval
     requested"). 1.x does send `idle_prompt`, 60 s after a turn ends,
     and that is not waiting.
   - Grok's `Stop` comes at the end of every completed turn, with
     `reason: "end_turn"`. A rejected prompt and a Ctrl+C send
     `StopCancelled` instead (`permission_rejected`,
     `user_interrupt`), and an API error sends `StopFailure`. Quitting
     sends `SessionEnd` and then one more `Stop` with `reason:
     "shutdown"`, which must not read as a finished turn.
   - Grok reads `auth.json` at session start. A running session kept
     answering after the file was emptied and after its key was
     changed, with no reload in its log. The binary has reload code
     (it compares the token key), but nothing set it off in 45 s.
   - Codex on real TUI turns: `PermissionRequest` fires when the
     approval shows, `Interrupt` on Esc at the approval and on Esc mid
     turn, and neither is followed by `Stop`. `Stop` comes at the end
     of every completed turn, with `last_assistant_message`. `codex
     exec` forces approval to never, so it never asks.
   - A failed Codex turn (the configured `gpt-5.5` is refused on a free
     plan) sent `SessionStart`, `UserPromptSubmit` and then only
     `SessionEnd`, and `SessionEnd` has `reason: "other"` after a good
     session too. So a failed turn is a `SessionEnd`, or a new
     `UserPromptSubmit`, with no `Stop` since the last prompt.
   - Codex's `-c` flags must all sit on one side of the subcommand.
     Hooks given before `exec` were dropped without a word once
     another `-c` came after it.
   - The Codex TUI asks "Trust this folder?" the first time it opens in
     a folder, before any hook runs. A project's first Codex tile shows
     that question, and it is the user's to answer.
2. **`horadric hook`, the command hook. Done 2026-09-30.** `horadric
   hook codex|grok` posts the payload as it came with an
   `X-Horadric-Agent` header, and the listener reads it in that agent's
   shape through `Agent::event`: Codex's `prompt` becomes the prompt,
   Grok's camel case keys and snake case event names become Claude's.
   The event names are left as each agent sends them, for steps 3 and 4
   to give phases. It prints nothing, so an overlap warning does not
   reach these agents yet. Both agents need it, since
   neither can post to Horadric over HTTP. It reads the event on stdin
   and posts it with the tag from its environment, as `horadric status`
   does for the status line, adding which agent sent it. It is the one
   place a process is spawned per event, which Claude Code does not
   need, so it must start fast and never block the agent (a short
   timeout, fail open), and it does nothing when `HORADRIC_SESSION` is
   unset, so an agent started outside Horadric is not slowed.
3. **Codex. Done 2026-09-30.** Horadric passes its hooks on the command line it starts,
   `-c hooks.<Event>=[{hooks=[{type="command",command="..."}]}]` for each
   event, with `--dangerously-bypass-hook-trust`, since a hook nobody has
   reviewed in `/hooks` is skipped without a word. That keeps
   `~/.codex` untouched: nothing for `install` to add, a `codex` started
   outside Horadric runs no Horadric hook, and a dev instance and the
   installed one each pass their own. The cost is that the bypass also
   runs any unreviewed hooks of the user's own. `PermissionRequest` is
   waiting, `Stop` is done, `Interrupt` is idle, and a `SessionEnd`
   after a turn with no `Stop` is a turn that failed. Every `-c` flag
   goes before the subcommand, together, or the hooks are lost.
   `codex resume <id>` carries a paused tile on.
   Live check: through the npm `codex.cmd`, `SessionStart`,
   `UserPromptSubmit`, `Stop` and `SessionEnd` all arrived, and a `-c`
   after `exec` lost them all again, so `Agent::line` moves the
   session's own `-c` flags up beside the hooks. Codex runs only when
   `HORADRIC_AGENT` names it until step 5.
4. **Grok.** A `command` hook calling `horadric hook`, in
   `~/.grok/hooks/horadric.json`, added and removed by `install` and
   `uninstall`, the dev instance never. The Claude hook Grok borrows
   needs nothing: Grok 1.0.44 refuses it. Should a later Grok accept
   a loopback URL, the listener must still drop a Grok payload on the
   Claude path by its shape. Phases: `Notification` `permission_prompt`
   is waiting, `Stop` with `reason: "end_turn"` is done,
   `StopCancelled` is idle, `StopFailure` is failed, and the `Stop`
   with `reason: "shutdown"` after `SessionEnd` is ignored.
   `idle_prompt` changes nothing.
   Done 2026-09-30. The file is written only when Grok's home (`$GROK_HOME`
   or `~/.grok`) exists, so a machine without Grok gets no `~/.grok`, and
   `reload` rewrites it with the Claude hooks. Every entry says a 5 s
   timeout, since `Stop` is a gate Grok would otherwise wait ten minutes
   on. `StopFailure` reads its class from `error` and its text from
   `errorDetails`. The listener drops a payload with a camel case
   `hookEventName` that came in as Claude's. Live check: the same file,
   as a trusted project's `.grok/hooks`, ran `horadric.exe hook grok`
   unquoted with forward slashes on a real `grok -p` turn, and
   `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd` and the
   shutdown `Stop` all arrived tagged. Grok sessions start with step 5.
5. **Starting one.** The plus button and `horadric new --agent codex` start
   any agent found on PATH, Claude by default. A tile carries a small mark
   for its agent, so two sessions in one project can be told apart.
   History lists each agent's own conversations: Codex keeps them in
   `sessions/` by date, Grok in `sessions/<cwd>/<id>/summary.json`.
   Codex done 2026-09-30: `horadric new --agent codex`, and "New Codex
   session" in the project menu when `codex` is found (the plus stays
   Claude's). A paused tile resumes with `codex resume <id>`, without the
   prompt it was started with, which would be sent again. The defaults
   pass as `-m` and `-c model_reasoning_effort=`: a Claude model is not
   passed on, and Max becomes `xhigh`, where Codex's scale stops, until
   step 6 gives each agent its own lists. History reads each rollout's
   first line for its folder, the desktop app's too, with the name from
   `session_index.jsonl` or else the first prompt, and lists them among
   Claude's by date. Live check: a tile started, went done on "pong",
   and resumed its conversation after the dev UI and its hosts were
   killed, one `codex.exe` each time. The tile mark comes with Grok.
   Grok done 2026-09-30: "New Grok Build session" in the project menu and
   `horadric new --agent grok` when `grok` is found, on `PATH` or in
   `~/.grok/bin`, where its installer puts it. A paused tile resumes with
   `--resume <id>`, without the prompt it was started with. The defaults
   pass as `-m` and `--effort`, Max as `xhigh`, where Grok's scale stops
   too. History reads `sessions/<folder>/<id>/summary.json`, the folder
   percent encoded, titled by its `session_summary` or else the first
   prompt in `prompt_history.jsonl`. A tile that is not Claude's says
   "Codex" or "Grok" in small dim letters before its age; Claude's tiles
   carry no mark. Live check: a tile asked Grok's folder trust question,
   went done on "pong", and resumed its conversation after the dev UI and
   its hosts were killed, one `grok.exe` each time; a start with Max and a
   Claude model passed `--effort xhigh` and no model. Grok prints an
   "SSRF protection" line in its TUI for every event, from the Claude
   `http` hook it borrows from `~/.claude/settings.json`.
6. **Limits per provider.** The usage window gets a screen per provider
   in use, named this time ("Claude", "ChatGPT", "Grok"), since with more
   than one the numbers need saying whose they are. Codex's come from the
   newest `token_count` in the transcript its hooks point at, read on each
   `Stop`, and map straight onto `Limit`: `used_percent` and `resets_at`.
   Grok shows no limits until a source turns up. The settings follow the
   agent: each has its own models and efforts, and Default passes nothing.
   Done 2026-09-30. A screen for Claude always, and for Codex and Grok
   when they were found installed at start or have a session. With more
   than one, a line on top names the provider with a chevron, and a click
   on it goes on to the next; folded, the line stays, so the one limit
   left still says whose it is. The listener reads the rollout's last
   256 KB on a Codex `Stop` (`Limits::from_codex`) and the feeder keeps
   them per agent (`agent_usage` in `state.json`, Claude's stay `usage`,
   which goes with the account). A limit now keeps its `window_minutes`
   and is named by it: a free ChatGPT plan's one limit is 30 days long and
   reads "Month", not "Session". Each agent has its own lists
   (`Setting::choices`), Codex's models and efforts as its own picker has
   them (GPT-6 Luna to GPT-5.5, Low to Max), Grok's Grok 4.7 and Low to
   Extra high, and no permission modes, so its screen has Model, Effort
   and Version. They are saved as `agent_defaults`, passed as is, and never
   typed into a running session: only Claude Code has the commands.
   Tested with a dev instance: three named screens, a faked Codex `Stop`
   whose rollout said 41 % of five hours and 82 % of a week showed both
   under ChatGPT, GPT-5.5 picked from Codex's list and saved, Grok's
   slider with five stops and its screen saying it has no limits, and
   folded under a header. Not tried: a real Codex turn feeding the window.
7. **Accounts per provider**, built on the Claude switching above. Codex:
   the whole `auth.json` is the login, the account is `account_id`, the
   email is in the `id_token`. Horadric forces the file store if a user
   moved it to the OS keyring. A running Codex treats another account on
   disk as a permanent error, so a switch stops and resumes its sessions
   as a Claude switch does.
   Grok: the login is the one entry in `auth.json`. A running Grok did
   not notice a changed file in the live check, so a switch stops and
   resumes its sessions, as for Claude and Codex. A switch touches one
   provider's sessions and leaves the others running.
   Done 2026-09-30. Every provider's screen in the usage window has the
   Account row, and its list holds that provider's accounts alone, with
   the plan for ChatGPT ("Plus", from the `id_token`'s
   `chatgpt_plan_type`). An account now says whose it is (`agent`,
   missing reads as Claude, so `accounts.dat` from before loads), and
   Codex's and Grok's ids start with the agent's name. Codex's id is its
   `account_id` and the ChatGPT user, Grok's its `user_id` and `team_id`.
   The app follows each agent's login on its own (`Login`): watched,
   kept, put back after Add account, guarded for 15 seconds after a
   switch, and a switch waits only for that agent's sessions. Add
   account opens `codex login` or `grok login`. Every Codex start, resume
   and login gets `-c cli_auth_credentials_store=file`, the value bare
   since a non TOML value is taken as the string. A dev instance
   switches Codex only with `CODEX_HOME` set, Grok only with `GROK_HOME`.
   ChatGPT's limits go with its account as Claude's do.
   Tested with a dev instance, a fake `CODEX_HOME` and `GROK_HOME` with
   two made up logins each, and `codex.cmd` and `grok.cmd` stand-ins
   that post a `SessionStart` through `horadric hook`: a login written
   from outside was kept, the list named both with their plans, picking
   the other ChatGPT account stopped the Codex sessions only and resumed
   the prompted one with `resume <id>`, one process each, and a Grok
   switch picked while its session was mid turn said "after 1 turn" and
   went through at its `Stop`, `--resume grok-1`, Codex left alone. Not
   tested: real second subscriptions, and whether `codex login` or
   `grok login` revokes the login it replaces. Grok has a leader process
   (`grok leader`), and whether a resumed session reads the file again
   or gets the old login from a leader still running is for the first
   real switch to show.

**Not in this.** Cursor, Gemini and the rest, until one is asked for.
API keys and OpenRouter, asked to wait. Grok Build on Windows is
"best-effort" by its own README, so a failure there may be Grok's.

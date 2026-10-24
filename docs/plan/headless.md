# Headless sessions and the Folio

Decided on 2026-10-24. Proposed, not built. A Claude session's pane
shows Claude Code's terminal today: PowerShell output, tool calls and
diffs scroll past in full, take most of the room, and are rarely what
the human wants to read. The human wants Horadric to own the whole
experience, "the freedom to truly transform how we develop with AI".

**The decision.** Claude sessions run headless: Horadric starts the
installed, unmodified `claude.exe` with its stream-json protocol on
pipes, no terminal, and draws the conversation itself in a view called
the Folio. Not the Agent SDK: it is TypeScript or Python, so a runtime
beside the Rust app, and its terms forbid offering subscription login
in a product built on it. Driving the CLI directly is the same agent
loop, in Rust, on the human's own login.

**The grey zone, accepted.** Anthropic's legal page says developers
building products that interact with Claude, including on the Agent
SDK, should use API keys, and it explicitly allows a user signing in to
the unmodified Claude Code binary with their own subscription. A
headless CLI on the user's own login sits between those two. Horadric is
free, built for the human, friends and colleagues, and the human accepts
the risk: if Anthropic objects, we revert to terminal sessions and say
sorry. So terminal sessions stay working beside headless ones, never
deleted, and switching back is one setting. Sources:
[legal and compliance](https://code.claude.com/docs/en/legal-and-compliance),
[Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview).

**The settled rule still holds.** State comes from hook events, never
from parsing terminal output. The stream is not terminal output: it is
structured messages the program sends on purpose, like OSC 7501. Hooks
still fire headless (the project's and user's settings load unless
`--bare`, which we never pass) and still drive the phase. Where hooks
and the stream both speak, hooks win, as today.

## The protocol, as far as it is known

From the official docs and the Python Agent SDK's source, read
2026-10-24 against Claude Code 2.1.310. The stdout message types and
user message fields are documented. The control protocol
(`control_request`, `control_response`, `initialize`, `can_use_tool`)
is not formally specified: it is what the SDK speaks, and can change
without notice. Hence step 1 records real streams and every later step
tests against them.

- **Start.** `claude.exe --input-format stream-json --output-format
  stream-json --verbose --include-partial-messages
  --permission-prompt-tool stdio`, plus `--resume=<id>`,
  `--session-id=<uuid>`, `--model`, `--permission-mode`,
  `--append-system-prompt`, `--mcp-config` as today's terminal launch
  passes them. Always the `=` form for values. The SDK does not pass
  `-p`; whether the CLI needs it is checked in step 1. `claude.exe`
  directly with `CreateProcess`, never a `.cmd` shim. The environment
  drops `CLAUDECODE`, `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`,
  so the subscription login is what it uses (init says
  `apiKeySource: "none"`). Large config goes in `initialize`, not argv.
- **In, one JSON object a line.** A prompt is
  `{"type":"user","message":{"role":"user","content":...}}`, content a
  string or blocks (images as base64 blocks), with `uuid`, `priority`
  (`now`, `next`, `later`) and `client_composed`. Control requests:
  `initialize`, `interrupt`, `set_permission_mode`, `set_model`,
  `get_context_usage`, `rewind_files`, `stop_task`, `mcp_status`.
- **Out.** `system`/`init` first (session id, model, tools, slash
  commands, skills, agents, MCP servers, `capabilities`), then
  `assistant` messages (text, thinking, `tool_use` blocks), `user`
  messages carrying `tool_result`, `stream_event` token deltas, and
  one `result` per turn (cost, usage, duration, errors,
  `terminal_reason`). Also `compact_boundary`, `status`,
  `conversation_reset` after `/clear`, task events for background work,
  and `control_request`s from the CLI: `can_use_tool` and
  `hook_callback`, answered with a `control_response`.
- **Asks.** Every permission prompt is a `can_use_tool` request.
  AskUserQuestion is one too: allow it with `updatedInput` carrying
  `answers`. ExitPlanMode most likely is, to be confirmed. Always echo
  `updatedInput` on allow.
- **Slash commands** go as plain user text: custom commands, skills,
  `/compact`, `/clear`, `/model sonnet`, `/effort`. Pickers and
  `/login`, `/theme` do not exist headless.
- **One process, many turns.** Stdin stays open for the session;
  closing it ends it. A prompt sent mid turn is queued (`next`), or
  steers at once (`now`). Stdout is read on its own thread so the pipe
  never fills; lines can be large (tool results, images), so no 1 MB
  cap.

The full research brief, with every source, is in the lore (note left
on 2026-10-24).

## The shape

- **The host speaks for the session.** Each session already runs in a
  `horadric-host` process that outlives the UI. A headless session's
  host starts `claude.exe` on pipes instead of a ConPTY, owns the
  protocol, and keeps the session's messages since it started. A UI
  that attaches (after a reload or a crash) gets the transcript from
  disk plus the host's live tail, so nothing is lost, as a terminal's
  screen replay works today. The pipe protocol between UI and host gains
  message frames beside the byte stream.
- **A pure model in `horadric-core`.** `folio.rs`: messages in, a list
  of blocks out (prompt, reply, thinking, a run of tool steps, a diff,
  an ask, a turn's footer). Every rule of what folds into what lives
  here and is tested against recorded streams, with no window.
- **The view in `horadric-ui`.** The Folio draws in the stage pane where
  the terminal is, Direct2D and DirectWrite as everything else. It
  scrolls, selects and copies, finds text, and opens files and diffs in
  the file viewer.
- **The phase** still comes from hooks. The stream adds what hooks never
  carried: tokens as they come, cost per turn, context used, the plan.

## The Folio

The conversation as pages, not a log.

- **Your prompt** as a heading-like block, with any images.
- **Claude's reply** rendered: paragraphs, headings, lists, tables,
  code blocks with the file viewer's highlighting, links. Streams in as
  it is written.
- **Thinking** folded to one line, open on a click.
- **Tool work** folded into one line per run of steps: "Read 4 files ·
  Searched twice · Ran tests ✓ · Edited pane.rs +12 −3". A click opens
  the steps; a failed one opens by itself. A shell command shows its
  command and the last lines of output, never all of it.
- **Diffs** as a list of files changed with their counts. A click shows
  the diff inline; another opens the file.
- **Subagents** as lanes of their own under the step that started them,
  folded the same way, from `parent_tool_use_id`.
- **Asks** as cards with buttons: allow once, always, deny with a
  reason; questions as their options; a plan as a page to approve or
  send back. The phone's Town Portal answers the same pending request.
- **Each turn's footer:** time, cost, tokens, context left.
- **The composer** at the bottom: multi line, slash commands and skills
  from init in a list as you type `/`, `@` for files, images pasted,
  Esc interrupts, Enter while it works queues, Ctrl+Enter steers now.
  Model and permission mode as controls on it, sent as control requests.

The rune ring, the burning letters and the rest of the glass's light
carry over: the composer is where typing burns in.

## What the terminal gave for free, rebuilt

- `/model`, effort and permission mode pickers: controls on the
  composer.
- `/config`, `/theme`, `/login` and other terminal only commands: a
  "Open in terminal" action ends the headless process once idle and
  resumes the same session in a terminal pane (`--resume`); "Back to the
  Folio" does the reverse. One session, two ways in, never both at
  once.
- Ctrl+O's transcript and verbose mode: every block opens in place.
- The folder trust dialog: Horadric asks before the first session in a
  folder, since headless runs a project's hooks and MCP servers without
  asking.
- The status line: the footer and the tile get model, cost and context
  from the stream.

## What it opens

What a terminal could never do, the reason for all this.

- **Edit and resend** any earlier prompt: a fork at that message
  (`--fork-session`, `--resume-session-at`), the old branch kept, both
  browsable. Files rewound with `rewind_files`.
- **Review before it lands:** an edit mode where each Edit or Write
  comes as a diff card to accept, change or reject, through
  `can_use_tool` and `updatedInput`.
- **Steer mid turn** without stopping it, and queue the next prompts
  while it works.
- **The work as a map:** the steps of a turn as a timeline, the files it
  touched lit in the Arcane Sanctuary as they happen.
- **Many sessions, one view:** a prompt sent to several sessions at
  once, answers side by side.

These come after the Folio works; each gets its own line here when it is
next.

**Markdown is our own** (decided 2026-10-25): no `pulldown-cmark`. The
human wants as few dependencies as possible, and a reply needs only a
subset: paragraphs, headings, lists, code blocks and inline code,
emphasis, links, tables, quotes. A pure parser in `horadric-core`,
tested against replies from the fixtures; anything it does not know
shows as plain text.

## Steps

1. **Spike.** A host that starts headless `claude.exe`, a hidden debug
   command that sends a prompt and prints the stream. Confirm: no `-p`
   needed or the flag it does need, subscription login with
   `apiKeySource: "none"`, hooks fire and the tile's phase follows,
   `can_use_tool` for a Bash ask, AskUserQuestion, ExitPlanMode,
   interrupt, resume of a session started in a terminal and the reverse.
   Record each stream as a fixture.
2. **The model.** `folio.rs` and its tests against the fixtures.
3. **The host.** Headless sessions in hosts that outlive the UI, with
   replay on attach. Verify a reload mid turn loses nothing, and count
   `claude.exe` after.
4. **The Folio, plain.** Prompt, reply as plain text, tool runs folded,
   the composer, interrupt, asks as cards. A setting, off by default,
   makes new Claude sessions headless.
5. **The Folio, rich.** Markdown, code highlighting, diffs, subagent
   lanes, images, slash command list, model and mode controls, footer.
6. **Open in terminal and back.**
7. **Default on** for new Claude sessions once the human has worked in
   it for a week. Codex and Grok keep their terminals.

Then "What it opens", one by one.

## Open questions

- **Quests and Warriv.** Whether quest sessions go headless too. Warriv
  reads sessions through hooks and transcripts, so it should not care.
- **The Brotherhood.** A headless session handed to another machine
  goes by its transcript, as today, and resumes headless there.
- **The protocol moving.** Feature detection by `capabilities`, and a
  check at start: an unknown message type is kept and shown raw, never
  dropped, and a stream that fails to parse falls back to a terminal.

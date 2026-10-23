# Signals from the agent

From the research pass of 2026-10-24: three things Claude Code already
sends that Horadric reads only in part or not at all. Each adds to the
hooks, never replaces them. Hooks win where both speak.

## Status line JSON: what is left

Mostly built: every `claude` Horadric starts gets `horadric status` as
its status line, which posts the JSON to `/horadric/status`. Context
fullness and the five hour and weekly limits come from it (the lore has
the details, ask about `usage.rs`). What it does not do yet:

- **Cost per session.** `cost.total_cost_usd` arrives and is dropped.
  Show it on the tile's hover or menu, and sum it per project.
- **`refreshInterval`.** The JSON comes only on events, so an idle
  session's numbers age. A timer every 30 s or so keeps the usage window
  live without a reply. Measure what it costs a session first.
- **A `claude` started outside Horadric** has no status line from it, so
  its tile has no context bar. Decide whether the installer should set
  `statusLine` in `~/.claude/settings.json` too, chaining to the user's
  own command when there is one, or whether those tiles go without.

## OpenTelemetry export

Claude Code exports metrics and events over OTLP, `http/json` among the
protocols, each carrying `session.id` and `prompt.id`
(https://code.claude.com/docs/en/monitoring-usage). Events include
`api_request` with tokens, cache hits and cost, and `tool_decision`.
Traces, still in beta, have a `tool.blocked_on_user` span.

The plan: when Horadric starts a session, set
`CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`
and `OTEL_EXPORTER_OTLP_ENDPOINT` to the listener, and add an OTLP JSON
receiver there (`/v1/logs`, `/v1/metrics`). That gives an exact token
and cost history per session and per turn, which the status line only
gives as running totals.

Open: whether a user's own OTel settings must win (they should: leave
the variables alone when already set), and how much the receiver keeps.
Nothing goes off the machine.

## Progress sequences (OSC 9;4)

Claude Code writes OSC 9;4 progress to its terminal
(`terminalProgressBarEnabled`), the sequence Windows Terminal uses for
its tab progress
(https://learn.microsoft.com/windows/terminal/tutorials/progress-bar-sequences).
Horadric's grid drops it today.

- Catch it beside OSC 7501 in the reader thread. It is a status the
  program sends on purpose, so it fits the OSC 7501 exception to "never
  parse terminal output".
- Use it as a second "working" signal, and for plain terminals, which
  have no hooks: a build or install that sends it lights its tile.
- Show the most urgent state on the stage's taskbar button with
  `ITaskbarList3::SetProgressState` (error beats paused beats working).

## OSC 8 hyperlinks

Smaller, found in the same pass: Claude Code sends OSC 8 links. A Ctrl
click on one could open it in the browser pane, the way a path opens in
VS Code.

# Privacy audit, 2026-10-02

A read only review of what Horadric sends off the machine, what it can
capture, and who on the machine can reach it. Three parallel passes:
network egress, screen capture and the browser pane, local listeners and
stored data. Nothing was built or run, so the items marked unverified are
read from code.

## Short answer

- **No telemetry, analytics or crash reporting.** The only HTTP client
  is the updater (`horadric-ui/src/net.rs:40`, called from `update.rs`).
- **No silent screenshots of the desktop.** No BitBlt, PrintWindow,
  GetDC(null), DXGI duplication or Graphics Capture in code. The one
  capture is `browser_screenshot`, which pictures only Horadric's own
  browser pane page (`horadric/src/mcp.rs:355`).
- **No keyloggers, no clipboard polling, no reading other processes.**
  The window hook in `browsers.rs:75` only sees window style and exe name.
- **Credentials never leave the machine.** Agent OAuth tokens are read
  locally, kept DPAPI encrypted in `accounts.dat`, and touched by no
  network code.
- **The weak spot is the browser pane and the local endpoints**, which
  trust any process running on the machine.

## What leaves the machine

- **Update check**, GitHub `latest.json`: at start, then every 24 hours.
  A bare GET with `User-Agent: Horadric/<ver>`, so GitHub sees your IP.
  No way to turn it off.
- **Discord Rich Presence**, over Discord's local pipe: only when enabled
  (off by default). Session counts and state; the project name only in
  "With project names".
- **Browser pane** (WebView2): your navigation, agent tools, and tabs
  restored at start. Address bar searches go to Google.
- **Agent system prompt**, through the agent's own provider: task list,
  worktree context, SSH host and fleet device names.

## Concerns, ranked

1. **High: agents can read every logged in web session.** One WebView2
   profile is shared by all projects (`web.rs:190`). `browser_devtools`
   passes any CDP method (`drive.rs:71`), `browser_evaluate` runs any
   script, `browser_navigate` takes `file:` URLs. A prompt injected agent
   can read cookies or webmail with no confirmation.
2. **High: agent browsing is invisible when another project is on the
   stage.** The page renders on a hidden window (`web.rs:412`) and no tile
   shows that the pane is being driven.
3. **High on a shared PC: any local user can run code as you.** The hook
   listener on 127.0.0.1 has no secret. `POST /horadric/reload` with an
   `exe` spawns that program (`listener.rs:97`, `app.rs:2955`);
   `/horadric/new` starts an agent anywhere. Web pages are kept out by the
   Origin check; other local processes and other Windows users are not.
4. **Medium: an unauthenticated DevTools port.** WebView2 starts with a
   remote debugging port on 127.0.0.1 (`web.rs:1045`), written to
   `devtools-port`. Horadric no longer needs it; any local process can
   drive the logged in browser through it.
5. **Medium: guessable session ids.** Ids are `<name>-<seconds since
   midnight>` (`session.rs:595`), and the id is the only credential for
   `/horadric/browser`. A child process (an npm postinstall) inherits
   `HORADRIC_SESSION` anyway.
6. **Medium, conditional: hook port squatting.** Every `claude` posts full
   prompts and tool output to 127.0.0.1:43117. When Horadric is closed,
   whatever holds that port gets them and can answer hooks. No
   `SO_EXCLUSIVEADDRUSE` (unverified whether binding beside it works).
7. **Medium: update check cannot be turned off** and is not in the README.
8. **Low: `/horadric/hook` accepts web origin POSTs.** A page can loop
   random session ids, each spawning a `claude agents` listing.
9. **Low, cross user: host pipe impersonation.** The UI never checks the
   pipe server's owner before attaching (`pipe.rs:104`).
10. **Low: plain text history.** `state.json` and `journal.jsonl` hold agent
    replies, `-p` prompts and tab URLs in Roaming AppData.
11. **Low, unverified: CDP `Browser.grantPermissions`** may let an agent
    grant camera or microphone without the WebView2 prompt.

## The agents themselves

Everything above is about Horadric's own code. The agents it starts are
separate programs, and they run as you, with your full Windows rights. So
"Horadric cannot screenshot" is true of Horadric and not of what runs in
it:

- **An agent can capture the whole screen.** A few lines of PowerShell
  (`CopyFromScreen`, or `PrintWindow` for a covered window) picture the
  desktop and every other app's window. This is how the agent developing
  Horadric checks its dev builds, following `CLAUDE.md`.
- **Nothing asks first** when the agent runs with permissions bypassed.
  Shell commands, capture included, run without a prompt.
- **What the agent reads leaves the machine.** A screenshot or file the
  agent looks at goes to its model provider as part of the conversation.

This holds for an agent in any terminal, not only in Horadric. Horadric
does not add a capture path; it starts agents with your user rights, as a
terminal would. To rein it in: run agents without bypass mode so shell
commands need approval, or add deny rules for capture commands in the
agent's settings (easy to word around, so a speed bump, not a wall).

## Verified fine

Loopback only binding everywhere; MCP is stdio; host pipes are user only
with remote clients refused; web pages cannot reach the command paths;
the updater manifest is signature checked; Discord is opt in; no secrets
in any log; the clipboard is read only on paste; terminal scrollback stays
in host memory and is never written or exposed through MCP.

Each fix is a quest in `.horadric/tasks.md`.

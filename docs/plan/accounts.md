# Accounts

Asked for: several Claude Max subscriptions, logged in to all of them, and
a switch between them without logging in again, while the sessions run.

- **What a login is.** Claude Code keeps one. On Windows the token is in
  `.credentials.json` under `claudeAiOauth`, beside the MCP servers'
  tokens, and whose it is sits in `.claude.json` under `oauthAccount`.
  Nothing else cares who is logged in: hooks, settings and conversations
  are shared. So an account is that pair (`horadric_core::accounts`), and
  a switch writes another pair in and leaves the rest of both files as it
  was. `CLAUDE_CONFIG_DIR` per account was the other way, turned down: each
  account would need the hooks, and a conversation could not be resumed on
  another account.
- **Kept logins** are in `accounts.dat` beside `state.json`, sealed with
  DPAPI for this Windows user. Claude Code's own file is plain text, so
  this is no weaker than what it copies.
- **Watched, not asked.** The app reads the login at start and whenever
  either file changes and has held still for a tick, since a login writes
  them one after the other. A pair whose organizations disagree is a login
  caught half written and is not kept, or the old account would get the
  new one's token. A refreshed token replaces the kept one, and a `/login`
  typed into any session adds its account.
- **The Account row** sits under the settings in the usage window and
  names the account in use. A click lists every account kept, with its
  plan ("Max 20x"), Add account, and Forget for the others. Add account
  opens `claude auth login` in a console of its own (a login, not a
  session, so no pane), and once the new login lands the old one goes back,
  so adding stops nothing. The new one is a pick away.
- **One click away.** The tray menu has an Account list too, and
  Ctrl+Alt+K (settable under Keys) drops the Claude list at the pointer.
  Every list shows what each account has left, "62% session, 80% week
  left", so the pick is plain: the one in use from the limits heard now,
  the others from the limits they left with, a window whose reset has
  passed counted empty again. An account a limit stopped says "used up"
  until its reset.
- **Switching by itself** (asked for 2026-10-17), "Switch account when
  one runs out" under Sessions in Settings, off until the human turns it
  on. When a `StopFailure` with `rate_limit` stops a Claude Code session,
  the account in use is marked out until its full limit resets (an hour
  when the limits heard show none full) and the switch goes to the kept account with the
  most left of its fullest limit. A session a limit stopped is free to
  restart whatever was typed, since it can do nothing more on that
  account, so the switch never waits on it. Each one resumes with a
  prompt telling it to go on, passed on the command line as a quest's
  first prompt is, so it lands once the agent is ready. When every other
  account is out too, a toast says so and the sessions wait as before.
  Once for each stop: a session stopped again on the new account can
  switch again, never back to one marked out. Taking an account by hand
  clears its mark, and "every account has run out" is said once until one
  is taken.
- **The Version row** is the last one: this build's version, or "Update
  to <version>" once a check found a release. A click shows that
  release's notes with Update now, or checks for one and says what it
  found. No chevron, since it drops no list.
- **Switching** is the hot part. A running `claude` holds its token in
  memory, so every agent Horadric runs stops and resumes (`claude --resume
  <id>`), which keeps its conversation. Each stops once
  `Session::free_to_restart`: not mid turn, and nothing typed since its
  last prompt, which could be a draft. A question it waits on comes back
  with the resume. The files change only once every one has stopped: one
  left on the old login would write its token back when it refreshes it.
  Then all resume together, and the pane that had the keyboard gets it
  back. While it waits, the list says for how many turns, and Switch now
  stops them at once.
- **Written over.** Any `claude` that read `.claude.json` before the switch
  can write it back after with the old profile, Horadric's own `claude
  agents` included. For 15 seconds after a switch the login is put back if
  it is found changed.
- **Limits are per account.** The ones shown go with the account leaving
  and the arriving account's come back, so the window never shows one
  account's numbers under another's name.
- **A dev instance** switches only a Claude Code of its own, with
  `CLAUDE_CONFIG_DIR` set, never the real login.

Tested with a dev instance on its own port and `APPDATA`, a fake
`CLAUDE_CONFIG_DIR` holding two made-up logins, and a `claude.cmd`
standing in for the agent: the row named the account, a login written into
the files from outside was picked up, and picking the other account
stopped both sessions and resumed them on it, one process each, the
conversation of one resumed by id, the MCP tokens left alone. A session
made to work by a faked `UserPromptSubmit` held the switch after the idle
one had stopped, the list said "after 1 turn", and its `Stop` finished it.
Add account opened `claude auth login` and its browser page. Not tested:
a real second subscription, which needs its owner to log in. Two things to
watch on that first real switch: that `claude auth login` does not revoke
the login it replaces, and that a resumed session really runs on the new
account (`/status`).

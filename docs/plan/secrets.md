# Secrets

Asked for on 2026-10-11: projects that run on their own stop at
authentication. A quest that has to push to GitHub, call an API or deploy
needs a token, and today it either sits in a plain `.env` file, gets pasted
into the conversation, or the quest blocks until the human comes. A place
Horadric keeps them, and a way for agents to use them, makes Warriv's
rounds and errands go further without the human.

**The danger is the agent, not the disk.** Whatever text an agent sees it
can repeat: into a commit, a log, a web form, or to a page in the browser
pane whose text told it to. So the rule this section is built on: **an
agent uses a secret without ever reading it.** It names the secret, and
Horadric hands the value to the one program that needs it.

- **Where they are kept.** `secrets.dat` beside `accounts.dat`, sealed
  with DPAPI for this Windows user by the same `seal` and `unseal` as kept
  logins. Not Windows Credential Manager: any tool can list its entries,
  and a blob stops at 2560 bytes, which a service account's JSON key
  passes. A dev instance keeps its own file in its own app data and never
  reads the installed one's.
- **Whose a secret is.** Each belongs to one project, by its folder, or
  to every project ("Everywhere", for a token several share). Its name is
  an environment variable's name, `GITHUB_TOKEN`, upper case, digits and
  `_`. A project's own one wins over an Everywhere one of the same name.
- **Names may be in the repository, values never.** `"secrets":
  ["GITHUB_TOKEN", "CLOUDFLARE_API_TOKEN"]` in `.horadric/config.json`
  says what the project needs. It is safe to commit, it lets a fresh
  checkout on another machine say what is missing, and it is what the
  settings list offers to fill.
- **Using one.** `horadric secret run GITHUB_TOKEN -- gh pr create ...`,
  several names joined by commas. The CLI asks the running app for the
  values, starts the program with them in its environment and nothing
  else's, passes its exit code back, and pipes its output through a mask
  that writes `[GITHUB_TOKEN]` wherever a value would have shown. The mask
  works across the chunks output arrives in, so a value split over two
  reads is still caught.
- **Asked through the app, with a key.** The values come over the local
  port, not out of the file, because the approvals, the log and which
  project a session belongs to live in the app. The port is open to
  anything on the machine, a web page included (a page may post to
  localhost), so each session gets a random `HORADRIC_SECRET_KEY` in its
  environment when it starts, and the app answers only a request carrying
  a live session's id and its key. A session's key dies with it.
- **Not in the session's environment.** Putting every secret into the
  agent's environment at start was the simple way, turned down: one
  `echo $GITHUB_TOKEN` and it is in the transcript. There is no `secret
  get` that prints a value either.
- **Asking first.** The first time a project's session uses a secret, its
  tile raises like a permission question: "Use GITHUB_TOKEN for `gh pr
  create`?" with Once, Always for `gh`, and No. Always is kept per secret
  and per program, the command's first word, so allowing `gh` does not
  allow `curl`. A shell or an interpreter (`sh`, `bash`, `cmd`,
  `powershell`, `pwsh`, `python`, `node`) is only ever offered Once,
  since it can do anything with the value, printing it in a form the mask
  does not know included.
- **Unattended.** A secret already allowed for that program just works,
  so Warriv's rounds and armed errands run. One that would need a
  question does not wait on a tile nobody watches: `secret run` fails at
  once and says why, and the agent blocks its quest with `horadric quest
  blocked --on-secret GITHUB_TOKEN` (a new kind for Waits a blocked quest
  can name), which holds once the secret is kept and allowed. The row
  reads "needs GITHUB_TOKEN" and a click opens Project settings with the
  name filled in.
- **A missing one** fails the same way, with the line the agent needs:
  "GITHUB_TOKEN is not kept for this project. Ask the human to add it in
  Project settings, or block the quest with --on-secret GITHUB_TOKEN."
- **Adding one.** In Settings, Projects, a Secrets row lists the picked
  project's names, the Everywhere ones under them, and the names
  `config.json` asks for that are not kept yet. Add asks for the name and
  the value in a masked field (`field.rs` with the characters drawn as
  dots), pasting allowed. A value is never shown again: a kept secret has
  Replace, Forget, Allowed for (the programs, each with a cross) and when
  it was last used and by which session. From a terminal, `horadric secret
  set NAME [--everywhere]` reads the value without echoing it, for a value
  too long to paste into a field.
- **The log.** `secrets.log` beside it, a line a use: time, project,
  session, secret, program, and allowed, asked or refused. Never the value
  and never the whole command line, which may hold other things worth
  keeping out of a log.
- **Telling the agent.** A session in a project with secrets kept or
  asked for gets a few lines joined into its `--append-system-prompt`:
  the names (never the values), the `secret run` form, never to print,
  write or pass a secret on in any other way, and the `--on-secret` wait.
  The CLI is the same for Codex and Grok Build, since it is a program they
  run like any other.

**What it does not protect against.** DPAPI opens for any program running
as this user, so malware already on the machine can read `secrets.dat`
just as it can read `.credentials.json`. An agent that means to leak a
secret can, through a program it was allowed for that sends it somewhere.
The mask catches the plain value, not an encoded one. What this does buy:
no secrets in repositories or `.env` files, none in transcripts by
accident, one place to see and revoke them, a log of every use, and a
question before any new program gets one. The advice it gives the human
in the Secrets row: narrow tokens over passwords (a fine grained GitHub
token for one repository, an API key with one scope), so a leak costs
little and a revoke is a click on the provider's site.

**Later, not in the first cut.**

- **Git over HTTPS.** `horadric secret git` as a credential helper, so a
  push needs no `secret run` around it. It would go in the project's
  `.git/config`, never the global one.
- **Secrets as files.** `secret run --file KUBECONFIG ...` writes the
  value to a file only this user can read, puts its path in the variable,
  and deletes it when the program exits, for tools that want a path.
- **Web logins** stay in the browser pane's own profile. An agent that
  needs a site it is logged in to uses the pane, and never gets a
  password.

**Pure and tested:** a secret's name, the mask over chunked output, the
program a command line names and whether it is a shell, matching an
approval, which of a project's and an Everywhere secret wins, the names
`config.json` asks for, the log line, the prompt text, and `--on-secret`
written and read in a quest's wait.

**Verifying it.** With a dev instance on its own port and app data and a
made-up secret: `horadric secret run TEST_SECRET -- cmd /c echo
%TEST_SECRET%` from a session prints `[TEST_SECRET]` after Once, and a
request with no key or a dead session's key is refused. Checked on screen:
the question on the tile, Always for one program and not another, a
shell offered only Once, the masked field, and a quest blocked
`--on-secret` going on once the secret is added. Then with `horadric run`
on haiku, told to open a pull request on a scratch repository with a
fine grained token kept for it, and a check that the token is nowhere in
its transcript.

Order of work: the sealed file, `secret set` and `secret run` with the
mask, then the key and the questions on the tile, then the Secrets row in
Settings, then the prompt and `--on-secret`. The later items after that,
each when a project asks for it.

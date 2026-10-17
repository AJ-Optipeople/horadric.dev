# Identities

Asked for on 2026-10-17: the human's projects belong to three worlds,
Optipeople, personal and Pradsgaard Labs, each with its own GitHub,
Vercel and npm accounts. The command line tools keep one "current
account" each, for the whole machine, so a session that runs `gh auth
switch` for its project switches every other session too, and a quest
stops on a push that went to the wrong account or a deploy that landed
in the wrong team. Logging in is not the problem. Switching is.

**The idea: a project names an identity, and its sessions start with
that identity's tools.** Horadric starts every session, so it decides
each one's environment. An identity is a folder of the tools' own config
(their login files where they keep them), and a session's environment
points each tool at its identity's folder. The human logs in once per
identity and tool, and nobody switches again. Two sessions of two
identities run side by side and never see each other's login.

- **What an identity is.** A name ("Optipeople"), a short slug for its
  folder, and the tools it has set up. Kept in `identities.json` beside
  `state.json`, the tools' config under `identities\<slug>\` in the app
  data. A dev instance keeps its own, as with accounts and secrets.
- **Whose a project is.** Each project names one identity, or none.
  None is today's behaviour: the session gets the machine's global
  logins, so nothing changes for a project until the human picks. The
  pick is in Settings, Projects, a row of its own, and may also be
  written `"identity": "optipeople"` in `.horadric/config.json`, by
  slug, so a fresh checkout says whose it is. The setting wins over the
  file, since the file is shared with whoever else clones the repo.
- **By folder.** An identity may claim folders, `Documents\Github\opti\`,
  so a new project under one is that identity's from its first session.
  The longest claim wins. A project's own pick beats any claim.
- **A session's identity is fixed at its start.** A session host keeps
  the environment it was born with, so changing a project's identity
  applies to its next sessions. The Projects row says how many running
  sessions still have the old one, with Restart them.

**The tools, one at a time.** Each tool has its own way to be pointed at
a folder, and an identity only sets what it has a login for. What each
reads was checked on 2026-10-17.

- **GitHub CLI.** `GH_CONFIG_DIR=<identity>\gh`. Its `hosts.yml` names
  the active user, and the token stays in Windows Credential Manager,
  keyed by host and user, where `gh` puts it now. A `gh auth switch` run
  inside a session then only switches that identity.
- **git.** The name, the email and which account the credential helper
  uses, set through `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>` and
  `GIT_CONFIG_VALUE_<n>` (git 2.31 and later): `user.name`,
  `user.email`, and `credential.https://github.com.username`, which tells
  Git Credential Manager which of its kept GitHub accounts to answer
  with. Nothing is written into any `.gitconfig`, global or the
  repository's, so the human's own terminals outside Horadric are as
  they were. The commit author is the part that matters most: a commit
  on an Optipeople repository should never carry the personal email.
- **npm.** `NPM_CONFIG_USERCONFIG=<identity>\npm\.npmrc`, its registry
  tokens inside, plain text as npm keeps them anywhere. A project's own
  `.npmrc` still applies on top, as npm means it to.
- **Vercel.** The CLI has no variable for its folder. It looks for
  `auth.json` in its XDG data folder, `--global-config` aside, so the
  identity sets `XDG_DATA_HOME=<identity>\xdg-data`. That moves every
  XDG minded tool's data in that session too, which is why it is set only
  for an identity with a Vercel login. The other way, a `VERCEL_TOKEN`
  for the identity under Secrets, works for every command that takes a
  token and is offered beside it. Which team a project deploys to is
  already in its `.vercel\project.json`; the identity only says who is
  logged in.
- **Claude Code is not one of them.** Accounts (see
  [accounts.md](accounts.md)) switches the Claude login for every
  session on purpose: a usage limit is spent per subscription, not per
  project, and `CLAUDE_CONFIG_DIR` per account was turned down there.
  An identity may later say which account it prefers, never force one.
- **More tools** go in as a project asks for them, each one line of what
  variable points it where: `AZURE_CONFIG_DIR` for the Azure CLI,
  `CLOUDSDK_CONFIG` for gcloud, `AWS_PROFILE` for AWS, `CLOUDFLARE_API_TOKEN`
  for wrangler (a secret, so under Secrets). A tool with no variable is
  left global, said so in the row, never hacked by copying files around.

**Logging in.** Each identity's row in Settings lists its tools with who
is logged in to each. Log in opens the tool's own login in a console of
its own, with the identity's environment: `gh auth login`, `npm login`,
`vercel login`, as Add account does for Claude. For git there is no
login, only the name, the email and the GitHub user to pick from the
ones Git Credential Manager keeps. Who is logged in comes from the files
where a tool keeps it (`hosts.yml`, the `.npmrc`'s registries, Vercel's
`auth.json` holds no name so `vercel whoami` is run on a click), never
from asking a tool every paint.

**Telling the agent.** A session with an identity gets a few lines
joined into its `--append-system-prompt`: whose project this is, who it
is logged in as for each tool, never to run a tool's login, logout or
switch, and what to do when a tool says it is not logged in: block the
quest with `horadric quest blocked --on-login gh`, a new kind for
[Waits a blocked quest can name](waits-a-blocked-quest-can-name.md),
which holds once that identity has a `gh` login. The row reads "needs gh
login for Optipeople" and a click opens the identity's row in Settings.
Same for Codex and Grok Build: the variables are in their environment
like any program's.

**Plain terminals and the browser pane.** A plain terminal in the stage
(see [plain-terminals.md](plain-terminals.md)) gets its project's
identity too, so the human's own `git push` there goes as the agent's
would. The browser pane keeps its own login profile; an identity
does not change that in the first cut, since web logins are the sites'
own business. A profile per identity is the obvious later step.

**With Secrets.** A secret may belong to an identity as well as to one
project or to Everywhere (see [secrets.md](secrets.md)), so Pradsgaard
Labs' `CLOUDFLARE_API_TOKEN` serves every Pradsgaard project without
being added to each. A project's own wins over its identity's, which
wins over Everywhere. The two parts do not depend on each other: either
can be built first.

**What it does not do.** It does not keep a tool's login safer than the
tool does: `gh`'s token is in Credential Manager either way and npm's in
a plain file either way, just a different file. An agent can still read
what its identity's folder holds, as it can read the global one today.
What it buys is that the login an agent finds is the right one, and that
one project's switch never stops another's quest.

**Pure and tested:** an identity's slug, which identity a project gets
(its pick, the config file, the longest folder claim, none), the
environment an identity sets for the tools it has, the `GIT_CONFIG_*`
entries, reading the active user from `hosts.yml` and the registries
from an `.npmrc`, the prompt lines, and `--on-login` written and read in
a quest's wait.

**Verifying it.** On a dev instance with two identities made for the
test, each with a different `gh` login and git email: two sessions of
two projects, one of each, run `gh api user -q .login` and `git config
user.email` and each answers with its own, and `gh auth switch` in one
leaves the other's answer as it was. `horadric run` on haiku, told to
commit and push to a scratch repository, and the commit's author and the
pushing account checked on GitHub. Checked on screen: the Projects row,
Log in opening a console that lands in the identity's folder, and a
quest blocked `--on-login` going on once the login is there.

Order of work: identities and the project pick with `gh` and git, since
those are the walls hit most; then npm and Vercel; then the folder
claims, the prompt lines and `--on-login`; then secrets owned by an
identity once Secrets is built.

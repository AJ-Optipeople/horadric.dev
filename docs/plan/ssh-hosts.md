# SSH hosts

A project's servers, at hand from the project: a terminal on the VPS a
click away, and agents that know the servers exist and work on them. Asked
for because a lot of what a project runs on lives on a VPS, and telling the
agent what to do there should be enough.

- **The agent stays local.** A session runs on this machine as always and
  reaches a server with `ssh myvps '<command>'` from its own shell tool. It
  has the code and the server in one place, so it can fix a bug, deploy it
  and read the logs after. Running `claude` on the server instead was ruled
  out: its hooks would post to the server's own localhost, which needs a
  reverse tunnel per connection, and every server would need Claude
  installed and logged in.
- **Horadric does not speak SSH.** It runs the `ssh` that ships with
  Windows, which reads `~/.ssh/config` for aliases, users, ports and keys.
  A library would be a dependency for what one executable already does,
  the same reasoning as the VT parser. Keys and passwords never pass
  through Horadric.
- **A project's hosts** are a list in `.horadric/config.json`, the
  `config.json` of step 4 that the task list needs too: `"hosts":
  ["myvps", "deploy@203.0.113.7"]`, each anything `ssh` takes as a
  destination. An alias is best, since it keeps users and ports out of
  the repo. "Add host" in the project menu asks for one, as the rename
  dialog does, and offers the `Host` names in `~/.ssh/config` that have
  no wildcard.
- **The SSH terminal** is a plain terminal (see Plain terminals) whose
  program is `ssh <host>`. "SSH to myvps" in the project menu, one entry a
  host. It gets a tile, a pane and a place in the order like any shell, a
  glyph of its own, and the host as its second line until the remote shell
  sets a title. An exit with code 0 closes it, as a shell does. Code 255 is
  `ssh` failing (connection refused, dropped, key refused), so the pane
  stays with the error in view and the tile goes paused; a click
  reconnects. After a restart it comes back paused, as a shell does.
- **Telling the agent.** Every session in a project with hosts gets them
  through `--append-system-prompt` when it starts or resumes: which hosts
  belong to the project, to reach them with `ssh <host> '<command>'`, and
  to pass `-o BatchMode=yes`, so a host that wants a password or a
  passphrase fails at once instead of hanging on a prompt the agent cannot
  answer. With the task list's prompt the two are joined into one. A
  resume takes the list as it is then, like the defaults.
- **Permissions are the user's.** A command on a server goes through the
  permission mode picked in the usage window like any other command. With
  bypass permissions on, the agent runs remote commands without asking,
  because that is what the user chose. Horadric adds no gate of its own.
- **Keys the agent can use.** An agent's shell is not interactive, so the
  key has to work without typing: no passphrase, or one held by an agent
  its `ssh` can reach. Git Bash, which Claude Code uses on Windows, brings
  its own `ssh` that does not talk to the Windows `ssh-agent` service.
  Worth checking on screen which one the agent gets, and saying in the
  prompt which `ssh` to run if it matters.
- **Pure and tested:** reading `hosts` from `config.json`, the `Host`
  names out of an `ssh_config` (wildcards and `Match` blocks skipped), the
  command line for an SSH terminal, the prompt text, and the exit code to
  close or keep.
- **Verifying it.** On screen against a real server with a harmless
  command (`uptime`), and against `localhost` if the Windows OpenSSH server
  is installed. The agent side with `horadric run -- -p "Run uptime on
  myvps" --model claude-haiku-4-5-20251001` in a project with a host.

Done: hosts in `config.json` (`horadric_core::ssh`, which leaves out a
host that starts with `-` or holds a space, since the file comes with the
repository, and passes the host after `--`), and "SSH to <host>" in the
project menu. An SSH terminal is a shell with `Session::ssh` set, named
SSH, SSH 2 and so on, running `System32\OpenSSH\ssh.exe` before any `ssh`
on `PATH`. Tested on screen with a dev instance against `localhost` with
no SSH server: the menu listed the host once and left out a
`-oProxyCommand` one, the refusal stayed in the pane with the tile paused
and `localhost` as its second line, a click reconnected with one
`ssh.exe`, and a restart brought it back paused with its host. Not tested
on screen: a session that connects and exits 0, since this machine has no
server to reach; it takes the path a shell's exit takes.

Done: the prompt for agents (`ssh::system_prompt`). Every Claude Code
session the app starts or resumes in a project with hosts gets it, joined
with the task list's into one `--append-system-prompt`, and so does one
from `horadric run`. Checked on screen: the agent's Git Bash runs its own
`/usr/bin/ssh`, which cannot reach the Windows `ssh-agent` pipe, so the
prompt names the Windows `ssh.exe` by its full path with forward slashes,
the same one the SSH terminal runs. Verified with `horadric run -- -p "Run
uptime on localhost"` on haiku: it ran
`C:/WINDOWS/System32/OpenSSH/ssh.exe -o BatchMode=yes localhost 'uptime'`
and reported the refusal, and a dev instance's `claude.exe` carried the
prompt on its command line. Not tested: a host that answers, for want of
one.

Order of work: hosts in `config.json` and the SSH terminal from the
project menu, then the prompt for agents, then "Add host" with the
suggestions from `~/.ssh/config`.

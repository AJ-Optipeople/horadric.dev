# Sessions that outlive Horadric

Today every pseudo console is created by the Horadric process. When that
process ends, its `HPCON`s close, conhost goes, and every agent with it.
Persistence and `reload` make the restart cheap (a click, or nothing, and
`claude --resume` brings the conversation back), but a restart still costs:
the turn in flight is cut off, tools the agent was running die, the
scrollback is gone, and `reload` has to wait for every session to be idle.
A crash used to be worse than a reload, since the sessions came back
paused. Option A, below, is built: they come back running.

The console can only outlive the UI if a process that is not the UI created
it. An `HPCON` is not a kernel handle and can not be handed to another
process, so there is no way to move a console after the fact. Whatever
survives has to own it from the start.

**Option A: stay in process, resume after a crash.** Built, see "After a
crash" under the reload. No new process. Mark
`running` in `state.json` all the time, not only on `reload`, and on a start
after an unclean exit resume those sessions as `app --reload` does. Small,
maybe a day. Covers the conversation, not the turn in flight, the tools or
the scrollback. Worth doing whatever else is chosen, since logoff and
restart end every process anyway.

**Option B: one host process per session.** `horadric host` is a small
process that creates the console, starts the agent, and serves one named
pipe, `\.\pipe\horadric-<instance>-<session id>`, with a DACL for the
current user only. The UI connects to it and sends input and resizes; the
host sends output and the exit code. The host keeps the last few MB of raw
output in a ring, and a UI that attaches replays the ring into a fresh
`Term`, then forces a resize so a full screen program like Claude Code
redraws. The host does not parse anything, so it has no `alacritty_terminal`
and almost no reason to change between builds. A host crash takes one
session; a UI crash takes none. Cost: one more process a session, a few MB
each, about 150 MB for forty.

**Option C: one broker for all sessions.** The same, but one process holds
every console, like a tmux server. Fewer processes and one pipe. A bug in
the broker ends every session at once, which is the thing we are trying to
get away from, and the broker can never be restarted without ending them
all, so every change to it is the same problem again one layer down.

**Option D: an existing multiplexer.** tmux in WSL, or similar. Rejected:
it would put the agents in Linux, not Windows, and it adds a toolkit
between us and the terminal, which is the settled decision.

**Decided 2026-09-25: A first, then B**, with Quit as proposed below. A is cheap and still needed for logoff
and restart. B isolates the failure where it happens and keeps the part that
must never change tiny. Things B has to settle when it is built:

- **Quit.** Tray Quit could leave the hosts running (sessions go on, tiles
  come back on the next start) or end them as today. Proposed: Quit asks,
  with "keep running" as the default when any session is mid turn.
  Decided.
- **Reload** stops waiting for idle sessions: the new build attaches to the
  same hosts. The UI and the host speak a versioned protocol, and a new UI
  must understand every host version still running, since hosts from old
  builds live on until their session ends. Keep the protocol to five
  messages (input, resize, output, exit, kill) so that stays easy.
- **Hooks while no UI runs** go nowhere, since they are HTTP posts to the
  port. On attach, the phase comes from the transcript tail as it does for
  the title, and is right again at the next event. Buffering in the host
  would mean the host listens on the port, which is not worth it.
- **Orphans.** A host with no UI and an exited agent ends itself. A host
  whose agent is still running stays, and `horadric` with no app running
  lists them, so a session never runs where nobody can see it.
- **Jobs.** The host is started with `CREATE_BREAKAWAY_FROM_JOB` and
  detached, so it outlives the UI even when the UI itself runs in a job, as
  it does when started from inside Claude Code. The job that traces windows
  back to a session moves into the host, which answers "is this process
  yours" over the pipe.
- **Dev instances** name their pipes with their own instance, so a dev UI
  never attaches to an installed session.

Sizing: A is a day. B is about a week: the host and its pipe server, the
UI side replacing `Pty` in `Console` behind the same four calls, attaching
on start, the replay, and on screen tests of kill the UI, start it again,
and find every session where it was.

**Option B is built**, as settled above, with these details:

- **`horadric host`** reads a JSON `Spec` (the command and the pipe name)
  on stdin, takes the first instance of its pipe with
  `FILE_FLAG_FIRST_PIPE_INSTANCE` (so a second host for the same session
  fails before it starts a second agent), starts the program in a pseudo
  console, and serves until the program ends. A startup error goes to
  stderr, which the UI reads and shows as it did before.
- **The pipe** is `\\.\pipe\horadric-<port>-<session id>`, the port
  standing for the instance, so a dev instance never attaches to the
  installed one's sessions. Its DACL names the current user's SID and
  nobody else, and it refuses remote clients. Every handle is overlapped:
  a synchronous handle runs one call at a time, and the reader sitting in
  a read would hold up every write.
- **The protocol**, `horadric_pty::wire`: a hello (`HRDH` and a version
  byte, 1 today), then frames of a kind byte, a length and a body. Five
  kinds: input, resize, output, exit, kill. Unknown kinds are skipped. The
  host sends its console size first, as a resize, then the replay, so the
  UI builds its grid at the size the replay was drawn at.
- **The ring** is the last 4 MB of output. Once it has dropped bytes the
  replay starts after the first line break, out of whatever escape
  sequence it began in. On every attach after the first the host resizes
  the console one row smaller and back, which makes ConPTY repaint and
  Claude Code redraw.
- **One client, the newest.** A UI that connects while another holds the
  pipe takes it over, as during a reload. The old one is disconnected.
- **Exit.** The host sends every byte of output, then the exit code, waits
  up to 5 seconds for the UI to read it, and ends. With no UI connected it
  ends at once. A host that disappears without an exit reads as
  `HOST_LOST`, a non zero code, so the session pauses and a click resumes
  it.
- **The binary** hosts run from is a copy of the UI's,
  `horadric-host-<size>-<mtime>.exe` in `%LOCALAPPDATA%\Horadric\hosts`
  (`Horadric-dev` for a dev instance). A long lived host running
  `horadric.exe` itself would stop `swap` from moving it aside a second
  time and lock `target\debug` against the next build. Copies no host
  runs from are deleted when a new build makes its own. The status line
  runs from the copy too. Task Manager shows the name, which tells a host
  from the UI.
- **Jobs.** The host is started with `DETACHED_PROCESS`,
  `CREATE_NEW_PROCESS_GROUP` and `CREATE_BREAKAWAY_FROM_JOB`, and without
  the last when the UI's job refuses it. The session's own job is named
  `Local\horadric-<port>-<id>`; the UI opens it for query and asks
  `IsProcessInJob` itself, so "is this window's process yours" needed no
  message of its own. Session jobs allow breakaway, so a dev Horadric
  started from a session's terminal gets hosts that outlive that terminal.
- **Attaching.** On every start, before resuming anything, the app lists
  the pipes with its prefix and attaches to each one that is a saved
  session. Its phase comes from the transcript's last user or assistant
  record (`title::mid_turn`): working after a prompt, a tool call or a
  tool's result, idle after a reply that ended the turn or an interrupt.
  Resuming after a crash (option A) then skips those sessions, since they
  are no longer paused, so A only resumes sessions whose host is gone.
- **Orphans.** `horadric` with no app running prints the sessions that ran
  on without it, then starts the app, which attaches to them.
- **Quit** asks "keep them going without it?" with Yes, No and Cancel.
  Enter is Yes when a session is mid turn, No otherwise. No kills every
  session and waits up to 3 seconds for the hosts to confirm, since the
  kills leave on threads that end with the process.
- **Reload** hands over at once. An installed Horadric from before hosts
  still waits for idle sessions and ends them on the way out, so the first
  reload into this build resumes them as before; from then on they run
  through.

Tested with a dev instance, three `cmd.exe` sessions (one printing a line
a second) and one real `claude`: the dev UI killed four times and started
again, then `reload`. Every session came back each time with its screen
replayed and the counter further on, the `claude` with the same pid, two
`claude.exe` on the machine (the agent doing the test, and that one).
Killing one host paused only its tile. Found on the way: the `windows`
crate turns an error into an `io::Error` holding the HRESULT, so a missing
pipe never read as `NotFound` and the UI gave up on a host it had just
started; `pipe::os` converts it back. Not seen on screen: the Quit
question itself, since a message box would have taken the screen from
the human.

# The task list

A list of work per project that agents take items from, one at a time or
all the way down by themselves. Asked for because every feature or fix
meant starting a terminal by hand and pasting the story in. Linear and the
like were ruled out as too much: this is a text file and a tile, not a
tracker.

- **The list is a file in the repo**, `.horadric/tasks.md`, beside
  `.horadric/config.json`, which holds the mode (`{"tasks": {"mode":
  "auto"}}`) and will hold step 4's settings and the SSH hosts. The human
  edits the list in VS Code, agents read and write it like any other file,
  and git keeps its history. Nothing about it is in `state.json` but
  whether the tile is folded. Committing it is the project's choice.
- **The format** is a Markdown checklist, and the file order is the work
  order. Moving a line is how priority changes. Indented lines under an
  item are its notes and go to the agent with it.

  ```
  - [x] Rename Glance to Horadric
  - [/] Fix the login redirect @fix-the-login-redirect-51234
    Happens only after a session expires. Repro in #12.
  - [?] Add dark mode to the settings page @add-dark-mode-51300
  - [!] Migrate to the new API @migrate-api-51400: needs a key I do not have
  - [ ] Show the build time in the footer
  ```

  `[ ]` open, `[/]` being worked on, `[?]` done by the agent and waiting
  for review, `[!]` blocked with a reason, `[x]` done. `@id` is the
  Horadric id of the session that holds it, so the file alone says who has
  what and a restart loses nothing. Only lines starting at the left edge
  with `- [` or `* [` are items; headings, prose and nested lists are left
  out. An `@` inside the title stays in the title: only the last ` @id` at
  the end, or before a colon, is a holder. No dependencies, labels,
  estimates or assignees.
- **Who writes what** (`horadric_core::tasks`, pure and tested). Horadric
  changes the marker and the `@id`, one line at a time: read the file,
  change that line, write it back through a temporary file and a rename
  (`horadric_hooks::tasks::update`), every other byte as it was, CRLF
  included. A change finds its item by line and title together, or by
  holder, and does nothing when the file moved under it. Everything else
  belongs to the human and the agents.
- **Taking an item** (`runner.rs`, `App::take_task`). A click on an open
  row writes `[/] @id` into the file first, then starts the session, named
  after the item, with the item, its notes and one line on how to report
  as its first prompt. The file is the lock: a list read a moment later
  already says the item is taken. `--append-system-prompt` tells the agent
  it works one item of the list, to commit when finished, how to report
  (`task done`, `task blocked "why"`), to file other work with `task add`
  rather than do it, and the file's format, so a planning item can write
  its own items below itself. It goes on every start and resume while the
  session holds an item; the first prompt only on the first start, never
  saved with the session's arguments, or a resume would send it again.
  A failed start puts the item back as open.
- **The first prompt says how to report too.** Tested with Haiku: given
  only the system prompt it answered the item and never ran `task done`.
  With one line at the end of the prompt, `(An item from
  .horadric/tasks.md. When it is finished, run `.../horadric.exe task
  done`.)`, it ran it. The human sees that line in the terminal, which is
  no loss.
- **`horadric task done|blocked WHY|add TITLE|list`** (`task.rs`). The
  command changes the file itself rather than asking the app, so the agent
  hears at once whether it worked: it walks up from its folder to the
  list holding an item for `HORADRIC_SESSION`, marks it `[?]`, or `[x]` in
  auto mode, or `[!]` with the reason, then posts to `/horadric/tasks` on
  `HORADRIC_OWNER_PORT` so the app looks at once. The agent runs this
  build by its full path with forward slashes (`runner::command_for`),
  since the `horadric` on `PATH` may be another build and Git Bash and
  PowerShell both take it.
- **Why the agent reports done.** `Stop` only means a turn ended. It is
  just as often a question as a finished job, so the hooks cannot tell the
  two apart. The agent can.
- **Three modes per project**, from the button in the tile's header:
  - *Manual.* Nothing starts by itself. Click items to take them. Done
    goes to review.
  - *Review.* Horadric takes the first open item. When the agent reports
    done, the item goes `[?]`, the row shows an approve button and a
    notification says so. Approve and the item goes `[x]`, the session
    closes and the next item starts. Or type into the session what is
    wrong; the agent goes on and reports done again.
  - *Auto.* The same without the gate: done goes straight to `[x]`, and
    the list is worked until it is empty. Then a notification says so.
- **One fresh session per item.** Carrying one session down the list
  would fill its context with the items before. The commit the agent made
  is what the next one builds on. A session whose item is `[x]` closes
  once it is not mid turn, since the agent reports from inside its turn.
- **When the runner stops.** At a blocked item that waits on the human
  (one that names a wait it can check is passed, see "Waits a blocked
  quest can name" below), since the order is the
  order and the next may need this one; an item added meanwhile waits
  behind it. At an item whose session is gone. At a paused session, after
  a restart: the runner never resumes one by itself, a click on its row
  or tile does. At a permission prompt, like any session, so how far auto
  mode gets alone depends on the permission mode in the usage window.
- **When a usage limit runs out** the runner waits instead of stopping
  (`Limits::out_until`, pure and tested). While the numbers the status
  line last gave say a limit is at 100 %, nothing starts until a minute
  past its reset, the latest reset if several are full. A session on an
  item that the limit refuses mid turn (a `StopFailure` with
  `rate_limit`) gets no nudge; the runner notes when the fullest limit
  resets, holds every list until then, and a minute after types into it
  once that the limit has reset and to go on with the item
  (`tasks::go_on`). With no reset known it stays for the human. A
  notification says when the list goes on. Only typed into a running
  terminal, so it never starts an agent. Tested with a dev instance and
  faked limits: a full limit resetting in 20 s held the first start for
  80 s, and a refused session was told to go on 70 s after a limit at
  97 % resetting in 10 s, once.
- **The nudge.** A session in review or auto mode whose turn ends without
  a report is asked once, typed into its terminal, whether it is finished,
  with the command spelled out. The Enter follows 400 ms later, so the
  input box takes the text as typed and not as a paste with a newline in
  it. If it stops again after that without a report, the row reads "asks
  you" and a notification says it needs you. Asked for by the user once
  the plan left it open.
- **Fuses.** The runner is the one part of Horadric that starts agents by
  itself, which is what ran away once. It holds one item per project, or
  as many as `parallel` says and never more than 8, it starts at most one
  session per project every 10 seconds, it never resumes a paused session
  and starts nothing beside one, a start writes the file before it launches
  and checks the item is still open in a fresh read, and `launch` still
  refuses a second process for a session.
- **Notifications**, once each while they hold: an item ready for review,
  one blocked (with the reason), one whose session stopped after the
  nudge, and a list finished by the runner.
- **The tasks tile** (`board.rs` for what a row says, pure and tested;
  `layout::TasksLayout`; `Painter::tasks`). It sits between the plus and
  the files tile, on a screen like the files: a chevron and TASKS, a
  summary ("2 of 5 done"), the mode as a small key that opens a menu, and
  a plus that asks for a new item with the rename dialog. A row per item
  not done, eight before the wheel scrolls: a glyph and a word in the
  colour its session's lamp burns in (working blue, asks you and review
  amber, blocked red, paused and session gone dim). A click on an open row
  takes it, on a row whose session is gone starts it again, on any other
  shows its session. Right click: start, show session, approve or mark
  done, put back in the list (which ends its session), edit the list. Its
  height is fixed, counted like the plus row; only the files tile flexes.
  The fold is kept as `tasks_collapsed` per cluster.
- **A list keeps its project up.** A project in the recent list whose
  list has an item not done gets its cluster with no session in it, so
  the list shows and its runner runs (`tasks::unfinished`,
  `App::listed`). Once every item is done and no session is left, the
  cluster goes as it did before. Only recent projects, the eight the
  tray offers, since those are the folders Horadric knows. Tested on
  screen: a recent folder with two open items came up as a cluster with
  only the tasks tile, marking both done took it away and an added item
  brought it back within a second, auto mode started one `cmd.exe` on
  it, and `task done` closed that session and the cluster.
- **Reading the list.** Every second the app compares each project's list
  and config with when they last changed (`fs::metadata`, two calls a
  project) and reads again only what changed, so an edit in VS Code shows
  within a second. Not the files tile's `ReadDirectoryChangesW` watcher:
  that one only runs in git projects, and ignores what git ignores, which
  a `.horadric` folder may well be. Horadric's own writes and `horadric
  task` read at once.

Tested on screen with a dev instance on its own port and `APPDATA`, first
with `HORADRIC_AGENT=cmd.exe` and `horadric task` typed by hand: a click on
a row wrote `[/] @id` and started one `cmd.exe`; `task done` from a
subfolder marked it `[?]` with a notification; the approve button marked it
`[x]` and closed the session. Auto mode, switched by writing the config,
started the next item within a second, closed each finished session and
started the next, never more than one agent for the list. `task blocked`
showed a red row and stopped the runner, with an item added behind it left
waiting; `task done` on it went on. A fake `Stop` got the nudge typed and
entered in the terminal, and only the stop after it the "needs you"
notification. After a restart the held item read "paused", nothing
started, and a click on the row resumed it. The plus asked for a title and
review mode started the new item at once. Then with a real `claude` on
Haiku in review mode: the runner started it, it ran `task done` after its
permission prompt, the row went to review with a notification, and
approving closed the session and said the list was done. Not tested on
screen: the right click menu and the mode menu, which a synthetic click
opens behind other windows.

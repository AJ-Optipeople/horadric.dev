# Waits a blocked quest can name

Asked for on 2026-10-01: a `[!]` quest stopped the whole list until the
human came, even when what it waited on was another quest that the list
would get to anyway.

- **The form.** `horadric quest blocked "why" --on-quest "title"`, or
  `--on-main <commit or branch>`, `--on-file <path>`, `--on-cmd
  "<command>"`, `--until <+30m, Unix seconds or 2026-10-01T14:05Z>`. With
  a wait the why may be left out. The wait goes at the end of the item's
  line in braces, `- [!] Wire it @wire-1: needs it {on quest: Build it}`,
  so the file stays the state and a human can write one too
  (`tasks::Wait`, pure and tested). A time is kept in UTC, so the file
  means one moment wherever it is read.
- **What holds.** A quest of that title, any case, marked `[x]` in the
  same log. A ref that `git merge-base --is-ancestor <ref> HEAD` passes in
  the main tree, so a branch with nothing new on it counts as merged. A
  file from the project folder. A command `cmd.exe /d /c` runs in the
  project folder exiting 0 within 30 seconds. The list and the clock are
  asked on every look; git and a command run on a thread at most every 15
  seconds, and until one has answered the wait is not over.
- **The runner** (`tasks::next`, tested) passes a blocked quest with a
  wait that does not hold yet and starts the next open one. A quest
  blocked with only a why still stops the list there, since that needs the
  human. A list with nothing left but waits is not finished. Once a wait
  holds the quest takes a place like a start (`Next::Resume`), in list
  order, within `parallel`, at most one start per project every 10
  seconds. Its session, if there and between turns, gets `[/]` back with
  its holder and is told what happened and to go on (`tasks::waited`),
  typed in like the usage limit's go on. A session that is gone, ended, or
  whose terminal exited, starts the quest again as a click on a gone row
  does. A paused session, after a restart or a crash, waits for a click,
  as every paused session does. Only in review and auto mode: manual
  starts nothing. A toast says the quest goes on; a waiting quest raises
  no "Blocked" toast, since it needs nobody.
- **The row** reads in the idle colour with a clock and says what it
  waits on, "after Build it", "for done.txt", "in 25 min"
  (`Wait::label`, `board::note`).
- **The system prompt** spells out the flags and asks the agent to use one
  whenever it fits.

Checked on screen with a dev instance on its own port and app data and
`cmd.exe` as the agent, in auto mode: of two quests, the runner started
the first, which was blocked on the second with `--on-quest`; the row read
"after Build the engine" and the runner started the second. `quest done`
on it marked it `[x]`, closed its session, put the first back to `[/]`
with its own session and showed "Quest goes on". A quest held by a session
that no longer exists, waiting on a file that was there, started again in
a new session. One `cmd.exe` at any time. A killed terminal read as
paused, so its quest waited for a click. Not checked on screen: `--on-cmd`,
`--on-main` and `--until`.

Built as the quest asked, with every kind of wait, before Orchestration
below was written. If its `After:` lines are built, `--on-quest` should
write one of those instead of `{on quest: ...}`, and `tasks::next`
already passes a quest that waits and wakes it in the same place.

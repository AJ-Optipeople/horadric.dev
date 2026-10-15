# Orchestration

Asked for on 2026-10-01, decided on 2026-10-05 (see the end of this
section). Every step is built (2026-10-05). The goal is development
that runs itself across many sessions, with the human hearing only
about what needs a human. Today the human is the orchestrator: with
several sessions in a project, a quest that cannot go on because it
waits on another sits `[!]` until the human notices, works out what it
waits on and starts it again. "Blocked quests resume by themselves" in
the quest log fixes part of that. It is not the whole answer, and this
section says what is.

**Why a blocked quest is the wrong place to start.** When quest B waits
on quest A, the dependency was there before either started. B finds out
halfway in, after a session and some of its context are spent. The
Runetome tile is the example on our own log: it was taken, worked until
it saw that two quests above it had not landed, and blocked. Resuming
it once they land is a good safety net. Not starting it until they land
is the fix. So the work splits in three layers, each useful alone, built
in this order.

**1. Dependencies in the quest log, declared before work starts.**

This reopens "No dependencies" in The task list. The human said yes on
2026-10-05. **Built on 2026-10-05**, as below, with three things the
plan did not say:

- `quest blocked "why" --on "title"` writes `{after}` at the end of the
  blocked line beside the `After:` line. Without it a quest blocked on
  the human that has `After:` lines from planning, all done, reads the
  same as one blocked until its quest is done, and would be resumed.
  `{on quest: ...}` and `--on-quest` still read as before and mean the
  same as `--on`.
- The waits on a file, a command, `main` and a time stay: they were
  built and work, and dropping them would break lists that use them.
- A name that matches nothing or several, and a cycle, read "tangled"
  in red on the row with the name, and a toast says what is wrong,
  since no quest finishing frees them. A click on a quest that waits
  still briefs and starts it: the human may overrule the order.

- **The form.** A notes line `After: <title>` names a quest this one
  waits for, one line each. The title is matched exactly or by a unique
  start, since titles are long and carry colons. A name that matches
  nothing or several quests makes the quest not ready, and its row says
  which name, so a typo never starts work early. The file stays the
  state; nothing is kept beside it.
- **Ready.** A quest is ready when every quest it names is `[x]`. Not
  `[?]`: in review the work may not be on `main` yet, and with worktrees
  it is not.
- **The runner picks the first ready open quest**, not the first open
  one. A quest waiting on another does not stop the list, it is passed
  over, which is what `parallel` needs: with three slots and a chain of
  three, one runs and the other slots take quests that do not wait. A
  cycle is not ready either; its rows say so and the list stops there,
  since only a human can break it.
- **Who writes them.** The quest giver's prompt says to add `After:`
  lines when a suggested quest needs another, and a planning quest that
  writes its own quests below itself does the same. `quest add "title"
  --after "other"` writes the line.
- **"Blocked quests resume", cut down.** `quest blocked "why" --on
  "title"` marks the quest `[!]` and writes the same `After:` line. When
  that quest is `[x]`: a live holding session is told to go on, typed
  in as `go_on` does after a usage limit, and the quest goes `[/]`; a
  session that is gone starts again (`start_again`). One rule with two
  ways in. Waits on a file, a command or a time are left out until a
  quest needs one. A plain `blocked "why"` still stops the list for the
  human.
- **The board row** of a quest that waits reads "after <title>", dim,
  with no lamp, since nothing runs.
- **Pure and tested** in `horadric_core::tasks`: reading `After:` lines,
  matching titles, ready, cycles, the runner's choice, the `--on` form.
  On screen with a dev instance and `cmd.exe`: two quests, the second
  after the first, auto mode, `quest done` on the first starts the
  second. Count `claude.exe` after, since this starts agents.

**2. An orchestrator session, woken by events.**

Some blocks no rule can clear: an agent asks a question another agent
could answer, two quests turn out to overlap, a merge conflicts, a
quest is too big and should be split. Today each of those goes to the
human. Most need judgment, not a human in particular. That judgment
lives in an agent; Horadric stays the plumbing that wakes it, which
keeps the runner small and testable and keeps state coming from hooks
and the file.

- **One per project, named Warriv** (the caravan master in the games,
  who moves the camp on when the way is clear), a session on the stage
  like the quest giver. It holds no quest and the runner never gives it
  one.
- **Woken by events, not left running.** A quest goes `[!]` without
  `--on`; a session on a quest reads "asks you" after the nudge; a quest
  names a dependency that matches nothing, or a cycle; a merge into
  `main` fails; the log runs out of ready quests in auto mode while some
  wait. Not every `Stop`: that would make it a second runner.
- **A fresh session per wake**, for the reason the runner gives each
  quest one: carrying a session from event to event fills its context.
  Its first prompt is the event, the quest, the reason and the holding
  session's last turn. Its memory is the quest log: it writes what it
  decided as a notes line on the quest (`Warriv: split into the two
  below`), so the next wake reads it. Events that arrive while it works
  wait and go to it at its next `Stop` in one prompt. When it has no
  event left and is not mid turn, it closes.
- **What it may do.** Change the log (`quest add`, `--after`, notes,
  move, put back), answer a holding session with `quest tell "title"
  "message"` (typed in once the session is not mid turn, the same way
  `go_on` types), and hand the event to the human with `quest blocked
  "question"` on the quest, worded so the human can answer in one line.
  It changes no code and starts no session; the runner starts what the
  log says, with its fuses.
- **Fuses.** At most one Warriv a project. At most six wakes a project
  an hour, then events go to the human as today, with a notification
  saying why. It is never woken by its own changes. A `quest tell` to a
  session goes once per event. Its permission mode is the project's, and
  a prompt it stops at is an "asks you" like any session's.
- **Off by default.** `"orchestrator": true` in `.horadric/config.json`
  turns it on. Without it, every event goes to the human as today.
- **Pure and tested:** which events wake it, the wake budget, the first
  prompt, the queue of events. On screen with a dev instance: a fake
  quest blocked with a question its notes answer, Warriv wakes, tells the
  session, the quest goes on. Count `claude.exe` after.

Built on 2026-10-05. `horadric_core::warriv` decides: `events` reads
what the log holds now (a quest `[!]` with no wait and a reason Warriv
did not write, a quest whose session stopped after the nudge, a tangled
`After:` line, and a log in auto mode with nothing in hand or ready and
nobody blocked on the human), a `Desk` per project hears each event once
by its key, queues the new ones and spends the budget of six wakes an
hour, and `prompt`, `more`, `told` and `system_prompt` are what Warriv
and the told session read. A failed merge is pushed onto the desk by
`after_landing`, since the log does not hold it. The UI's `warriv.rs`
starts a fresh session named Warriv in the main tree when events wait
and none is awake, types the queued ones in as one line at its next
`Stop`, and closes it at a `Stop` with nothing left. What the plan did
not say:

- **Never woken by itself** is by kind. Warriv can only cause a tangled
  `After:` line or a stalled log, so one of those that first shows up
  while it is awake is taken as its own and not queued. A quest it hands
  on is blocked with a reason that starts `Warriv asks: `, which wakes
  nobody and tells step 3 which quests are the human's.
- **Its commands.** Besides `quest tell` and `quest add --after`:
  `quest note "title" "text"` (marked `Warriv: ` when Warriv runs it),
  `quest add --below "title"`, and `quest blocked "question" --quest
  "title"` to hand a held quest to the human. It starts with
  `--allowedTools "Bash(<horadric> quest:*)"`, so in the project's mode
  these never prompt, and its system prompt says to change the log only
  through them. A tell goes once per quest a wake; the app types it,
  with how to report done, once the holder is at `Stop`, and a blocked
  quest goes `[/]`. A tell for a quest whose session is gone becomes a
  `Warriv:` note and the quest starts again.
- **The last turn** is the `Stop` hook's whole `last_assistant_message`,
  kept on the session (not saved) and cut to its last 2000 characters.
- **Found on screen:** a session in the main tree inherited
  `HORADRIC_TASKS` from the Horadric that started it, so a dev instance
  started from a quest's worktree pointed `horadric quest` at that quest's
  own log. Warriv read the real horadric.dev log that way. Sessions
  without a worktree now get it empty.
- Seen on a dev instance with a scratch repository and a real Claude: the
  quest blocked on "Which port should the server listen on?", Warriv woke,
  ran `quest tell` and `quest note` without a prompt, the session got the
  answer, wrote the port, committed and ran `quest done`, and Warriv
  closed. No `claude.exe` left under the dev hosts after. A folder Claude
  has not been told to trust stops every session, Warriv's too, at the
  trust check before any hook, which nothing in Horadric sees yet.
- Not done: a Warriv session restored after a restart is paused like any
  other, and a new event starts a fresh one beside it.

**3. The human hears only what Warriv could not settle.**

Not an inbox: that was dropped on 2026-09-26, and the tiles, the hotkey
that walks waiting sessions and the notification still give the
overview. What changes is what reaches them. With Warriv on, a quest is
red only when it handed the question on, and its reason is that
question. "N quests need you" counts only those. The notes say what
Warriv tried, so the human answers without reading the session first.

Built on 2026-10-05. A project's `Desk` keeps the events that went to
the human: those that came while Warriv rested (its six wakes spent),
a tangle or stall that showed up while it was awake (its own doing,
which it would not settle), and each event it was given that still
holds when its session ends without telling that quest anything.
`Desk::holding` is every other quest among the events, which the app
keeps per project; Warriv hears before the toasts are said, so those
are left out of them, and "N quests need you" counts the rest. On the
tile, a blocked or tangled row Warriv holds reads "Warriv" in the
working colour with no red. A quest Warriv handed on is red "blocked",
and its toast is "Warriv asks: <title>" with the question alone.
`quest blocked --quest` refuses until the quest has a `Warriv:` note,
so the notes say what was tried. What the plan did not say:

- A quest's own session tile is untouched: a session stopped on a
  question still lights as waiting and the hotkey still walks to it,
  since the tile shows the session, not the quest.
- Seen on a dev instance with `cmd.exe` for the agent: a quest blocked
  on a question read "Warriv" while Warriv was up; handed on with a
  note it read red "blocked"; blocked anew and its Warriv killed
  mid wake, it read red "blocked" and no second Warriv started.

**Decided on 2026-10-05.** The human asked for an orchestrator that runs
"an insane amount of tasks" without them, and chose four things. Step 1
has its yes, and the work grows by three layers the section above did
not have. Built in this order: 1, 4, 5, 6, then 2 and 3.

- **Warriv is a full planner.** Everything in step 2 above: it answers,
  splits quests, adds `After:` lines and files fix-up quests. It still
  changes no code and starts no session.
- **4. Finished quests merge themselves.** In auto mode, a quest done in
  its own worktree is rebased on `main`, the project's checks run in the
  worktree, and `main` is fast forwarded to it. The checks are a
  `"checks"` list of commands in `.horadric/config.json`; none means
  merge without. A rebase conflict or a red check does not wait for the
  human: the branch stays, a fix-up quest is added right below the
  finished one with the failing output in its notes, and the quests
  that name the finished one in `After:` wait for the fix-up too. This
  also answers the open question about conflicts: a worker resolves
  them, Warriv only files the quest. One merge at a time per project,
  since two fast forwards race. Review mode keeps the click.

  Built on 2026-10-05. `horadric_core::merge` decides: `checks` reads
  the list (one string is one check), `plan` is rebase, each check,
  fast forward, `failed` reads what a step's failure means, `fix_up`
  writes the quest, and `add_fix_up` puts it in the list. The UI's
  `worktree::land` runs the plan on a thread when an auto mode quest's
  session ends, before its worktree goes, holding a lock per project.
  A rebase that conflicts is aborted. A check runs with `cmd /c` in
  the worktree, stdout and stderr in one pipe, for at most 30 minutes.
  A fast forward refused because `main` moved starts over, three times
  at most. A merged branch goes with its worktree, journaled and
  toasted as a clicked merge is. A conflict or a red check removes the
  worktree, keeps the branch, and adds "Fix the merge of <branch>" with
  the last 40 lines of output in its notes, which tell the worker to
  merge the branch into its own. Anything else (uncommitted changes in
  the worktree, or work in the main tree in the way of the fast
  forward) is no quest a worker can do, so it falls back to the
  toast whose click merges by hand. `add_fix_up` puts the fix-up right
  below the finished quest and gives every quest whose `After:` names
  the finished one an `After:` line for the fix-up as well, so nothing
  built on the work starts before it is on `main`. A line of output
  that reads as an `After:` line is quoted with `>`. The notes tell the
  worker to cherry-pick the branch's commits rather than merge it,
  since the auto merge rebases, which drops a merge commit and meets
  the same conflict again (seen on screen). While a merge runs, the
  runner sees its quest as still in hand (`while_landing`), so a quest
  after it does not start on a `main` without the work. Manual and review
  mode keep the click. This repository's checks are fmt, clippy and
  test.
- **5. Up to 16 at once, paced by usage.** `MOST_PARALLEL` goes from 8
  to 16, and the menu offers 1, 2, 4, 8 and 16. The runner stops
  starting quests while the fullest limit the status line reports is at
  90 % or more, rather than learning at 100 % mid turn. The 10 second
  gap between starts and every other fuse stays. Built on 2026-10-05:
  `Limits::too_full` is the check, the hold says "Quests wait on
  usage" once, and tombs stay at most 8, since a human picks among
  them. Seen on a dev instance with `cmd.exe`: starts 10 s apart, a
  faked 92 % held the next for a minute, 50 % let it start.
- **6. Quests in worktrees skip permission prompts.** A runner or click
  started quest in its own worktree runs with prompts bypassed, so it
  never stops for one. Sessions in the main tree, Warriv included, keep
  the project's mode, since there a wrong command touches what the
  human and other agents work in.

  Built on 2026-10-05. A session that holds a quest and has a worktree
  of its own (`tasks::bypasses_prompts`) starts with
  `Defaults::flags_for(.., bypass)`: the mode picked in the usage window
  gives way to `Agent::bypass_args`, which is `--permission-mode
  bypassPermissions` for Claude Code, the form its other modes take,
  `--dangerously-bypass-approvals-and-sandbox` for Codex and
  `--always-approve` for Grok. A mode the session's own arguments chose
  still wins. It is worked out at every start, so a resume after the
  quest is done gets the project's mode back. Tried with a dev instance
  and Haiku in a scratch repository: the worktree quest ran `echo` and
  `quest done` with no prompt, a main tree session in the same project
  stopped at "Do you want to proceed?". Claude's folder trust check still
  comes first in a folder Claude has not been told to trust, as it did
  for the scratch repository in `%TEMP%`; Horadric's own worktrees
  beside a trusted checkout do not show it.

**Open questions.**

- Whether Warriv may review `[?]` quests in review mode, as a first
  reader before the human. Decided on 2026-10-05: yes, as a mode of its
  own. See "The caravan moves on its own".
- Which model Warriv runs on. Its turns are short and many, which says a
  small one, but a wrong call costs more than the turn saves.

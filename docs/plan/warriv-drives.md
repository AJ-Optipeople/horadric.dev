# Warriv drives

Asked for on 2026-10-05. The human wants to leave the PC on, glance at
Warriv working now and then, and come back to what shipped: the
autonomous agent pushed as far as Claude goes today. Today Warriv is
several reflexes (event wakes, the reviewer, errands), each a fresh
session with no shared picture. This section makes it one mind that
uses all of them on purpose. The human decided:

- **One switch, "Warriv drives".** Off, Horadric is as today. On,
  Warriv runs the project alone. A second switch, "and ships public",
  lets it cut public releases unattended. Both are the human's to flip.
- **No caps.** No daily quest cap, no spend cap, no usage ceiling, no
  weekly pacing. While Warriv drives, its six wakes an hour are lifted
  too. What stays is what only waits: at 90 % of a limit the runner
  holds until the reset and goes on (`Limits::too_full`, `go_on`),
  since a turn cut off at 100 % is lost work, not saved money. An
  errand due in that hold waits for the reset instead of being skipped.
- What stops runaways is not a budget but the rules already there:
  Warriv is never woken by itself, each event is heard once, and the
  shipping rails below.

**1. A memory, `.horadric/warriv.md`.** Every Warriv session (a wake, a
round, the reviewer, an errand) reads it first and writes to it last.
Three parts: **Rules** (how this project wants things done), **Lately**
(what it did and why, newest first, compacted by Warriv when it passes
about 200 lines) and **Open** (what it is waiting to see). When the
human answers a question Warriv handed on, the next wake writes the
answer as a rule, so the same kind of question never comes twice.
Committed with the project, so it travels with the repository and its
history shows how Warriv's judgment grew. `warriv::memory_prompt` and
the compaction rule are pure and tested.
**Built on 2026-10-05**, for wakes, reviewers and errand sessions, the
Warriv sessions there are yet (`memory_tools` and `memory_prompt` in
the UI's `warriv.rs`); rounds add the same two when they land. The
session reads and writes the file itself, so the prompt says how instead of carrying it; it is the one
file Warriv may edit (`Edit(./.horadric/warriv.md)` is in its allowed
tools, and covers writing it too; Claude Code refuses a `Write(...)`
rule). Past `MEMORY_LINES` (200) the prompt asks for a compaction to
under 150: every rule and Open kept, the newest twenty Lately lines as
they are, older ones folded a line a day or week. The app commits the
file alone (`git commit --only`) when a wake, a review or an errand
closes. A handed on
question is noted with when Horadric first saw it; the next prompt into
its quest's session, or a `quest tell` from the human, is the answer,
and Horadric writes it into Open as an `- Answered:` line
(`warriv::with_answer`). The next wake's prompt says to make each such
line a rule and remove it. A quest that goes on without words Horadric
heard leaves a line that says to read the quest's notes. An answer is
kept once, though the quest still reads as blocked on it a moment after.
Seen on a dev instance with Haiku in a scratch repository: the first
wake created the memory with a Lately line and the question in Open,
and the app committed it alone; a `quest tell` from the human became an
`Answered:` line; the next wake made it the rule "anything under 50 EUR
a year goes on the company Visa" and settled a new quest by it without
asking. The prompt names the memory by its whole path, since Haiku read
a relative one against a parent folder, and says not to commit it.

**2. Rounds.** While Warriv drives, a round every hour, after each
landing and when the human leaves: a Warriv session that looks at the
whole project (the log, the aims, `main`'s checks, the memory, open
questions, errands due) and decides. Events stay for speed; rounds are
for judgment. In a round it may do everything a wake may, and also
cast a stone (`horadric runeword cast "name"`, new), ship (below) and
reorder the log. A round that finds nothing to do writes one line to
Lately and closes. One Warriv session at a time a project still: a
round due while one is awake is folded into it.

**Built on 2026-10-05**: a round is an event of its own on Warriv's
desk, `Kind::Round`, with no quest. `Desk::hourly` brings one an hour
after the last was due (the first look while driving, after the switch
or a reload, starts the hour) and `Desk::round` one for a landing or
the human leaving, heard where errands hear `landed` and `away`. Only
while driving; turning the drive off drops one waiting. Waiting rounds
are one with every reason, and a round is told like any event: as the
first prompt of a fresh Warriv, or at the next stop of one awake,
which is the fold. Its brief (`warriv::prompt`, pure and tested) is the
picture: the quests not done (the first 40) with what blocks them, the
aims, `main`'s commit, whether the checks passed there and what landed
since the last ship (`warriv::main_line`), each armed errand and when it
goes next (`warriv::errand_line`), and the project's stones. The memory
and its Open part it reads itself. It says to build nothing, since Haiku
first took "what moves it on" as leave to write the open quest's file.
`horadric runeword cast "name"` (by label or runeword name, any case)
has the app cast a stone: sessionless on the project, else in a fresh
session named after it with its first `say` as the prompt. A Warriv
casts only while it drives, and "Ship Public" only with "and ships
public" on (`warriv::may_cast`); a driving Warriv may run the command
without asking. Reordering the log came after (2026-10-05): `horadric
quest move "title" --below "title"` (or `--above`) moves a quest and
its notes, leaving headings where they are (`tasks::move_item`, by name
`warriv::move_quest`). It is a quest command, so a Warriv runs it
without asking, and `warriv::driven_prompt` names it for rounds. On the way:
`is_warriv` read this quest's own session, `warriv-s-rounds-...`, as a
Warriv, so it was started with Warriv's prompt; session ids made from a
base are now matched as the base and a number (`session::made_from`).
Seen on a dev instance (port 4110, scratch `APPDATA` and `LOCALAPPDATA`,
Haiku) in a scratch repository with a memory rule to cast a `run` stone
each round: a `round-now` file in the dev state folder (dev only) brought
a round, a second one twelve seconds later was told at the first's stop
as "While you worked, more happened", the stone was cast once a round
with no permission asked, each round wrote one Lately line, the session
closed as settled and the app committed the memory. No `claude.exe` was
left. Claude Code's trust prompt for a new folder holds a round as it
holds any session there.

**3. Decide what can be undone, ask about what cannot.** While Warriv
drives, the quest sessions' prompt says: a choice that can be changed
later (a name, a layout, which of two fixes) is made, with a
`quest note "title" "Assumed: ..."` line, and the work goes on. Only a
choice that cannot be undone, or that only the human can know, blocks.
Warriv does the same with what it is told. A quest blocked on the human
no longer stops the list: it is passed over like a quest that waits,
and the rest of the caravan moves. The away card lists every
`Assumed:` line since the human left, so each can be overruled in one
line.

**Built on 2026-10-05**: `tasks::driven_prompt` goes to a quest's
session beside its system prompt while Warriv drives the project, and
`warriv::driven_prompt` to Warriv's; both name the line, `quest note
"title" "Assumed: ..."`, and what still blocks (deleting data,
publishing, money, an account, what only the human knows). `tasks::next`
takes whether Warriv drives: then a quest blocked on the human is passed
over like one that waits, a quest `After:` it waits too, and the list is
not finished while it is open. A session gone and a cycle still stop
the list. `quest note` with a line `tasks::assumed` reads (any case,
from any session, Warriv's too) writes an `assumed` record to the
chronicle, and `chronicle::away` lists those since the human left,
oldest first, the same line twice once. An assumption alone opens the
card. On the card it is a part of its own, "assumed", in amber, between
Warriv and the questions, each with a field that reads "Leave it, or
overrule it in one line". What the plan did not say is where an
overrule goes, `warriv::overrule`, by how its quest stands now: held by
a session, it is told "You assumed X; instead: Y" as the human's answer;
not taken yet, that is a `The human answers:` notes line; done or gone,
a new quest "Overrule on <title>" at the end of the log, with both in
its notes. The answered row reads "overruled". Seen on a dev instance
with `cmd.exe` for the agent: with the drive on in `state.json` and a
quest blocked on the human second in an auto log, the runner started
the third; two `Assumed:` notes made the card open on `away-now`, and
an overrule typed on the done quest's field added its "Overrule on"
quest. Not tried on screen: an overrule told to a live session, which
is the path the card's answers already take.

**4. Warriv ships.** While it drives, a round ships local when `main` is
green and quests have landed since the last ship, by casting the
project's "Ship Local" stone in a session of its own. With "and ships
public" on, it ships public instead when the landed work is worth a
release to users (a feature or a fix a user would notice), picking the
version and writing the notes per RELEASING.md and CLAUDE.md. The
rails:

- After a ship, the next round reads `reload.log`. A build that rolled
  back, or `main`'s checks red twice in a row, stops shipping: the
  switch shows "shipping held" and a fix-up quest is filed with the
  log. Shipping goes on by itself once that quest lands and the checks
  pass.
- The drive switches live in `state.json`, so they survive the reload a
  ship causes; the new build picks the rounds up where they were.
- CLAUDE.md says the switch is the human's go ahead to ship, the one
  way besides their words.

**Built on 2026-10-05**: a round's picture says what to do about
shipping (`ship::brief`, pure and tested), in a project with a "Ship
Local" stone. `ship::due` ships when any quest landed since the last
ship, the checks passed on `main` and nothing holds it; then the round
is told to cast "Ship Local", or with "and ships public" on to cast
"Ship Public" when the work is worth a release and "Ship Local" else.
The cast is a session of its own, as any stone's. The rails are kept
on the drive in `state.json` (`held`, `red`, `judged`), so a reload
keeps them. As a round is told, the app reads `reload.log`: a reload
of the project's build that rolled back since the last round read it
(`ship::rolled_back`, `ship::holds`) holds shipping and files "Fix the
build that rolled back" at the end of the log with the log in its
notes. Turning the drive on reads the log as judged, so an old
rollback holds nothing. "Red twice" is read as two landings in a row
whose checks failed (`ship::red_after`), since a landing runs the
checks before `main` moves; a conflict counts neither way. The second
holds shipping on the fix-up quest that landing filed. `ship::resumes`
lifts the hold once that quest has a `Merged` record after the hold
began and the checks passed on `main`. While held, `warriv::may_cast`
refuses both ship stones to a Warriv, the tile reads "Warriv drives,
shipping held", the mode menu has a line "Shipping held until "<quest>"
lands" and the tray names the project with ", shipping held". Toasts
say when it holds and when it goes on. Seen on a dev instance (port
4100, scratch `APPDATA` and `LOCALAPPDATA`, `cmd.exe` for the agent),
with the drive on in `state.json` and a faked `reload.log` whose build
of the scratch project rolled back: a `round-now` brought a round,
`held` went into `state.json`, the fix-up quest came into the log with
the four log lines, and the tile read "Warriv drives, shipping held,
settling". Not tried on screen: a real ship, the red count, and the
hold lifting, which the tests cover.

**5. A model per quest.** Warriv writes a `Model: haiku|sonnet|opus`
notes line on quests it files or meets without one, by how hard the
work is, and the runner starts the quest with that `--model` (Claude
Code only; other agents ignore it). A human's `Model:` line is never
changed. Pure and tested: reading the line, the flag. **Built on
2026-10-05**: `tasks::model_line` reads `Model: haiku|sonnet|opus` in
any case and nothing else, so a typo starts the default model rather
than a session that fails. `Task::model` takes a human's line over
Warriv's (written as `Warriv: Model: ...` through `quest note`), and of
Warriv's its last. The runner adds `--model` for Claude Code only, in
`extra_args`, read again at every start, so a resume takes a model
Warriv wrote since, and the usage window's default model stays out.
Warriv's prompt says how to pick by difficulty. Seen on a dev instance:
a `Model: haiku` quest started as `claude --model haiku`, and its pane
read Haiku 4.5.

**6. Event triggers for errands.** `"on": "landed"`, `"shipped"`,
`"away"` or `"back"` on a stone casts it at that event instead of on a
clock, so "post what shipped to me on Slack" is a stone. Armed like a
clock errand. **Built on 2026-10-05**: `runeword::parse` reads `"on"`
as `Every::On(Event)`, pure and tested; a stone with both `"every"` and
`"on"`, or another event, is cracked with the reason. The app marks an
armed errand that hears the event `fired` (saved in `Armed`), and
`runeword::tick` treats it as due since then, so one at a time, the
90 % hold and `"for"` work as for a clock errand; an event while it runs
casts it once more after. Landed is heard as a quest merges by itself
(`merged`), away and back as the absence begins and ends (locking the
screen too), and shipped by a build started from a reload once
`reload.log` says it came up (`ship::came_up`) and was built from the
project's folder (`ship::shipped`), watched for two minutes. The arm
question and the tooltip say "on each landing", "after each ship",
"when you leave" or "when you come back", without the line about a
missed cast. Seen on a dev instance (port 4110, scratch `APPDATA`,
`cmd.exe` for the agent): a quest in an auto log with two at a time
finished, landed on `main`, and an armed `"on": "landed"` run stone
appended to a file once within the second. Not tried on screen:
shipped, away and back.

**7. Spectator mode.** While Warriv drives and the human is away, the
stage follows the work: it shows the project where something last
happened, the pane mid turn focused, staying at least 20 seconds on
each. A Warriv session gets the stage first. It never takes the
foreground from another program: it only moves when the stage was the
foreground window when the human left. The first input gives the stage
back exactly as the human left it. **Built on 2026-10-05**:
`spectator::next` chooses, pure and tested: only driven projects, a
Warriv mid turn first, else the session that did something last; in that
project Warriv, else the pane mid turn, has the keyboard; a view stays
20 seconds. The app (`spectating.rs`) begins as the absence does, when
the stage is in front and something is driven, and moves only while the
stage is still in front, so a program that took the foreground keeps it.
It keeps the project, the active pane and the zoom as they were; a
100 ms timer watches the input clock, and the first input puts them
back without taking the foreground. Sessions shown while spectating are
not marked read. A dev instance leaves at once with a `spectate-now`
file in its state folder. Seen on a dev instance with cmd.exe sessions
in two projects: with Chrome in front nothing moved; with the stage in
front it went to the busy project, held 20 seconds, moved to the other's
busy pane, and a key press gave back the project and pane as left.

**8. A look back.** While it drives, once a day (at the first round
after 04:00) Warriv reads the day's chronicle and its memory: repeated
merge conflicts, a flaky check, sessions that stopped without
reporting, quests that came back from review twice. Each pattern
becomes a quest, and a lesson worth keeping becomes a rule.

**Built on 2026-10-05**: the look back is an event of its own,
`Kind::LookBack`, which `Desk::look_back` puts beside a waiting round
while driving when `lookback::due` says none was told since the latest
04:00 local. Due reads the chronicle (a `warriv_woke` line with a
`look_back` event), so a reload never brings a second one, and it is
read only when a round waits. The patterns are found by
`lookback::patterns`, pure and tested, over the project's chronicle
since the last look back (a day at most): two or more rebases stopped
on a conflict, a check red two or more times, two or more wakes for a
session that stopped without reporting, and a quest that came back from
review twice (by `quest fix`, or taken again after `[?]`, the larger of
the two, counted over its whole story). The chronicle did not record
those first two, so a merge that fails now writes `not_merged` with its
`merge::Failure`, and the reviewer's `quest fix` is a `warriv_ran` line
with the command `fix`. The round's prompt gets the day in a line
(`lookback::day`) and each pattern as a line, and says to file a quest
per pattern unless one in the log already does, to write what it taught
as a rule, and one Lately line; a clean day is one Lately line. Seen on
a dev instance (port 4110, scratch `APPDATA`, Haiku) with fake
chronicle lines in a scratch repository: `round-now` woke Warriv with a
round and the look back together, and it filed five quests (the
conflicts, the red check, one per silent session, the twice reviewed
quest) with `Warriv:` notes and wrote a rule and Open lines to its
memory. Haiku also tried to commit the memory itself, which the
permission prompt stopped.

**The switch and the stop.** "Warriv drives" and "and ships public" are
in the tray menu and the quests tile's mode menu; the quests tile reads
"Warriv drives" in gold while it is on. A tray item and a hotkey
(Ctrl+Alt+W, with Shift on a dev instance like the other keys) stop it at once: the switch goes off, Warriv's
sessions and errand sessions are closed, the runner stops starting
quests. Quests in hand finish and land as they would in auto mode.

**Built on 2026-10-05**: `warriv::Drive` per project key in `state.json`
(`drives`, and `stopped` for the projects the stop key stopped). Turning
it on sets the log to auto mode; `warriv::orchestrates` turns Warriv on
whatever the config says, `Desk::drive` lifts the six wakes and the rest,
and `warriv::line` puts "Warriv drives" (and "and ships public") in gold
at the head of the tile's Warriv line. "and ships public" asks once
before it turns on, since every install sees what it publishes. The stop
(tray, Ctrl+Alt+W, Shift on a dev instance) closes each driven project's
Warriv session and holds its runner: nothing new starts there until the
human picks a mode in the mode menu or lets Warriv drive again, which
the menu says. While Warriv drives, `warriv::when_full` gives
`runeword::tick` `Full::Wait`: a due errand in the 90 % hold is neither
cast nor skipped, its clock stays put, and it is cast the tick the limit
drops. Not driving it is skipped as before. The stop halts every errand
the clock cast in the project (`halt_errands`, its command's tree with
it), which is not a failure: it stays armed and goes at its next time.
An errand's session closes there too. Checked on a dev instance (port 4170, scratch `APPDATA`) with
a `"1m"` errand, driving, at 95 %: nothing cast and nothing skipped for
75 seconds, a status at 10 % cast it within the second, and the stop
key (posted as `WM_HOTKEY`) killed its `ping`, turned the drive off and
left it armed, not running and not failed.

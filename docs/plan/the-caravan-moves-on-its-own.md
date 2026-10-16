# The caravan moves on its own

Asked for on 2026-10-05. The human wants the app to feel like autonomy.
Warriv today is a firefighter: it wakes only when something goes wrong,
and it can only rearrange quests that already exist. When the log runs
dry the caravan stops, in review mode every `[?]` waits for the human,
and what happened while the human was away is spread over toasts and
notes. Autonomy needs the work to move forward when nothing is wrong
and nobody watches, and to show what it did when the human comes back.
Six parts, each useful alone.

**1. Aims.** The human writes where the work is going, not each quest.

- **The form.** `Aim: <text>` lines at the top of `.horadric/tasks.md`,
  above the first section, in order. An aim names what done looks like
  and may point at a plan section ("Warriv in the quest log, all of
  it"). `horadric quest aim "text"` adds one, `quest aim done "text"`
  marks it `Aim reached: <text>` so it stays as a record.
- **Warriv files the next quests.** The stalled event (auto mode,
  nothing in hand or ready, nobody blocked on the human) already wakes
  it. With an aim open, its prompt carries the aims and says: read what
  they point at, file the next quests with `quest add` and `After:`
  lines, at most eight a wake, or mark the aim reached. Each quest it
  files gets a `Filed by Warriv for: <aim>` notes line. With no aim
  open, stalled is handed to the human as today.
- **Fuses.** It never files a quest whose title matches one already in
  the log. An aim with three wakes in a row that filed nothing goes to
  the human as a question.
- Pure and tested: reading and writing aims, the prompt with aims, the
  title match.

Built on 2026-10-05. `horadric_core::aim` reads the aims above the
first section or quest, adds one after the last (or as its own
paragraph above the first section) and marks one reached by its text or
its start, as quests are found; `aim::same` is the title match (case,
spaces and punctuation do not count). A dry log with an aim open is an
event of its own, `warriv::Kind::Dry`, which fires even with no quest
left at all, so an empty log and an aim are enough. It is never taken as
Warriv's own doing, since filing is its job; the six wakes still bound
it. Its brief carries the aims and the rules, and the system prompt the
commands. `quest add` from a Warriv session refuses a title the log
has. What the plan did not say:

- **Filed nothing** is known when a wake given the dry log ends: the
  log is still dry the same way, no quest gained a `Filed by Warriv for:`
  line, and the open aims are as they were. Such a wake is heard again
  at once, and the third in a row goes to the human as a toast,
  "Warriv asks", naming the first aim (`Desk::dry_ended`). A wake that
  moved something but left the log dry is heard again too, with the
  count started over.
- **Warriv's log path.** A Warriv session gets `HORADRIC_TASKS` set to
  its own project, so a `quest` command run from another folder still
  writes the right log.
- **Found on screen** with Haiku: it put `cd <dir> &&` before the
  command, and once used the PowerShell tool, both of which miss the
  allowed `Bash(<horadric> quest:*)` and stop at a permission prompt.
  The system prompt now says to run it with the Bash tool, as written.
  It also rewrote a horadric path that looked like a Claude scratchpad
  folder into its own; the installed path does not look like one.
- Seen on a dev instance with a scratch repository and Haiku: an aim
  and an empty log in auto mode woke Warriv, which filed "Create
  hello.txt" and "Create bye.txt" (`After: Create hello.txt`, each with
  its `Filed by` line). The runner started and landed them in turn, the
  log ran dry again, and the next wake marked the aim reached. No
  `claude.exe` was left but the one idle session.

**2. Warriv reads first in review mode.** A fourth mode beside manual,
review and auto: "Warriv reviews". A quest going `[?]` wakes a reviewer
session with the quest, its notes, and the branch's diff against `main`.
It ends one of three ways:

- `quest pass "title"`: merges as the human's click would, through the
  same landing (rebase, checks, fast forward).
- `quest fix "title" "what is wrong"`: tells the holding session if it
  is alive, otherwise adds a fix-up quest below as a failed merge does.
- `quest blocked "question" --quest "title"`: the human gets it, with
  what the reviewer saw in a `Warriv:` note.

Reviews have their own budget, not the six wakes: one reviewer a
project at a time, the rest queued. The reviewer runs no checks, since
landing runs them. The mode menu says what the mode does in one line.
Built on 2026-10-05: `Mode::Warriv` (`"mode": "warriv"`) finishes a
quest as review does and lands it as auto does. `warriv::Reviews` keeps
the queue, `warriv::review_prompt` puts the diff in (cut at 20 000
characters, since the prompt goes on a command line), and a reviewer is
a session `warriv-review-*`, so it counts as a Warriv for notes and for
handing on. A reviewer that ends without a verdict leaves the quest to
the human, who hears "Ready for review" then and not before.

**3. While you were away.** When the human has given no input for 30
minutes (`GetLastInputInfo`, already read in `app.rs`) and then comes
back, one card opens on the stage if anything happened: quests landed,
what Warriv decided and why (one line a wake), errands run and what
they found, and last the questions only the human can answer, each
with a one line answer field. Enter on an answer does what
`quest tell` does for that quest. Esc closes it; it does not come back
until the next absence. `chronicle::away(since)` builds it, pure and
tested. Nothing happened, no card.

Built on 2026-10-05. `chronicle::away(records, project, list,
warriv_has, since)` gives a project's quests marked done since then
(with the agent's summary), a line a wake (what Warriv did, then its
own words for why, or what woke it when it ran nothing), and the
quests blocked on the human: no wait Horadric checks, a session that
holds them, and not among those Warriv has. Only news opens the card:
a question asked before the human left is asked again but opens
nothing. The UI's `away.rs` draws it with the catch-up's layout and
plate, which learned a field row and a width. What the plan did not
say:

- **It takes the catch-up's place.** Away 30 minutes or more with news
  in any quest log, the card opens and the catch-up does not, so coming
  back is one card. Shorter, or no news, the catch-up opens as before.
- **It stands over the stage,** owned by it and centred on it, at most
  520 DIPs wide and narrower over a narrow stage; with the stage hidden,
  in the middle of the primary screen. It does not close when the focus
  goes elsewhere, since answering often means reading a session first.
- **An answer is the human's, not Warriv's.** The session is told "The
  human answers: ...", no "Warriv answered" toast is shown, and a quest
  whose session is gone gets a `The human answers:` notes line, not a
  `Warriv:` one, and starts again. Enter moves the keyboard to the next
  question not yet told, and the one told reads "told" with its lamp
  out. Tab and the arrows move between questions.
- **Errands** had no chronicle line yet, so the card had no errands
  part. Part 4 added both: an "errands" part under Warriv's, a line a
  cast that filed something (its quests) or failed (why).
- A dev instance treats a file `away-now` in its state folder as coming
  back from an hour away, since an absence can not be tried while
  anyone uses the machine.
- Seen on a dev instance with a seeded chronicle and `cmd.exe` for the
  agent, in Skeuomorphism (dark), Flat (light) and over a 420 pixel
  stage: an answer typed and Entered wrote the note, started the quest
  again, and moved to the next question; Esc closed the card. Not tried
  on screen: typing into a live session between turns, which is the
  path Warriv's tells already take.

**4. Errands: runewords on a clock.** A stone in the Runetome may carry
`"every"`, and then Warriv casts it unattended:

```json
{ "runewords": {
    "Feedback to quests": {
      "every": "1h",
      "steps": [ { "say": "Read Slack #horadric and the inbox since {since} for feedback on Horadric. File each new point as a quest with a From: line." } ] },
    "Nightly ship": { "every": "day 03:00", "steps": [ { "say": "Ship local" } ] },
    "Clean target": { "every": "sunday 12:00", "steps": [ { "run": "cargo clean" } ] } } }
```

- **When.** `"every"` reads `30m`, `1h`, `day 09:00`, `weekday 08:30`,
  or a weekday name and a time. `runeword::every` parses it and
  `runeword::due` says the next cast from the last one, pure and
  tested. A cast missed while Horadric was off runs once at start, not
  once per missed slot.
- **How.** A stone of only `run` steps runs as a sessionless runeword
  does today. A stone with `say` steps starts a fresh session named
  after the stone, in Warriv's gold, in the main tree, and its steps
  are cast on it turn by turn as any runeword. After the last step's
  `Stop` it closes. Its system prompt (`warriv::errand_prompt`) says
  what Horadric and the quest commands are, that what it finds becomes
  quests, that each quest carries a `From: <link or id>` notes line,
  and that it files nothing whose `From:` is already in the log. `{since}`
  in a step is the time of the last cast that finished. The `test`,
  `review` and `merge` runes need a quest's session, so a stone with
  them and `"every"` shows cracked with that reason.
- **What it can reach** is whatever the agent can: `gh` for pull
  requests, MCP connectors for Slack and mail, any command. Horadric
  adds no integration of its own.
- **Arming.** An `"every"` stone does nothing until the human arms it
  from the tome once: a click shows the steps, the schedule and the
  permission mode, and asks "Run this unattended?". The arming keeps a
  hash of the steps in `state.json`. A stone whose steps change is
  disarmed and shows the mark, so a pull that changes an errand never
  runs it unseen. Arming an errand that publishes (a push, a release, a
  post) is the human's standing go ahead for it; that is the one way
  "ship public" happens without the human saying it that day, and
  CLAUDE.md says so.
- **Permission mode.** The project's, so a prompt stops the errand as an
  "asks you" like any session. `"mode": "bypass"` on the stone skips
  prompts, and arming says so in red.
- **Fuses.** One errand at a time a project, the rest wait their turn. A
  due errand is skipped (not queued) while the fullest limit is at 90 %
  or more. An errand running past `"for"` (default 30 minutes) is
  stopped. A failure toasts once and the stone shows red until a cast
  succeeds. Errands do not spend Warriv's six wakes. Count `claude.exe`
  after any test, since this starts agents on a clock.
- **In the tome** an errand stone has a thin ring that fills toward its
  next cast; hover shows the last cast, how it ended and the next one.
  Unarmed, the ring is dim. **In the chronicle** each cast is a dot on
  Warriv's lane like a wake, with the quests it filed.
- **The Runesmith** knows `"every"`, `{since}` and the `From:` rule, so
  "check Slack every hour for feedback" is a sentence to it.

Event triggers (`"on": "landed"`, `"on": "away"`) are the next step once
the clock works, and are left out until an errand needs one.

The engine is built (2026-10-05), with errands of `run` steps.
`runeword::every` reads `"every"` (a span of `s`, `m` and `h` pieces,
once a minute at most, or `day`, `weekday`, a day's name or its first
three letters, and `H:MM`), `runeword::span` reads `"for"`, and a stone
with either wrong, or with test, review or merge, is cracked with the
reason. `runeword::due` is the first slot after the last cast, from the
local clock's offset, so a slot missed while the app was off is due at
once and the next one counts from then. `runeword::tick` decides per
project: while one runs the others wait and it is `Overdue` past its
`"for"`; otherwise the one due longest is cast, or every due one is
skipped (its clock moved on to now) while the fullest limit is at 90 %.
Armed errands are `errands` in `state.json` (`Armed`: the steps'
fingerprint, the last cast, the last good finish for `{since}`, failed,
running), and a stone whose steps no longer fit is taken out of it. The
tome marks an errand that is not armed with the amber dot and one whose
last cast failed with a red one; a click on an unarmed errand asks "Run
<label> unattended?" with the schedule and every step, and the right
click menu offers Arm or Disarm, and Cast to run it once by hand. An
errand's cast is a sessionless runeword as any: success says nothing,
a failure toasts once until a cast succeeds, and an overdue one is
stopped with its command's tree (`taskkill /T`). The tooltip and
`horadric runeword list` say when it runs.

The session half is built (2026-10-05). A due errand with a step that
needs a session starts a fresh one, `errand-<n>` named after the stone,
in the main tree, its name on the tile in Warriv's gold (Warriv's own
sessions too). Its first `say` step is its first prompt, so nothing is
typed into an agent still starting; the rest are cast turn by turn as
any runeword, and after the last turn's `Stop` the session closes. A
step that stops it leaves the session open for the human to read why,
and an agent that quits mid cast fails it at once. `Armed.session`
keeps which session is the cast's, in `state.json`, so a reload goes
on. It starts in the project's permission mode with the quest commands
allowed, as Warriv is; `"mode": "bypass"` on the stone skips prompts,
and arming such a stone asks in red and says so. The arming covers the
mode too: a stone turned to bypass after it was armed is disarmed (a
stone without a mode keeps the fingerprint it had). Its system prompt,
`warriv::errand_prompt`, says what Horadric and the quest commands are,
that findings become quests with a `From:` notes line naming that one
finding, and lists the `From:` lines in the log as the cast began
(`warriv::froms`, the newest 200) so nothing is filed twice.
`runeword::since` puts the last good finish in UTC where a step says
`{since}` (when none finished yet, the arming or the last cast).
CLAUDE.md says arming a publishing errand is the human's standing go
ahead. The stop key's `halt_errands` closes an errand's session too.

What the human sees is built (2026-10-05). In the tome an errand stone
stands in a thin ring, drawn behind the slab, that fills clockwise in
Warriv's gold from its last cast to its next (`runeword::toward`), full
while it is cast; unarmed it is the faint track alone. The app paints
the tome again every 5 seconds while any errand is armed. Hovering adds
two lines, `runeword::cast_lines`: the last cast and how it ended
("Last cast at 21:40: failed, it exited with 1", "Casting, begun at
...", "Not cast yet"), then the next ("Next at 21:42", "Next Sunday at
12:00"). `Armed` keeps `cast`, when the last cast began (`last` also
moves on a skip or an arming), and `why` it failed. In the chronicle a
cast is `errand_cast` (its session, or its label when it has none) and
`errand_ended` (settled, failed with why, or cut short by the stop key
or lost with the app), and the `quest` commands an errand's session runs
are `warriv_ran` lines as Warriv's are. `chronicle::wakes` folds them in
with Warriv's wakes, with the errand's label, so `wake_dots` puts each
cast on the lane with lines to the quests it filed. A cast is a small
square there, not a dot, red when it failed; its detail says when, what
it filed, what else it did and how it ended. The Runesmith's prompt
explains `"every"`, `"on"`, `"for"`, `"mode"`, `{since}`, the `From:` rule, and
that only the human arms a stone. What the plan did not say:

- **Not every cast is a dot.** An errand cast each minute would be a
  dot a minute. `chronicle::lane` leaves off a cast that ran no command
  when the next cast of its errand ran none either and ended the same
  way, so each run of quiet casts, or of failures alike, is one square,
  its newest. The away card folds the same way.
- **Nor a line for good.** A cast of only `run` steps is told to the
  chronicle when it ends, both lines at once, and one that went well is
  told at most once an hour, so an errand each minute does not grow the
  chronicle by two lines a minute. A cast with a session is told when it
  starts, so its square glows while it runs.

Checked on a dev instance of its own (port 4101, scratch `APPDATA`) in
Skeuomorphism and Flat, with three stones of `run` steps or never armed,
so no agent started: one each minute that succeeds, one each two minutes
that fails, and an unarmed one with a `say` step, two of them armed
through `state.json`, and a seeded chronicle. The rings filled, the
unarmed track was dim, hovering read the last and next cast, the lane
showed one square for the quiet errand and one a distinct failure, a
square's detail read as above with its line to the quest it filed, and
`away-now` opened the card with the errands part.

Checked on a dev instance of its own (port 4180, scratch `APPDATA`), a
scratch repository with Haiku and a stone `"every": "2m"` of two `say`
steps (file each line of `feedback.txt` as a quest, then reply "done"),
armed through `state.json` ten minutes back. The first cast filed one
quest with `From: fb-17`, cast both turns and closed in 26 seconds; the
next, two minutes later, filed nothing and closed. No `claude.exe` of
the scratch repository was left after either. On the way: Haiku used
the PowerShell tool for every `quest` command, both plain and as
`& "..."`, and stopped at "needs permission" each time. Warriv and
errands now get the commands allowed for PowerShell as well as Bash
(`warriv::quest_tools`), and the prompts say Bash, as written, never
behind `&`, `cd` or `&&`, since the `& "..."` form matches no rule. A cast whose agent sat on Claude's folder trust
question was stopped at its `"for"` as it should be. The red arming
dialog was not clicked on screen.

Checked on a dev instance of its own (port 4170, scratch `APPDATA`) with
a stone `"every": "1m"` writing the time to a file, armed through
`state.json` ten minutes back: it ran at once, then each minute; with the
app off four minutes it ran once at the next start; a status posting 92 %
held it two minutes, "skipped" in the log, and 10 % let it run at the
next minute; changing its command disarmed it within the second, no cast
after, and the stone showed the amber dot. The arming dialog itself was
not clicked on screen, since the human was using the desktop.

**5. Ships proposed.** When three or more quests have landed on `main`
since the last ship local and the checks passed on `main`'s head, a
toast says "5 quests landed. Ship local?" and its click casts the
project's "Ship Local" stone. Once per landing count; ignored, it waits
for the next landing. The rule is pure and tested.

Built 2026-10-05. `ship.rs` in `horadric-core` holds the rule. A landing
now records `checked`, the commit its checks passed on, in its `merged`
chronicle line, and "checks passed on `main`'s head" means `main` is still
at the newest landing's `checked`: a commit made on `main` since was
checked by nobody, so the proposal waits for the next landing. The last
ship is `reload.log` when it names the project (written as the binaries
go in, so surer), else the newest `shipped` chronicle line, which casting
the "Ship Local" stone writes. Only a project with that stone is asked.
The click puts the project on the stage and casts the stone as a click
on it would, asking first. Seen on screen with a dev instance, two fake
landings in its chronicle and real ones of a scratch repository.

**6. A pulse.** While Warriv, a reviewer or an errand works, the quests
tile's edge breathes slowly in Warriv's gold, so the camp is seen moving
without reading anything. Still when nothing runs. Beside the live line
already planned in "Warriv in the quest log".

Built on 2026-10-05, in `theme::warriv`, the gold of Warriv's lane in the
quest log. `Shared::astir` holds the projects
where something of Warriv's works; Warriv's camp sets it while a session
is awake, and the reviewer and errands set it the same way once they
exist. The breath is drawn in the light pass, over the kept layer, at the
waiting tile's frame rate, and asks for no frames when nothing breathes;
with Windows' animations off the edge holds still at half a breath. On
the way, `motion::cycle` took the wall clock as an f32, which at today's
seconds since 1970 has no fractions left, so the busy wash never breathed
either; it counts whole nanoseconds now.

Already built and left alone: a session cut off by a usage limit is
told to go on a minute after the reset (`tasks::go_on`).

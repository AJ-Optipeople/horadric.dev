# Warriv in the quest log

Asked for on 2026-10-05. Warriv's work is hard to see: a tile named
Warriv while it is awake, rows on the quests tile that read "Warriv",
and `Warriv:` notes in each quest's detail. Nothing shows a wake as a
whole, what woke it, what it did and how it ended, and nothing shows
it after its session closes.

- **The chronicle records wakes.** A line when Warriv wakes (the
  wake's id, its conversation id, the events, each by kind and quest),
  a line for each `quest` command it runs (`tell`, `note`, `add`,
  `blocked`, with the quest and the text), and a line when it closes:
  settled, or handed on (the quests it handed to the human), or cut
  short (its session killed or the app quit). The app writes the first
  and last from `warriv.rs`; the CLI writes the commands, since it
  already knows when Warriv runs one (`warriv::is_warriv` on the
  session id, as `quest note` uses to mark `Warriv: `).
  Built on 2026-10-05. The lines are `warriv_woke`, `warriv_ran` and
  `warriv_slept`, keyed by the Warriv session's id; none names a quest
  holder, so `chronicle::quests` passes them by. A wake given more
  events while awake is another `warriv_woke` with the same id.
  `chronicle::wakes` folds them into wakes, joining a command or an end
  to the latest open wake of its id, since an id comes back each day.
  The conversation is filled from whichever line knew it: the app's
  once the registry has it, the CLI's from `CLAUDE_CODE_SESSION_ID`.
  Handed on lists both the events left holding and the quests Warriv
  handed on itself. A Quit or reload with Warriv awake writes a cut
  short line from `freeze`; a crash writes none, so a wake with no end
  and no session is one that was cut short.
- **Its own lane.** In the quest log diagram Warriv's wakes sit on a
  lane of their own beside the trunk, a dot a wake in Warriv's own
  colour, a gold that is neither a lamp nor the magic blue. A thin
  line runs from the dot to each quest the wake touched, at that
  quest's row. `chronicle::graph` places them, pure and tested.
  Built on 2026-10-05. `chronicle::wake_dots`, beside `graph`, puts each
  wake in the band of the newest quest accepted before it woke (the
  oldest row when none was), spreads the wakes sharing a band over it
  oldest lowest, and finds the rows of the quests it woke for, ran a
  command about or handed on, by title. The lane sits left of the
  trunk, only when there are wakes, in `theme::warriv`, a palette
  colour of its own in every theme. A dot is filled, ringed when cut
  short, glowing while awake; its lines brighten when it is picked or
  under the cursor.
- **A wake's detail.** Click the dot: when, how long, the events that
  woke it, each command in order, and how it ended, with the question
  for each quest it handed on. Read the session works as for a quest.
  Built on 2026-10-05. The question is the one Warriv asked with `quest
  blocked`, else the quest's blocked reason. A wake with no end whose
  session is gone reads cut short. Read the session writes the wake's
  story over its transcript, read from the project's folder, where
  Warriv works. Carry it on stays latched: a wake is not carried on.
- **A live line on the quests tile.** While Warriv is awake, a line
  under the mode reads "Warriv: settling 2" in the working colour; when
  its six wakes are spent, "Warriv rests until 21:40" in the dim colour.
  Nothing when it is off or asleep with wakes left. The words are pure
  and tested. **Built on 2026-10-05**: `Desk::watch` says which, from
  the events the awake session has not answered plus those queued, or
  the hour's first wake; `Watch::words` says it. The line is a strip of
  its own under the header, right aligned to the mode, shown folded too.

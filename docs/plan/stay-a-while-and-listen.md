# Stay a while and listen

Deckard Cain's catch-up. Asked for after the task list ran by itself for
more than six hours: coming back, the only way to know what had happened
was to read every terminal. The tiles say what is true now, not what
happened while you were gone.

- **When.** On coming back after being away: no real input for 15 minutes
  (`GetLastInputInfo`) or the screen locked (`WTSRegisterSessionNotification`),
  then input again, and only if something happened meanwhile. Also on
  demand from the tray menu and a hotkey, for "since this morning".
- **What it says,** in the order you act on it: sessions waiting on you,
  oldest first; task items ready for review; turns that finished unread
  (see Identify above); items blocked, with the reason; then what simply
  happened: items the runner finished and their commits, branches merged,
  sessions that ended, a usage limit that ran out and when it reset. One
  line each, grouped by project, and a click on a line shows that session
  the way a tile click does.
- **A journal** is what makes it possible, since the registry only knows
  the present. An append only `journal.jsonl` beside `state.json`: a line
  per thing worth telling (a phase into waiting, done or ended, a task
  mark, a runner start, a merge, a limit), with the time, the session and
  its project. Trimmed to a week. `horadric_core::journal` reads it and
  turns the lines since a time into the summary; that is pure and tested.
- **Drawn like the rest,** a panel on the plate in the look of the toasts
  and the picker, not a window of Windows'. Dismissed with Esc or a click
  outside, and it marks nothing read by itself: identifying is still
  looking.
- **Later.** A sentence per session on what it did, not just its last
  message. The last assistant message and the commits say a lot already;
  asking a small model to summarise a run is a step after that, and a
  cost to justify.

Built:

- **The journal** is `journal.jsonl` beside `state.json`
  (`store::journal`, `horadric_core::journal::Entry`). The app writes a
  line when a session's phase changes into waiting, done or ended (a
  session that vanishes counts as ended), compared in `reconcile` against
  the phases last written (`App::journaled`); a session first seen at a
  start or a reload writes nothing. A task list read that changes an
  item's mark writes started, review, blocked or finished
  (`journal::marks`, matched by title). A merge from the menu or the
  notification writes merged, and a usage limit that holds the runner
  writes once per reset. Lines older than a week go at each start.
- **The summary** (`journal::summary`, pure and tested) keeps only the
  last word on each session and each item, so waited, answered and
  finished is one line. What asks something of you is told only while it
  still holds, which the app answers from the registry and the lists.
  Lines go waiting, review, unread, blocked, then what happened, oldest
  first in each, grouped by project, the project with the most pressing
  line first. The account's lines (a limit) stand under "Account".
- **Away** (`catchup::Away`, tested): 15 minutes without input by
  `GetLastInputInfo`, looked at every second, or the screen locked
  (`WTSRegisterSessionNotification`); input within 5 seconds, or
  unlocking, is coming back. Input on the lock screen is not. Coming back
  opens the panel only if something happened, and not while Windows asks
  for quiet (a full screen game, a presentation), as the toasts do.
- **On demand** from the tray's "Stay a while and listen" and
  Ctrl+Alt+Home (Ctrl+Alt+Shift+Home for a dev instance): since local
  midnight, or the last eight hours early in the day. Asked for, it says
  "Nothing happened" rather than staying shut.
- **The panel** (`catchup.rs`, laid out by `layout::catchup`, drawn by
  `render::catchup`) is a plate like the toasts in the middle of the
  primary screen, a lamp by each line in its section's colour and dark
  for what simply happened, the age at the right. As many lines as fit
  the screen, then "and N more". It takes the focus like a setting's
  list; Esc, the cross or a click outside closes it. Windows may refuse
  the focus to an app you are not using, and then it closes when the
  focus moves anywhere. A click on a line with a live session shows it
  as a tile click does.

Tested on screen with a dev instance on its own port and `APPDATA`, a
journal seeded with a finished item, a merge, a limit and a line ten
days old, and three fake sessions posted to it: the old line was trimmed
at start; a permission wait, a `Stop` and a `SessionEnd` each wrote their
line; the hotkey opened the panel with the waiting session first, the
unread turn, the ended one, then the other project's finished item and
merge, and the limit under Account. Not tried on screen: coming back
after 15 minutes or an unlock, clicking a line, and the tray item.

- **Commits under a finished item** are asked of git when the list says
  it is done, not heard as they happen: `git log --first-parent
  --no-merges --since` the item's last started line, in the tree of the
  session that held it (its worktree, or the main tree when it is gone).
  First parent only, since a merge of main into the branch would bring
  every commit main had meanwhile. They go on the finished line
  (`What::Finished`'s `commits`, newest first) and its detail says the
  one, or how many and their subjects. An item the journal never heard
  taken has none. Checked with a dev instance: an item held by a session
  in its own worktree, two commits and a merge of main there, then done;
  the line had the two and the panel said "2 commits: Test the gizmo ·
  Add the gizmo". Commits on main meanwhile were rightly not its.

- **A background session's line** carries a small cloud after its words
  (`theme::BACKGROUND_ICON`), known by its `bg-` id
  (`background::is_tile`), since Claude Code's daemon holds it and a click
  attaches rather than shows a pane of ours.
- **The wheel scrolls** a panel that did not fit, a row a notch as the
  tasks tile does (`layout::catchup_scroll`, tested): down while rows are
  left under the last one showing, up to the top. A panel that does not
  fit takes all the height it may have, so scrolling never resizes it.
  The last line counts what is under it, and at the bottom what is
  above. Checked with a dev instance and a journal of 39 lines: the cloud
  on both background lines, ten notches down stopped with the last line
  showing and "1 more above", four up came back to the top.

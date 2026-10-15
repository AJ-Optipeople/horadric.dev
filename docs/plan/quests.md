# Quests

"Quests" in [IDEAS.md](../IDEAS.md). The task list is a quest log: an item
is a quest, taking it is accepting it, done is completed. Renamed once,
with the old names still read, since the list that runs the work runs
through the rename too.

- **The file** is `.horadric/quests.md`. A project with only
  `.horadric/tasks.md` keeps using it, read and written, until a quest log
  sits beside it; then the quest log wins. Nothing moves the old file: a
  running list keeps its file, and an installed build from before the
  rename, still reading `tasks.md`, sees the same list as the new one. A
  new list is a quest log. `tasks::list_file` picks, pure and tested;
  `horadric_hooks::tasks::file` and `rel` are where it is on disk.
- **The command** is `horadric quest done [SUMMARY]|blocked WHY|add
  TITLE|list`. `horadric task` does the same, so every session started
  with the old prompt still reports back. New prompts say `quest` and name
  the file the project really has, and ask for `quest done "<one short
  line on what you achieved>"`: the words after `done` go into the
  chronicle as the quest's summary (a bare `done` still works). A `quest
  add` from inside a session goes into the chronicle too, as added by
  that session, so the quest log draws the new quest branching from the
  one that session works.
- **The tile** says quests: "New quest", "Accept", "Mark completed",
  "Edit the quest log", and the toasts "Quest log completed" and "N quests
  need you".
- **Editing on the tile.** A quest's right click menu has "Edit quest"
  (the themed input with its title and notes filled in), "Move up",
  "Move down" and "Delete quest", so the log is kept without VS Code.
  Asked for on 2026-09-29. Each changes the file as the rest of the list
  does, found by line and title and doing nothing when the file moved
  under it (`tasks::edit`, `remove`, `shift`, pure and tested). A move
  swaps a quest with the next one, notes and all, and leaves a heading
  between them where it is, so a quest crosses into the next section.
  Delete asks first and is not offered while a session holds the quest:
  put it back first. Checked on screen with a dev instance.
- **A draft survives a click away.** Clicking elsewhere to look something
  up used to throw away the title and notes typed for a new or edited
  quest. Asked for on 2026-10-03. The input now hands what was typed back
  when it is left rather than answered (`ask::Reply::Left`), and the
  runner keeps it, for a new quest by project and for an edit by the
  quest's line and title too (`runner::draft_for`, tested). The next +
  or Edit quest fills it in with the caret at its end. Esc still throws it
  away, and adding or saving clears it. Kept in memory only, so a reload
  forgets it. Checked on screen with a dev instance: type, click away, +
  again, the text is there.
- **The quest giver.** A gold ! left of the plus, the mark over a quest
  giver's head in the games, starts a session named "Quest Giver" in the
  project, on the stage. Asked for on 2026-10-01. Its
  prompt (`tasks::giver_prompt`, tested) has it read the README, the docs,
  the quest log, the git log and the code, change nothing, and suggest three
  to five quests not on the log, each with notes. It asks which to add and
  adds only those, with `quest add "title" --notes "..."`. It suggests
  rather than writes, since a project in auto mode starts whatever lands in
  the log at once. It holds no quest, so it gets no quest system prompt and
  the runner leaves it alone. Checked on screen with a dev instance and
  `cmd.exe` as the agent: the session starts and takes the stage.
- **Left alone**: the `"tasks"` key in `config.json`, the
  `/horadric/tasks` path, its header and `HORADRIC_TASKS`. They are what
  builds on either side of the rename say to each other, and no one reads
  them.

Not checked on screen: the tile's words are the only change there.

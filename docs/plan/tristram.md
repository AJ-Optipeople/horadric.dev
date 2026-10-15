# Tristram

Asked for on 2026-10-15. The human works in many projects, but some
work belongs to none of them: a broad question, an errand across every
project, the first talk about a project that does not exist yet (mrr
and socialsandbox started that way). They also like the browser pane
with an agent beside it as a general assistant. Doing that inside a
project's stage feels wrong, because the project has nothing to do
with it.

Tristram is the town above the projects. The place every act starts
from: always there, belongs to no project, sees all of them.

## What it is

- **A project Horadric owns.** Its folder is
  `%USERPROFILE%\Horadric\tristram` (dev: `Horadric-dev\tristram`),
  made on first start. Sessions there are ordinary sessions keyed by
  that folder, so the stage, panes, browser pane, hooks, history and
  the stash all work unchanged. The only new thing is that Horadric
  knows this key is the town.
- **Always shown.** Its cluster is pinned first in the columns and
  stays when it has no sessions, drawn as the town (its own mark, not a
  project name) so it never reads as one more repository. Clicking it
  with no sessions opens its stage with the browser pane and a "New
  session" prompt, so the assistant is one click away.
- **Its own browser pane.** The town's pane is the human's own browser
  with an agent beside it. Logins carry over, since every pane already
  shares one profile (see "Browser pane"). Project panes stay as they
  are, because agents testing their own site need them.
- **Not a git repository.** No files tile, no git glance, no worktrees,
  no tombs. It has a quest log, for errands that span projects.

## What the town agent knows

Horadric writes `CLAUDE.md` in the town folder on start and whenever
the set of projects changes. Generated, so a hand edit is lost; the
human's own notes go in `NOTES.md` beside it, which the generated file
tells the agent to read. It holds:

- Every project Horadric knows (from state and history): its name, its
  folder, the first lines of its README or CLAUDE.md, when a session
  last ran there.
- That this is the town: it does not edit code in projects. It reads
  them, answers, and files quests for the project's own sessions to do.
  Projects run in trunk mode with other sessions in them, so a town
  agent editing there would be one more writer nobody planned for.
- The commands below.

Which projects to list and how to describe them is a pure function
(`town::brief`), tested.

## Seeing and acting across projects

The quest CLI works on the project of the current folder. The town
needs it to work on any project.

- `horadric quest add --project <name or folder> "<title>"`, likewise
  `list`. A name matches a known project's folder name; an ambiguous
  one is refused with the matches listed.
- `horadric projects`: every project, its folder, its sessions and
  their phases, open quests by count, last commit. One call answers
  "what is going on everywhere?" without the agent walking folders.
- Reading a project's files needs nothing new: the paths are in
  `CLAUDE.md`, and Claude Code reads outside its folder when allowed.
  The town session starts with `--add-dir` for each project folder so
  reads need no prompt. Writes there stay the human's call.

## An idea becomes a project

A town session that turns into a real thing gets "Found a project" in
its tile menu (and `horadric found <name>` for the agent). It asks for
a name and a parent folder (default: the parent of the last project
made), makes the folder, runs `git init`, writes a first README from
what the session settled, and starts a session there with a summary of
the town conversation as its first prompt. The new cluster appears.
The town session stays in town, ended or kept as the human likes.

## Order

Each is a quest.

1. The town folder, the pinned cluster that stays empty, its stage
   and browser pane. Useful on its own. **Done.** `town.rs` owns the
   folder and the key, `town::make` runs at start. The cluster is named
   Tristram by `project_name`, drawn with a house mark, pinned by
   `Columns::pin_first` above the usage window (dragging it away puts
   it back), has no files watcher and no "Close project". A header
   click while it is empty opens the stage with its browser pane; the
   plus under the header is the new session. The start window no
   longer counts the town as a project, so it still shows, under the
   town, when nothing else is open.
2. The generated `CLAUDE.md` and `--add-dir` for project folders.
3. `--project` on the quest CLI and `horadric projects`.
4. "Found a project".

## Open

- One town or several (work and private)? One until the human asks.
- Should a project session be able to hand a question up to the town
  ("ask in Tristram")? Not until the town has been used for a while.
- macOS: the folder moves to `~/Horadric/tristram`; nothing else in the
  plan depends on Windows.

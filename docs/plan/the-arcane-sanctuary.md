# The Arcane Sanctuary

Asked for on 2026-10-15. The human likes how Obsidian draws a vault as
a graph: dots for notes, lines for links, hubs that grow large, orphans
drifting on an outer ring. The lore (see "Lore") gave Horadric the same
kind of map of each project. The Arcane Sanctuary draws it, and lights
up what the agents do in it as they do it. It is a thing to look at,
not a tool: nothing depends on it, and it must cost nothing when it is
not shown.

Named for the floating paths in the void in Act 2.

## What it shows

- **Nodes.** The project's files (from `git ls-files`, as the code index
  lists them), sized by how many links they have, so hubs like `app.rs`
  read as hubs. Lore notes and quests are nodes too, each in a colour of
  its own, smaller and dimmer than files.
- **Links.** File to file where one calls a symbol the other defines
  (the code index's calls, resolved ones only, weighed by how many). A
  note to the files it names. A done quest to the files it touched (its
  `touched` record).
- **Orphans.** A node with no links is pushed to an outer ring, as in
  Obsidian, rather than left in the middle.
- **Labels** only on hover and on the few largest hubs, so the picture
  stays a picture.

## What lights up

Live, from what Horadric already hears. Nothing parses terminal output.

- A file a session reads glows softly in that session's colour and
  fades over about a minute.
- A file a session edits or writes pulses bright and fades slower.
- A new file is born with a flash at the edge of the cluster it links
  to; a deleted one fades out and its links with it. The files watcher
  that feeds the files tile says which.
- A `lore_recall` lights the notes it answered with, and a quest's
  context pack lights its files when the quest starts, so the human can
  watch an agent remember.
- A link between two lit nodes carries a spark from the one read first
  to the one read after, so a session's path through the code shows.

The events come from `PreToolUse` and `PostToolUse` (the tool and its
file path, as `overlap.rs` already reads them) and from the MCP server
for the lore tools.

## Where it lives

A pane on the stage, like the browser pane, so it sits beside the
sessions and the browser while work goes on, and it has the zoom
button in its header that every pane has, to fill the stage. One per
project. It is opened from the cluster header's menu and from a key on
the stage, and it is kept in the grid order like any pane.

In Tristram it shows every project at once: each project a galaxy of
its own, placed apart, lit by all sessions everywhere. A click on a
galaxy goes into that project's sanctuary.

## How it is drawn

Direct2D, as all of Horadric's own UI is: no web view and no d3. A few
thousand dots and lines is easy for Direct2D.

- **Layout** is a force simulation (links pull, all nodes push, a weak
  pull to the middle), with Barnes Hut for the push so it stays fast
  past a thousand nodes. It is a pure function from graph and previous
  positions to new positions, tested. Positions are kept per project
  in the state folder, so the picture does not reshuffle on every open;
  a new node starts beside its first link and settles in.
- **Motion** runs on the vsync clock only while something moves: the
  layout settling, a glow fading, a spark travelling. When everything
  is still the pane draws nothing, so an open sanctuary costs no CPU.
- **Pan** by dragging, **zoom** with the wheel toward the cursor. Hover
  shows the name and, for a file, who touched it last.
- Colours follow "The look": session colours as on the panes, the
  background the stage's, dark and light.

## Order

Each is a quest.

1. The graph and its layout: the pure part. Build the graph from the
   code index, the lore notes and the quests' `touched` records; the
   force layout with Barnes Hut; positions kept per project. Tested.
   Done 2026-10-15: `horadric_core::sanctuary` (the graph, `Layout` to
   step it, `layout` to run it to still), `code::links` for the calls,
   `Place::positions` and `keep_positions` beside the notes
   (`<project>.sanctuary.json`), and `horadric sanctuary` to try it.
   This repo: 390 nodes, 1620 links, laid out in 57 ms from nothing.
   Only calls that resolve to one file make a link; a kept picture with
   nothing new is not moved at all.
2. The pane: drawn on the stage beside sessions and the browser, zoom
   to fill the stage, pan, wheel zoom, hover, opened from the header
   menu and a key, still when nothing moves.
3. Lighting it up: reads, edits, new and deleted files, lore recalls,
   context packs and the sparks, in each session's colour.
   Built 2026-10-15, the data side: `horadric_core::glow` keeps a
   project's recent touches and says how each node is lit, how far a
   deleted one has faded, which sparks are on their way and whether
   anything still moves. Reads glow a minute, edits three, a new file
   flashes, a deleted one fades out in four seconds; a session going
   from file to file within 20 seconds sends a spark that takes 1.2.
   The app keeps one per project in `glows` and the feeder fills it
   from `PreToolUse` and `PostToolUse` (a Write of a file not there yet
   is a new file, a file a shell command names that is gone after it
   is a delete), worktree paths mapped to the main tree's. The lore
   tools pass what they showed to the app over `/horadric/glow`, read
   from the answer the agent got, and a quest's pack is lit where it
   is recorded. Panes have no session colours, so a session's colour
   is one of eight picked by its id, the same in every process. The
   files watcher is not used: hooks say who, and it does not.
4. Tristram's sky: every project as a galaxy in the town's sanctuary,
   a click going into one.
   Built 2026-10-15, the data side: `horadric_core::sky` places each
   project's laid out graph as a galaxy apart in one flat picture of
   stars and links, with `hit` for a click and `frame` for zooming into
   one. A galaxy grows with the fourth root of its node count, so a
   project of thousands of files is a few times a small one, not all of
   the sky. The largest sits in the middle, each other on a ray of its
   own picked from its key, slid out until it touches none placed
   before, so a project that comes or goes moves only smaller ones.
   `lore::town_sky` charts every project `horadric projects` lists,
   laying out and keeping any never drawn, and keeps each in memory
   until a file its graph is read from changes. `horadric sky` tries
   it. Here: 7 projects, 2052 stars, 3982 links; 9.5 s the first time
   ever (indexing the code of projects never indexed), 0.9 s cold after
   that, 1.4 ms again from memory. The pane should chart it off the UI
   thread the first time. Drawing it in the town's pane is left for
   after step 2.

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
2. The pane: drawn on the stage beside sessions and the browser, zoom
   to fill the stage, pan, wheel zoom, hover, opened from the header
   menu and a key, still when nothing moves.
3. Lighting it up: reads, edits, new and deleted files, lore recalls,
   context packs and the sparks, in each session's colour.
4. Tristram's sky: every project as a galaxy in the town's sanctuary,
   a click going into one.

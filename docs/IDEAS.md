# Ideas

Things worth building one day, most of them from the name. Horadric is
named after the Horadric Cube in Diablo II, the box you drop items into
to transmute them into something better. None of this is planned. When
an idea becomes work, it moves into [PLAN.md](PLAN.md) and leaves this
page.

The rule for all of it: the theme has to earn its place by making
something clearer or quicker. A joke that gets in the way of reading a
tile is out.

Built ideas are gone from this page (2026-10-19). What was decided about
them is in the lore: ask `lore_recall` about transmute, identify, the
stash, the tombs, rarity colours, loot drops, runewords, quests,
experience, the Cow Level or Town Portal.

## New recipes

The cube is built but off by default, since its three recipes mostly
repeat what a tile's menu does. These are what it could do that no menu
could. Each is about putting things together and getting something new
out, and about running several agents at once, which is what Horadric
is for.

- **Fuse.** Two sessions go in, one new session comes out that knows
  what both learned. For two threads that drifted into the same problem.
- **Teach.** A, then B. A writes down what it found out (the gotchas,
  the dead ends) and that goes into B, so what one agent learned reaches
  another without you telling it again.
- **Conflict check.** Two sessions go in and the cube says whether their
  branches will clash before either merges: the files both touched, or
  a gold "clean".
- **Forge.** One session goes in and forks into three, each trying a
  different approach from the same point. When all three are done the
  cube reviews them and hands back the winner. Best of three on demand.
- **Wait for.** A session goes in with a condition (CI green, another
  session done, nine in the morning) and wakes by itself when it holds.
- **Ingredients.** Files from Explorer, a screenshot or a URL go in with
  a session and are handed to it as context. Without a session, a new
  one starts about them.
- **Extract.** A finished session goes in and a lesson comes out: a line
  for CLAUDE.md, a memory, or a skill. The session ends; what it learned
  stays.

Forge is the strongest: it orchestrates several agents, and dragging
really is quicker than a menu for it.

## Driving a phone

The agent driving a phone it is testing an app on, through adb or a
simulator over MCP. A tool for the agent, like the browser pane, not a
remote tile. Split out of the Brotherhood.

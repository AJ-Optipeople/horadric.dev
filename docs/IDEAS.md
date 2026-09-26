# Ideas

Things worth building one day, from the name. Horadric is named after the
Horadric Cube in Diablo II, the box you drop items into to transmute them
into something better. None of this is planned. When an idea becomes work,
it moves into [PLAN.md](PLAN.md) with its reasoning, and its entry here
says so.

The rule for all of it: the theme has to earn its place by making something
clearer or quicker. A joke that gets in the way of reading a tile is out.

## Useful

### Transmute

Drag tiles onto a cube and a recipe runs on them. The cube could be an
actual cube, drawn in 3D and turning slowly, sitting in a corner of the
column or the stage, opening its lid when a tile is dragged over it.

Recipes to start with:

- Two finished sessions: start a reviewer on both diffs.
- One session and `main`: merge its branch.
- Three idle sessions: close them and keep a one-line summary of each.

A recipe is a small, named, combinable action, the same shape as the
runewords below. The two could be one system: recipes for things you
combine, runewords for sequences.

Built, with these three recipes: see "Transmute" in [PLAN.md](PLAN.md).

### Identify

In Diablo an unidentified item drops grey and you do not know what it is
until you identify it. A session that finished a turn you have not looked
at stays "unidentified": its tile marked until you have seen what it did.
It answers "did I read that yet?" across ten tiles.

Being built: see "Identify" in [PLAN.md](PLAN.md).

### Stay a while and listen

Deckard Cain's catch-up, for coming back after hours away. Asked for after
the task list ran on its own for more than six hours and there was no way
to see what had happened except reading every terminal. What finished,
what is waiting on you, what is blocked, and in what order to look.

Planned: see "Stay a while and listen" in [PLAN.md](PLAN.md).

### The stash

A small grid, three by three, where sessions go that you deliberately want
to keep for later. History already has every session, but history is
everything; the stash is the few you chose. A stashed session is paused and
out of the columns, and a click brings it back as it was.

Built: see "The stash" in [PLAN.md](PLAN.md).

### Tal Rasha's tombs

Seven tombs and only one is real. Start the same task in several sessions
at once, each in its own worktree, and pick the winner; the others fade out
and their worktrees are cleaned up.

Built: see "Tal Rasha's tombs" in [PLAN.md](PLAN.md).

## Visual

### Item rarity colours

How a session ended, in Diablo's item colours:

- White, normal: it ended and changed nothing.
- Blue, magic: it made changes.
- Yellow, rare: it made changes and the tests pass.
- Gold, unique: its work landed on `main`.
- Green, set: sessions from one batch, such as parallel list items or
  Tal Rasha's tombs.

This has to live beside the phase lamps without being read as a phase. See
"The look" in PLAN.md: phase is light, project is accent. Rarity would be a
third job and needs its own place on the key, likely the name's colour.

Built as the name's colour: see "The look" in [PLAN.md](PLAN.md).

### Loot drops

When a session finishes, a beam of light rises from its tile, like an item
dropping, with a short sound. A high chime, like a rune dropping, when its
work lands on `main`. The sounds have to be our own, never Blizzard's.

Built, sounds and all: see "Life in the tiles" in [PLAN.md](PLAN.md).

### The transmute animation

Tiles swirl into the cube and something comes out, when a recipe runs or a
batch closes.
Built for a recipe running: see "Transmute" in [PLAN.md](PLAN.md). A batch
closing does not play it yet.

## Gamification

### Runewords

Sequences that combine into something useful, the way runes in the right
order make a runeword. Like the recipes, but over time instead of at once:
"test, review, merge" as one named sequence a session can be given. See
Transmute above; the two may be one system.

### Quests

The task list could become a quest log. Rename Tasks to Quests, and make
it read like one: an item is a quest, taking it is accepting it, done is
completed, blocked is a quest you cannot finish yet. The tile, the command
(`horadric quest done`) and the file (`.horadric/quests.md`) would all
change, so it is a rename worth doing once, deliberately, with the old
names still read.

### Experience

XP per merged commit and a level in the tray menu. On its own it is a game
only one person plays, so it is not worth it for now. Worth it if Horadric
goes public and there is a leaderboard to put it on.

### The Cow Level

There is no cow level. An easter egg: a hidden way in, perhaps forty
sessions at once or a secret recipe in the cube, and a portal opens.

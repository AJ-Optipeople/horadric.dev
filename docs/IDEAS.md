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

Built, but the three recipes mostly repeat what a tile's menu does, so the
cube is off by default until it holds something only it can do (see "New
recipes" below).

### New recipes

What the cube could do that no menu could. Each one is about putting things
together and getting something new out, and about running several agents at
once, which is what Horadric is for.

- **Fuse.** Two sessions go in, one new session comes out that knows what
  both learned. For two threads that drifted into the same problem.
- **Teach.** A, then B. A writes down what it found out (the gotchas, the
  dead ends) and that goes into B, so what one agent learned reaches
  another without you telling it again.
- **Conflict check.** Two sessions go in and the cube says whether their
  branches will clash before either merges: the files both touched, or a
  gold "clean".
- **Forge.** One session goes in and forks into three, each trying a
  different approach from the same point. When all three are done the cube
  reviews them and hands back the winner. Best of three on demand.
- **Wait for.** A session goes in with a condition (CI green, another
  session done, nine in the morning) and wakes by itself when it holds.
- **Ingredients.** Files from Explorer, a screenshot or a URL go in with a
  session and are handed to it as context. Without a session, a new one
  starts about them.
- **Extract.** A finished session goes in and a lesson comes out: a line
  for CLAUDE.md, a memory, or a skill. The session ends; what it learned
  stays.

Forge is the strongest: it orchestrates several agents, and dragging
really is quicker than a menu for it.

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

### Town Portal

Horadric from your phone, so work goes on wherever you are: see every
session on the PC, answer it, and start new ones. Asked for on
2026-10-01.

Much of it is there already. Each session's host keeps the last 4 MB of
output and speaks five messages (input, resize, output, exit, kill), and
the registry holds every session's phase from the hooks. A phone needs
the session list, a stream from a host's ring, and a way to send input
back. The UI passes these on, since the host pipes refuse remote
clients.

- **A PWA first, a native app maybe later.** A web page needs no app
  store and works on iOS and Android, with xterm.js for the terminal.
  The human waived the "no web view" and "dependencies one at a time"
  decisions for this on 2026-10-01.
- **The page is hosted, not served by the PC.** The goal is any browser
  anywhere, with nothing installed first (decided 2026-10-02). So the
  page lives on a public site of ours, such as `app.horadric.dev`, and
  reaches the PC through the relay. The site sends only the HTML and
  JS; sessions never pass through it. One fixed HTTPS address also
  makes "add to home screen" and Web Push simple. The page and the
  relay live in a repo of their own, since both are deployed to the
  internet and neither ships with the desktop app. This repo gets the
  PC's side: the API, pairing and the connection out to the relay.
- **The page must cope with older PCs.** The hosted page always runs
  the newest version, while a PC may run an old Horadric for weeks. The
  PC says which API version it speaks when it connects, and the page
  supports a range of versions, or tells the human to update the PC.
- **An API, not pages.** The PWA is written as if it were a third party
  client, against a versioned API: JSON over WebSocket for events and
  session state, binary frames for terminal output. Then a native app is
  a second client of the same server, and the server, the protocol, the
  pairing and the network all carry over. Only the UI is written again,
  and push gets a second sender (APNs and FCM beside Web Push), while
  the part that decides when to notify stays.
- **Every PC is its own server.** It has to scale to thousands of
  users, and it does by having no centre: each Horadric serves its own
  few phones, so a thousand users is a thousand small servers we never
  run. Nothing we host holds sessions or state.
- **Reaching the PC: a relay of ours.** Tailscale only works on a
  device where it is installed, so it cannot give "any browser
  anywhere". The PC connects out to the relay, so no port is opened on
  the PC or the router. The relay is stateless: it only joins a browser
  to its PC, and the two encrypt end to end so it never reads a
  session. A relay like that scales sideways, more instances behind a
  load balancer. Tailscale can still be a way in for development.
- **Push needs one small service of ours.** Web Push the PC can send by
  itself, but an iOS app's APNs key is a secret no install can carry, so
  native push goes through a sender we host. It forwards a "session waits"
  with no content in it, so it holds nothing worth stealing.
- **Off by default.** This is remote control of a machine whose agents
  may run with permissions bypassed. Each phone is paired by a device
  token the human approves on the PC, and the PC listens on no port: its
  only way in is its own outgoing connection to the relay.
- **An inbox first, not a terminal.** Typing into a terminal on a phone
  is slow. Away from the desk the need is "2 sessions wait for you":
  approve a permission, answer a question, send a short prompt, and a
  push notification when one waits. Then a live terminal to read, then
  starting a session in a project. Step 5 in the plan names an inbox
  already, and the two should be one.

Claude Code's own Remote Control covers a single Claude session. This
covers the fleet: every project, Codex and Grok Build too, and starting
sessions.

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
Built, for a recipe running and for a batch of tombs closing: see
"Transmute" in [PLAN.md](PLAN.md).

## Gamification

### Runewords

Sequences that combine into something useful, the way runes in the right
order make a runeword. Like the recipes, but over time instead of at once:
"test, review, merge" as one named sequence a session can be given. See
Transmute above; the two may be one system.

Built, as one system with the recipes: see "Runewords" in [PLAN.md](PLAN.md).

### Quests

The task list could become a quest log. Rename Tasks to Quests, and make
it read like one: an item is a quest, taking it is accepting it, done is
completed, blocked is a quest you cannot finish yet. The tile, the command
(`horadric quest done`) and the file (`.horadric/quests.md`) would all
change, so it is a rename worth doing once, deliberately, with the old
names still read.

Built: see "Quests" in [PLAN.md](PLAN.md).

### Experience

XP per merged commit and a level in the tray menu. On its own it is a game
only one person plays, so it is not worth it for now. Worth it if Horadric
goes public and there is a leaderboard to put it on.

### The Cow Level

There is no cow level. An easter egg: a hidden way in, perhaps forty
sessions at once or a secret recipe in the cube, and a portal opens.

Built, as a secret recipe: see "The Cow Level" in [PLAN.md](PLAN.md).

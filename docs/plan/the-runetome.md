# The Runetome

Asked for on 2026-10-01. Runewords become shortcuts: programmable
buttons that do anything, from one prompt to a chain of prompts, keys
and commands. Each is a rune stone with a generated runeword carved on
it, kept in a tile of its own, the Runetome. Builds on "Runewords"
above, whose engine (one step a turn, the human taking over stops it,
saved in `state.json`) stays.

- **The tile.** One Runetome a project, in its cluster beside the
  quest log, shown whenever the project has a cluster. Its stones sit
  in rows, the built in ones first, then the project's, then the global
  ones, and last an empty stone. (The built in ones were "Test, merge",
  "Test, review, merge" and "Review, merge" until the feedback below.)
- **A stone** is drawn in Direct2D: a rough rounded slab, lit from the
  top left like the cube, with a glyph cut into it and its label under
  it. The glyph and a runeword name ("Tal Eth Ko", two to four of the
  33 rune names) both come from a hash of the stone's label, so a
  stone keeps its look when its steps are edited and two stones rarely
  match. `runeword::carve` (strokes from the hash) and `runeword::name`
  are pure and tested. Hovering a stone shows its name and its steps
  in a tooltip (tip.rs), so what a click does is never a guess.
- **Steps.** A stone is a list of steps, cast in order:
  - `say`: typed to the session, done when its turn ends (the said
    rune of today).
  - `keys`: keystrokes into the session's terminal at once, such as
    `"Esc"`, `"Ctrl+C"` or `"/clear{Enter}"`. Parsed by a pure, tested
    `runeword::keys`. Done once written; it waits for no turn.
  - `run`: a command run with `cmd /c` in the project's folder (or the
    session's worktree when it has one), hidden. Done when it exits; a
    non zero exit stops the runeword with a toast carrying the
    command's last line of output. `"show": true` runs it in a plain
    terminal pane on the stage instead, for a command worth watching.
  - `test`, `review`, `merge`: the runes as they are.
- **Keys stay inside Horadric.** A `keys` step only reaches Horadric's
  own terminals. Input sent to other programs' windows is fragile and
  fights the window manager; anything outside Horadric is a `run` step
  (a script, AutoHotkey, `start ms-settings:`), which needs no new
  dependency.
- **Casting.** A stone with any step that needs a session (`say`,
  `keys`, `review`, `merge`, `test`) casts on the session focused on
  the stage when that session is this project's, and asks "Cast on
  which session?" with the project's sessions otherwise. Dragging a
  stone onto a tile or a pane casts on that one, the way a tile goes
  into the cube. A stone of only `run` steps needs no session and
  casts at once, with the project's folder as its directory. A
  sessionless runeword lives on the project rather than a session, so
  it is saved beside the sessions in `state.json` and goes on through a
  reload too.
  A session casts one runeword at a time, and any other stone cast on
  it says to stop that first, except a stone of only `keys` steps:
  keys wait for no turn, so they are typed in beside the runeword
  already running, which is how a permission prompt that holds it up
  is answered from the tome without stopping it.
- **While one runs** the stone glows in the cube's gold and shows its
  step ("2/4"); a click on it then offers Stop. The tile it casts on
  shows "rune 2/4" as today. The tile menu keeps "Stop <name>" and
  loses the "Runeword" submenu, since the tome is where they are given.
- **Where stones live.** A project's in `.horadric/config.json`, every
  project's in `%APPDATA%\Horadric\runewords.json`, same shape:

  ```json
  { "runewords": {
      "Fresh start": { "steps": [ { "keys": "/clear{Enter}" },
                                  { "say": "Read docs/PLAN.md and take the next quest" } ] },
      "Open the site": { "steps": [ { "run": "start http://localhost:3000" } ] },
      "Ship": ["test", "Update the changelog", "merge"] } }
  ```

  The list form of today is still read: a bare word is a rune or a
  `say`. The tome reads both files again when they change, so a stone
  an agent adds appears without a restart. `runeword::parse` is pure
  and tested, and a stone that does not parse shows cracked, with the
  reason in its tooltip, rather than vanishing.
- **The empty stone** makes new ones, the way the quest giver makes
  quests. A click starts a session named "Runesmith" in the project, on
  the stage. Its prompt (`runeword::smith_prompt`, tested) says what a
  stone is, the step kinds, both files and their shape, and asks the
  human what the stone should do and whether it is for this project or
  every one. It writes the stone, then runs `horadric runeword list` to
  check it parses, and reports the stone's runeword name. Agents make
  stones; a human never has to write the JSON, though they can.
- **Trust.** A `run` step is any command, and a project's config comes
  with its repository, so a cloned project can carry stones. Nothing
  runs without a click, and the tooltip shows the command before it.
  A stone from a project's config whose steps changed since it was
  last cast shows a small mark until it is cast once, so a pull that
  changes a command is seen.
- **Left alone**: the cube and its recipes, and how runes are cast
  turn by turn.

The engine is built (2026-10-01). `runeword::parse` reads both forms
and every step kind, `runeword::stones` lays out built in, project and
global stones with a cracked one's reason, and `runeword::keys` turns
a spec into pieces written 400 ms apart, a run of text one piece and
each key in braces one of its own, so `/clear{Enter}` lands as typed.
A `run` step goes through `horadric runestep <file> [--show]
<command>`, started out of the app's job, which writes the exit code
to `<file>.exit` (and, hidden, the output to `<file>.log`) under
`runes` in the app's folder. That file is how a build after a reload
learns how a command it did not start ended. A shown one runs in a
plain pane that waits for Enter after a failure. Stones of only `run`
steps cast on the project (`OnProject`, saved as `runewords` in
`state.json`). The global file is `runewords.json` beside the state,
in `Horadric-dev` for a dev instance, and both files are read again
when their time or size changes. Until the tome existed, the tile
menu's Runeword submenu offered every stone that parsed. Checked on a dev instance with `cmd.exe` as the agent: `keys` typed
`echo ...{Enter}` and cmd ran it, keys then a hidden `run` wrote its
file in the project, a failing command toasted "it exited with 3:
boom went the command", a shown one opened a pane and closed it on
exit 0, the global stone ran, and a 30 second command cast before a
`reload` finished after it, the new build completing the runeword.

The tile is built (2026-10-01). It sits under the quest log in every
cluster of a project with a folder, folded by its header like the
others, the fold kept with the cluster. The app reads each project's
stones once a second (both files only when they changed) and hands the
tiles what moved: a stone written, one cast, one done. A stone's tooltip
is `runeword::tip`, its runeword name then its numbered steps, or why it
is cracked; the tooltip plate now takes a line made at run time as well
as a fixed one. The mark is a small amber dot. It is on a project's stone
whose steps are not the ones last cast, a stone never cast included, so
a stone a clone or a pull brings shows it before anything runs; built in
and global stones never carry it. The steps last cast are kept as
`runeword::fingerprint` (FNV-1a over the steps as JSON) in `state.json`
as `stones_cast`. A click casts at once a stone of only commands, or on
the session with the keyboard on the stage when it is this project's,
and otherwise asks "Cast on which session?" with the project's live
sessions; a paused, background or plain terminal one cannot take a
stone. A session that is casting already is not given a second
runeword: a toast says to stop the first. A drag that lets go over a
tile in any cluster or a pane on the stage casts there, the window on
top at that point deciding, so a stone dropped on a tile under another
window casts nothing. While a stone is cast it glows and its label reads
"1/3" in gold, and a click offers "Stop <label>" for each session or the
project casting it. The empty stone starts "Runesmith" as the quest
giver starts its session. The tile menu lost its Runeword submenu and
its stops for project casts, and keeps "Stop <name>" for the session's
own.

Checked on a dev instance with `cmd.exe` as the agent and a scratch
project with a stone of each kind: a hidden `run` of only commands wrote
its file at a click and its dot went; `keys` typed into the focused
session; `say` glowed "1/1" and the tile read the rune, and the click's
menu stopped it; a shown `run` opened a pane that closed on exit; keys
then a `run` did both; a global stone ran. With the stage on another
project, a click asked which session and cast on the one picked. A stone
dragged onto a pane on the stage and onto a tile in the other project's
cluster cast on each. Then with a real `claude` (Haiku) the empty stone
started the Runesmith: it read its prompt, was answered by a `say` stone
cast on it, wrote "Hello file" into the project's config, and the stone
showed on the tile with its dot within the second; a stone dragged onto
its pane approved its permission prompt, and a click on the new stone
wrote `hello.txt`. No `claude.exe` of the dev instance was left after.
What it showed:

- A tile added above the tome, a shown command's pane or a new session,
  moves every stone down. A stone pressed as the layout changes is the
  one now under the cursor.
- Claude Code asks whether to trust a folder it has not seen, and a
  stone of keys is how to answer it from the tome: `{Down}{Enter}` there,
  `{Enter}` for a permission prompt.

## The human's feedback (2026-10-02)

After using the tome: no way to remove a stone, built in stones nobody
would click, and a click that casts without asking. What changed:

- **Built in stones that show what a stone can do**, one or two of each
  step kind, each worth a click on day one: Approve (`{Enter}`, which
  slips in beside a runeword held up on a permission prompt), Interrupt
  (`Esc`), Recap and Commit (`say`), Fresh start (`/clear{Enter}` then a
  `say`, a chain), Second opinion (the review rune) and Open folder
  (`run start "" .`, sessionless). A file's stone with a built in one's
  label takes its place, as one with the same steps did already.
- **`"about"`**: a stone in its object form may say in a sentence what
  it is for. The tooltip, `horadric runeword list` and the question
  before a cast show it, and the Runesmith is told to write one.
- **A click asks first.** "Cast <label>?" with what it is for, what it
  casts on and every step, Cast or Not now, and a "Do not ask again"
  check (`cast_without_asking` in `state.json`). The dialog took a check
  for it, beside its buttons. A stone with a command is asked in the
  warning tone. A project stone whose steps are not the ones last cast
  asks even when told not to, since that is the trust mark. A pick from
  "Cast on which session?" and a drag are not asked again: the pick and
  the drop were the human saying so.
- **Right click a stone** for its menu: its label and runeword name,
  Cast (or Stop while cast), and for a stone in a file "Change with the
  Runesmith" (`runeword::reforge_prompt`, the smith told which stone and
  file) and "Remove", which asks, then takes it out of the file with
  `runeword::unwrite`. That edits the text in place rather than through
  serde, whose maps are sorted (no `preserve_order`), so every other key
  and all the spacing stay as written, a BOM too. A built
  in stone offers "Put away" (`stones_hidden` in `state.json`). Every
  menu, and the one on the empty stone or the tome's header, has "Ask
  before a click casts" and, once one is put away, "Bring back".
- **Drag a stone within the tome to move it** (asked for 2026-10-02).
  While carried over the tome it shows in the place it would take
  (`layout::stone_slot`, the nearest stone's, the last over the empty
  stone), the others closed up around it; let go there and it stays.
  Let go outside the tome and it casts as before. The order is kept per
  project as labels (`stones_order` in `state.json`), laid out by
  `runeword::arrange`, so built in, project and global stones mix
  freely and a stone the order does not name, a new one, shows last.
  Checked on a dev instance: a move, a drop on the empty stone and a
  drop on a tile, which cast.
- **Found on the way**: a toast about a session casting already named the
  project rather than the session (`on_label` without a session in hand
  looked the id up as a project key).

Checked on a dev instance with a scratch project whose config was written
with a BOM: the click asked with the steps and the dot's note, the check
ticked and was kept, the command ran; the right click menu removed a
project stone, leaving the file byte for byte but for the stone and its
comma, BOM included; Approve was put away and brought back from the
header's menu.

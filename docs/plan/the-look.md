# The look

The first look was a default dev tool: flat boxes, one type size, a phase
as a tinted fill, nothing moving. Asked for something with its own
identity. The rule behind it: colour has two jobs and they never share a
place. A phase is light, in a tile's lamp and icon. A project is an
accent, on its mark and on the stage's edge. So a project's colour can never
be read as a session needing you.

The look went through three versions. First deep dark glass: acrylic
behind every cluster, tiles a breath of white. Then clay (commit
`d0354b5`): soft moulded surfaces, raised or pressed in, which made the app
distinct but never sat right beside the terminals. Now a hardware control
panel in the dark, the metaphor that fits: sessions are keys with lamps,
and a terminal is a screen set into the panel.

- **Metal, keys, screens.** Every window is a matte faceplate, a shade
  lighter at the top where the light falls, with a groove cut round it
  and another under the header (`Painter::plate`, `Painter::engrave`). No
  texture: the reality is in bevels, shadows and light. A session is a key
  (`Painter::key`): a face lit from above, a bevel along its top edge, its
  side showing below the face, its shadow on the plate. The plus bays are
  sunk into the plate with a dashed edge, and a key rises into one under
  the cursor. Anything that scrolls sits on a screen sunk into the plate:
  the files, the usage limits. The header counts are small LEDs, the plus a
  round push button, limits and context segmented meters. The stage's
  faceplate is painted with the same light (`TerminalWindow::paint_plate`,
  a GDI gradient and seam), and each pane is a screen set into it: a bezel
  of plate that continues the stage's light from where the pane sits, the
  glass sunk in with rounded corners and shade under its top edge, the
  name printed on the plate above it beside the session's lamp, and the
  glass rimmed in the project's colour on the pane with the keyboard
  (`glyphs::grid_origin`, tested). The terminal's black is the screens'
  glass and its red, green, yellow and blue are the lamps' colours. No
  acrylic any more, so every window is opaque and its text ClearType.
- **Soft shadows without blur.** Direct2D's hwnd targets have no effects,
  so a soft edge is eight copies of a shape, each a little bigger and
  fainter (`blur_steps`, tested). A shadow inside a shape is the outside of
  a moved copy, drawn as a wide stroke under a layer clipped to the shape
  (`Painter::inner`, `Painter::hollow`).
- **Phase as light.** Each key has a lamp down its left edge
  (`theme::lamp`). Working is blue with a hot spot running up and down it.
  Waiting breathes amber and backlights its whole key, light spilling out
  under it, and sends a ring off itself the moment it starts waiting. Done
  is green and flashes once. Idle, paused and ended lamps are dark glass,
  and paused and ended keys are latched down (`theme::depth`) and fade back
  (`theme::presence`).
- **The selected key is held in.** The session whose pane on the stage has
  the keyboard has its key latched in level with the plate, like the one
  button held down on a tape deck: out of the light, shade falling in over
  its top edge, the plate's lit lip under it (`theme::key_depth`,
  `Painter::latched`). It is fully lit, so it never reads as paused. The
  stage keeps `Shared::active` and a change sends `Input::Spotlight`, which
  redraws the clusters, so the latch follows a tile click, a pane click
  and a keyboard move between panes.
- **Project accents.** Eight colours chosen away from every phase colour,
  picked by an FNV hash of the project key (`theme::accent`), so a project
  keeps its colour across runs. It marks the cluster's name, washes faintly
  down from its top edge, rings the cluster whose project is on the stage,
  tints the stage window's border (`DWMWA_BORDER_COLOR`, the accent sunk
  two thirds into the dark) and underlines the pane with the keyboard. All
  of them muted: an accent says which project, it does not outline one.
- **A live tile.** The icon is the tool the turn is in (Segoe Fluent Icons,
  `theme::tool_icon`), or the phase when there is none. The session keeps
  the tool and when it last did something (`Session::tool`,
  `Session::activity`, neither saved), and the tile draws the last ten
  minutes as a trace of twenty bars. How full the context is, from the
  status line, is a short bar under the icon, drawn like the
  usage window's limits; from 75 % the number shows as
  well, amber then red, in the trace's place. The top line says only how
  long, except for waiting, where the verb decides what you do.
- **Motion** (`motion.rs`, `anim.rs`, pure and tested). A new tile rises
  into place, tiles slide when the list changes, hover fades in 120 ms.
  On the stage the panes without the keyboard step back behind a veil of
  the background, and panes fade in when the stage switches project.
  Windows' animation setting (`SPI_GETCLIENTAREAANIMATION`) off holds all of
  it still.
- **Nothing jumps** (`glide.rs`, pure and tested; `appear.rs`). Asked for
  on 2026-09-26 as the small niceties that make the app feel good to use.
  A cluster glides to its place in the columns: let go of after a drag,
  pushed down by one above that grew, turned by the wheel. Its first
  place, one from off screen and a screen change still jump. On the stage
  a new session's pane fades in, and the others glide to their new cells,
  as do swapped and zoomed panes; a pane takes its new size at once, since
  a terminal resized every frame redraws its agent every frame, and an
  edge dragged by hand places them at once. A glide stops when something
  else moves the window, so a drag takes hold of a gliding cluster. Menus
  fade in over 90 ms, dialogs and the picker over 160 ms while rising 8
  DIPs into place, by `WS_EX_LAYERED` alpha as the toasts do. Checked on
  screen with a dev instance: the clusters below a growing one, a
  project menu, the End sessions dialog, and panes of five `cmd.exe`
  sessions travelling to their cells (logged, since the stage was behind
  the installed one).
- **Life in the tiles** (`anim.rs`, `motion.rs`, pure and tested). Asked
  for the same day as "more fun or cool animations", and worked as the
  Animations section of the task list. All of it holds still with
  Windows' animations off, and none of it asks for a frame at rest.
  - *A finished turn lands*: its key jumps and bounces once, and a loot
    beam of the lamp's green shoots up over the tiles above and fades in
    a second ("Loot drops" in IDEAS.md).
  - *Loot is heard* (`loot.rs`, pure and tested; `sound.rs`): a soft
    thump and a rising glint with the beam, and a high two note bell, E6
    then B6, when a session's work lands on `main`, merged from its menu or
    found there by git. Made from sine waves into a WAV in memory and
    played by `PlaySound`, so they are our own and need no crate or file.
    Off until "Loot sounds" in the tray turns them on, which plays the drop
    so the choice is heard. Silent in a full screen game, a presentation,
    quiet time, and under focus assist or do not disturb, which
    `ToastNotificationManager::NotificationMode` says.
  - *Waiting quickens*: the breath goes from 1.8 s to 1.4, 1.1 and 0.9 s
    at 1, 5 and 15 minutes, counted across the steps so it never jumps.
  - *Every phase change eases* the key's depth, presence and lamp from the
    old phase's to the new over 450 ms (`Stance`), so a resumed tile rises
    out of its pause and its lamp warms up.
  - *Ending powers down*: the lamp squeezes to a white line and a dot and
    goes out like a picture tube. A tile that leaves stays a moment as a
    ghost (`Leaving`), sinking and fading while the ones below slide up
    over it. Not for the last tile in a cluster: the cluster shrinks at
    once and its bottom row covers the ghost.
  - *Subagents are sparks* circling the working lamp, one for each seen
    in the turn within the last 30 s. Hooks fired inside a subagent carry
    its `agent_id`, so no hook had to be added to anyone's settings; no
    hook says a subagent ended, hence the 30 s (`Session::subagents`).
  - *The icon turns over* like a card when the tool changes, 280 ms.
  - *The activity trace scrolls*: slices stand still on the clock and the
    bars drift left as the newest fills, the oldest fading
    (`Session::trace`).
  - *The context bar fills like a liquid*, rising to each reading with
    the last segment lit only as far as the level reaches, and a glint
    runs along it on a working session past 75 %.
  - *A finished task* keeps its row a moment: a green check, a strike
    through the title, then the row folds and the rows below close up
    (`Finishing`).
  - *A taken task throws a light* from its row to the lamp of the new
    session that took it, once, when a session less than 3 s old turns
    out to hold an item (`Handoffs`).
  - *A busy project glows*: its accent washes deeper from the cluster's
    top and breathes over 4 s while any of its sessions works.
  - *A new project's cluster* rises 14 DIPs into its column and fades in
    over 300 ms, through `appear`. The windows there at start stand at
    once.
  - *A dragged cluster gets room*: the others in the column under the
    cursor glide apart for it, laid out as the drop would lay them but
    saved only on the drop. Only within a column already there with
    others in it, since a new or emptied column moves the columns under
    the cursor.

  Checked on screen with a dev instance and fake sessions posted to its
  port, captured by `PrintWindow` since the desktop was in use: the beam,
  the power down and the ghost, three sparks, the icon turning, the
  context bar rising to 88 %, a struck task folding, the light from a
  taken row. A new cluster's arrival was read back from its window's
  alpha and position. A real drag of one cluster up over another in its
  column: the other glided down while the button was held, and the drop
  landed in the gap.
- **How it ended, in item colours** ("Item rarity colours" in
  IDEAS.md; `rarity.rs`, `theme::rarity_color`, pure and tested). A
  session's name is printed in Diablo's colours, the third job colour has
  and only on the name, never lit: white changed nothing, blue changed
  files, yellow changed files and the tests passed after the last change,
  green one of a batch the runner held at once that changed something,
  gold what it committed is in what the main tree has checked out. Gold
  beats green, since landing is what a batch is for. All from the tool
  hooks (`Session::loot`, saved): `Edit`, `Write` and friends change
  files, and a shell command that runs a test suite or a `git commit` or
  `git merge` is read from `tool_input.command`, its outcome from
  `PostToolUse` or `PostToolUseFailure`. A worktree's diff counts as a
  change too. Whether it landed is asked of git (`merge-base
  --is-ancestor`) with the diff recount once it committed, and a merge
  from the project menu gilds its sessions at once, since the sweep may
  take the worktree before anyone asks. Each colour leans off its nearest
  lamp and is paler, so a yellow name does not read as waiting. Checked on
  screen with a dev instance and fake sessions in a scratch repository:
  each colour, a worktree session turning gold when its branch was merged,
  and green and gold coming back from the saved state on paused tiles.
- **Type.** Project names in Segoe UI Variable Display, session names
  semibold, ages with tabular digits so they do not shuffle each second,
  FILES and RECENT letter spaced like legends printed on the plate.

What it costs, measured on a release build with two clusters of fake
sessions: 1.1 % of one core while two tiles work and two wait, 0.8 % with
only waiting ones breathing, 0.2 % with nothing moving. The first cut cost
11 %. Two changes got it down. Everything that holds still is drawn once
into a compatible render target and copied each frame, with only the light
drawn fresh (`Painter::still` and `Painter::light`); the layer is redrawn
when anything else changes. And gradient brushes are kept by their stops
and moved or faded by their properties, since each new one is a texture on
the GPU. A cluster asks for frames by timer only while something moves: 40
ms while a light goes round, 66 ms while only a breath does, none at rest.

Tested on screen with a dev instance on its own port and state folder: the
acrylic behind clusters with Chrome underneath, the orbit, the breath and
the ring leaving a tile that starts waiting, the done flash, three `cmd.exe`
panes on the stage with the unfocused two dimmed, the stage's border
matching its cluster's mark, context rings at 38 % and 82 %. After the first
look proved too light and its borders too bright, the darker pass was
checked the same way beside the installed build.

Panes are rounded too, not only the glass inside them. A pane is still a
square child window, but a groove cut round it with corners of the glass's
radius plus the bezel shows its edge, and outside the groove its plate runs
on into the stage's, so the corners read as round. No window region: a
region's edge is aliased, and the plate at the corners is the same either
way. A lifted header is rounded with it. Checked on
screen with `cmd.exe` panes on a dark glass and on a light one (a program
setting the background by OSC 11): the corners run parallel to the
glass's, and nothing clips the text. The drop and lift highlights were not
seen, since scripted mouse input did not reach the stage. The tray menu and
the stage's title bar were left here and are drawn now, see "No Windows
chrome" below.

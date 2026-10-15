# Plain terminals

A project needs terminals that are not an agent: a dev server, a log, a
deploy, a quick `git log`. Asked for so they are at hand beside the
sessions instead of in another terminal app.

- **A shell is a session** with `shell` set (`Session::shell`, saved). So
  it gets a tile, a pane in the project's grid, a place in the saved order,
  the tile menu, and a cluster when it is the project's only session, all
  from the code sessions already had. The alternative, panes with no tile
  like the file viewer, left a dev server with nothing to click once the
  stage showed another project.
- **Ways in.** A small button with a terminal glyph beside the bottom `+`
  (`Hit::Shell`), Ctrl+Shift+T in any pane (`CharAction::NewShell`, as a new
  tab in Windows Terminal; plain Ctrl+T still reaches the program), and
  "New terminal" in the project menu. It opens in the project folder, joins
  the stage without moving it, and gets the keyboard. Named Terminal,
  Terminal 2 and so on.
- **Which shell** (`shell::program`): `HORADRIC_SHELL` by path or name,
  otherwise `pwsh`, otherwise Windows PowerShell, otherwise `COMSPEC`.
- **Not a session to the hooks.** The shell gets no `HORADRIC_SESSION` or
  `HORADRIC_OWNER_PORT`, and loses them when Horadric itself inherited them, so
  a `claude` typed into it is nobody's and stays off the tiles, as it
  would outside Horadric. Found on screen: a dev instance started from a
  session handed its tag down.
- **The tile.** No hook reports on a shell, so its phase stays idle and the
  tile reads the terminal instead: the terminal glyph, the title the program
  set as its second line (`shell::title` drops a title that only names the
  executable, and takes the command from `cmd.exe - npm run dev`), and its
  output as the activity trace, at most one mark a second
  (`Session::touch`). The pane header and the stage's title use the same
  cleaned title.
- **Lifetime.** Any exit closes it, tile and pane, whatever the code: `exit`
  means done, and there is nothing to resume. Closing the stage leaves it
  running. "Start over" leaves a project's terminals alone. Quitting Horadric
  says terminals close, apart from the sessions that resume. After a restart
  a terminal comes back as a paused tile in its place, and a click opens a
  fresh shell there; a reload reopens the ones that were open. What ran in
  it is gone either way: the process question below covers shells too.

Tested on screen with a dev instance: the button opened Windows PowerShell
(no `pwsh` on this machine) as a second pane beside a `cmd.exe` session
with the keyboard, setting the window title from inside it showed on the
tile, the pane header and the stage, `exit` removed tile and pane, a
restart brought it back paused and a click reopened it, one process each
time and no `claude`. Not tested on screen: Ctrl+Shift+T, since a scripted
key would go to whatever window has the focus; the mapping is unit tested.

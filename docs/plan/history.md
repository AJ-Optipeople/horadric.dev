# History

A session ended for good (`/exit`, End session, Start over) used to be
gone from Horadric, with no way back to its conversation. Asked for so a
closed session can be picked up where it was left.

- **Claude Code already keeps them.** Every conversation is a transcript in
  `~/.claude/projects/<folder>/<id>.jsonl` (under `CLAUDE_CONFIG_DIR` when
  set), the folder being the working directory with every character that
  is not a letter or digit made a hyphen. The drive letter comes in both
  cases, so both folders are read. Horadric keeps no history of its own:
  this one also has the conversations started outside Horadric.
- **History in the project menu** (right click a header or the bottom `+`)
  lists the newest ten, by the title Claude Code gave them and how long ago
  each was touched (`history::label`). Conversations a tile holds, live or
  paused, are left out, and so are ones with no title, since Claude Code
  writes one after the first prompt. Only the last 256 KB of each file is
  read and at most 40 files are looked at, since the menu is opening on
  the UI thread (`transcript::history`).
- **A click** starts `claude --resume <id>` as a new tile in the project,
  with the title and id set at once, so a pause before its next prompt
  still resumes that conversation. "All conversations..." at the bottom
  starts `claude --resume` with no id, and Claude Code's own picker lists
  the rest in the pane.
- **A project closed** has no cluster and so no project menu. Two more
  ways in: History in the tray menu, with a submenu for each
  recent project, and a right click on a recent project in the start
  window, which offers New session and that project's History. Menu ids
  are spans of `history::SPAN` per list (`history::pick`,
  `history::pick_nested`), so one tray menu holds every project's.
- `tray::Item::Submenu` is new for it.

Tested on screen with a dev instance and `cmd.exe`: the project menu's
submenu listed this project's ten newest conversations with titles and
ages, and a click opened a pane titled with the conversation, its process
given `--resume` and the id. With no project open, a right click on the
start window's recent project and a pick from it replaced the start window
with the project's cluster and the resumed tile. The tray's History showed
the project's submenu without the conversation just resumed, and a pick
there started the right one. A dev instance lists the installed one's live
sessions too, since its registry does not hold them.

**Folded into the quest log window (2026-10-03).** The History submenus
are gone from the project menu, the tray and the start window. The
conversations they listed are rows on the quest log's main line (see The
quest log window), and the three menus offer "Quest log..." for the
project instead: the tray a "Quest log" submenu of the recent projects,
the start window's right click New session and Quest log.

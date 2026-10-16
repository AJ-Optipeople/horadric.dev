# The Settings window

Settings grew one menu line at a time and now hide among the actions.
The tray menu has about 25 lines, half of them settings: Theme, Terminal
font, Tiles on screen, Notify when a session needs you, Loot sounds, Show
on Discord, Start with Windows, Check for updates, and Warriv's two
switches per project. A project's settings sit in its header menu among
its actions (Work mode, Colour, Add host). The Runetome's menu holds Ask
before a click casts. The session defaults live in the usage window. The
three hotkeys can not be set at all. Nothing shows the whole of it, and a
menu is a poor place to read a choice before making it.

**One window, applied at once** (decided 2026-10-07). Drawn by Horadric
like the usage window, with its own controls (`dropdown.rs`, `field.rs`,
the effort slider), so no toolkit. Every change takes effect the moment
it is made, as the menu lines do today, so there is no Save button and no
state where the window and the app disagree. Sections down the left:

- **Appearance:** theme, terminal font, which screen the tiles stand on.
- **Notifications:** notify when a session waits, loot sounds.
- **Sessions:** default model, effort and permissions, and which agents
  are offered. The usage window keeps its rows as a shortcut to the same
  values. Built: an Agent row picks whose defaults the rows under it
  show, effort is a list here rather than the slider, and "In the
  project menu" turns off another installed agent's New session line
  (Claude Code is always offered).
- **Keys:** the three hotkeys, each set by pressing the new chord.
- **Startup and updates:** start with Windows, check for updates now.
- **Privacy:** what Discord may show.
- **Runetome:** ask before a click casts, built in stones put away.
- **Projects:** every project in one list, not a window each. Picking one
  shows its colour, work mode, SSH hosts, and Warriv drives with and
  ships public.

**The menus become actions.** The tray keeps New session, the recent
projects, the quest logs, Next waiting session, Stay a while and listen,
the tile and terminal actions, End all sessions and Quit, and gains
"Settings...". Warriv drives stays in the tray too: it is the go ahead to
ship, and a go ahead should be one click and in plain sight. The project
menu loses Work mode, Colour and Add host and gains "Project
settings...", which opens the same window at that project. Tile menus are
actions already and stay as they are.

**A window of its own** (decided 2026-10-07): settings are visited, not
watched, so it closes rather than standing in the columns. It opens in the
middle of the screen the cursor is on, has a taskbar button, moves by its
title bar, and closes by its cross, Esc or Alt+F4. The arrow keys go
through the sections.

**Built so far** (2026-10-07): the frame (`settings.rs`, laid out and hit
tested in `layout::settings`), Appearance, Notifications, Startup and
updates, and Privacy. Their tray lines are gone; the tray has
"Settings..." and keeps "Update to" when a check found a release. A list
row drops the usage window's list, which now serves either window
(`dropdown::Whose`), and a list too long for the screen climbs to its top.
Then Runetome: ask before a click casts is a switch, and the stones put
away are counted with a button that brings them all back. The stone menu
keeps only its actions (Cast, Arm, Put away, Change, Remove, Make a new
stone).
Then Keys: a click on a shortcut listens, the chord pressed takes effect at
once and is kept in state.json (`hotkeys`), Esc keeps the old one and
Backspace puts back the default. It listens through a low level keyboard
hook, not the window's key messages, because a chord another program
registered never reaches the window, and that is the one to say is taken.
A taken chord keeps the old one; a chord taken at start shows as taken.
Naming and parsing chords is `hotkey.rs`, pure and tested.

**Projects built** (2026-10-07): the section's first row drops every
project (recent, on screen, or with a quest log) and the rows below it
are the picked one's: colour, work mode, its SSH hosts and "Add...", Warriv
drives and And ships public. A row with no choice says why: "Not a git
repository", "Needs a quest log", "Only while Warriv drives". Ships public
asks first, as the tray does. The project menu lost Work mode, Colour
and Add host and has "Project settings...", which opens the window there.

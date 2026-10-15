# Experience

"Experience" in [IDEAS.md](../IDEAS.md). One XP for every commit of yours
that landed, and the level they add up to, on the first line of the tray
menu: "Level 7    105 / 140 XP".

- **Counted from git, never kept.** In every recent project, the commits
  on what the main tree has checked out, merges left out, whose author
  email is the one `git config user.email` gives there. A hash counts
  once, so two projects in one repository count its commits once. A
  reinstall or a new machine loses nothing.
- **On a thread.** The count runs at start and each time the tray menu
  opens, which shows the one before, so opening it never waits on git.
- **Levels come slower as they go**, as in Diablo: level L at
  `5 * L * (L - 1) / 2` XP, so level 2 at 5 commits, 10 at 225 and 99, the
  last, at 24255. Pure and tested (`horadric_core::experience`).

Not seen on screen: the one try found the screen in use by a full screen
game and was stopped before the menu was read.

# The file viewer

A click on a file in the files tile shows it on the stage, read only, beside
the project's sessions: line numbers, VS Code's Dark+ colours, long lines
cut at the edge and scrolled sideways. Asked for so a file an
agent is working on can be read without leaving Horadric. A viewer, not an
editor, on purpose.

- **A pane with no program.** A file view is a `Console` without a pseudo
  console, its grid fed escape sequences that paint the file
  (`viewer.rs`, pure and tested). The pane draws it like a session, so
  selection, the wheel, glyph fallback and wide characters cost nothing.
  Control characters in the file become their Unicode pictures, so a file
  can never send the terminal a command.
- **One per project.** A click on another file replaces it, like VS Code's
  preview tab, so browsing never piles up panes. It comes last in the
  grid and is never saved in the order. Closing the stage drops it.
- **Colours** (`highlight.rs`). `syntect` with the Sublime grammars it
  bundles, which are TextMate grammars like VS Code's, and Dark+ written
  out as scope rules, so no theme file ships. Pure Rust regexes
  (`regex-fancy`), no C build. Highlighting runs on a thread; the file shows
  plain first. The bundle has no TypeScript or TOML: `.ts` and `.tsx` borrow
  JavaScript, TOML stays plain.
- **Sideways.** Long lines are not wrapped: wrapped code loses the shape
  its indentation gives it, which is what VS Code keeps too. Shift+wheel
  and a horizontal wheel or touchpad scroll it sideways, six columns a
  notch, never past where the longest line ends. The line numbers and the
  changed line marks stay put. Each step lays the file out again from the
  new column (`viewer::render`, one row per line), as a resize does.
- **Keys.** Up and Down, Page Up and Down, Ctrl+Home and Ctrl+End scroll.
  Left and Right scroll sideways four columns, Home and End to either
  side. Ctrl+C copies, and a drag copies on release, as in a session.
  Copying leaves out the line numbers (`viewer::copy` maps grid cells to
  file bytes), and a selection from the gutter or to the right edge takes
  what is off screen on that side of its line too. Esc or the cross at the
  end of its header closes it. Nothing typed reaches anything.
- **Live.** When the project's watcher rescans, the view reads its file
  again if its size or time changed, keeping the same line at the top. So
  an agent's edit shows within a second or so. A resize lays the file out
  again for the new width, also keeping the top line.
- **Limits.** Files over 4 MB are not read, binary files (a NUL in the
  first 8000 bytes) are not shown, and past 10,000 lines the rest is left
  out and counted. Each line is a grid row a full width of cells, so that
  limit is what bounds memory, about 30 MB at 120 columns.
- **Changed lines.** Between a line's number and its text, a bar in VS
  Code's gutter colours says how it differs from the last commit: green
  for added, blue for changed, and a red underline on the line a removal
  follows. It comes from the hunk headers of `git diff -U0 HEAD` for the
  one file (`viewer::marks`, pure), run on the colouring thread. Every
  rescan asks again even when the file did not change, so a commit clears
  the marks. An untracked file, or one outside a repository, has none.
- **Search.** Ctrl+Shift+F opens the terminal's search bar, but a view
  searches its file, not its grid (`viewer::find` and `viewer::cells`), so
  a line number never matches. A match off to a side scrolls the view to
  put it in the middle, and scrolling moves its mark with the text. It
  starts at the top line on screen, and Enter and F3 go down the file, as
  in an editor, where a terminal goes up its history. Shift goes back, and
  both wrap round. Case is ignored unless the query has a capital.

Tested on screen with a dev instance on this repo: `main.rs` opened beside
a `cmd.exe` pane with the colours right, the wheel scrolled it, a click on
another file replaced it, an edit to the shown file appeared within a few
seconds, a drag across the gutter copied only the text, and Esc and the
cross both closed it with the keyboard back in the session. The changed
lines and the search were checked on a scratch repository with a line
changed, two removed and two added: each mark in its place, Enter stepping
to the next match and round to the first, "12" (only a line number there)
not found, and the marks gone a few seconds after a commit. Sideways
scrolling was checked on a scratch file of 150 column lines: Shift+wheel
moved the text 30 columns in five notches with the numbers and the two
marks in place, a horizontal wheel brought it back 12, a search for a word
at column 200 scrolled over to it, and a Shift+wheel notch after that moved
its mark with it.

# Performance

On 2026-09-26 everything felt less smooth: typing in the stage, right
clicks, the app in general. Every thread but the UI thread sat idle, and
the UI thread did work nobody asked for. Measured on a dev instance with
fifteen clusters, three animating tiles and eight hook events a second,
its share of a core fell from 75% to 6%. With the tiles
animating and no events, it fell from 15% to 1.3%. What was
fixed, and what to keep that way:

- **No paint waits for the screen.** Render targets present with
  `D2D1_PRESENT_OPTIONS_IMMEDIATELY`. With the default, every `EndDraw`
  waited for the next refresh, and every window paints on the one UI
  thread, so a few animating clusters filled each frame and a keystroke's
  echo waited behind them. DWM composes the windows, so nothing tears.
- **A keystroke paints once.** Sending typed bytes repaints the pane only
  when that cleared a selection or scrolled back to the live screen. The
  echo repaints; a paint before it showed the old screen and pushed the
  echo a frame back.
- **A cluster redraws only when what it shows changed.** A full redraw of
  the kept layer is the dearest thing the app does. `Cluster::update`
  lays out again and redraws only if the layout, the sessions, the quest
  rows, the board or the stage marks differ from what the kept layer was
  drawn from (`Still`). Every hook event, every arrange and every change
  count goes through it, so an event for one project redraws that one.
  `Cluster::fit` still always redraws, for a click that changed something
  only the cluster knows. The second tick redraws a cluster only when an
  age label or an activity trace moved (`Clock`), which for an old tile is
  once a minute.
- **Hook events are taken in bursts.** The feeder posts
  `WM_HORADRIC_EVENT` only if the UI has not been told yet, and the flags
  wait in `EVENTS`. Ten events queued behind a menu cost one pass.
- **Folder keys of missing folders are remembered for five seconds.**
  `folder_key` is called per session, per cluster, per paint. For a
  folder that is gone (a swept worktree, a deleted project) it started
  `git` every time, which made each event cost 90 ms with four such
  folders in the saved state.
- **Menus do not wait for git.** The tile menu lists the change count the
  tile keeps, counted again within seconds of the agent's last move, and
  counts only when there is none. The project menu reads history and asks
  git side by side, transcript titles are kept while their file is
  unchanged, and `worktree::main_tree` remembers its answer.
- **Light nobody can see does not move.** A cluster's animation frame is
  skipped while it is minimised, off screen or wholly under another
  window. Browsers and Electron apps draw without a redirection bitmap,
  so that style alone does not make a window see-through; layered,
  click-through and overlay tool windows never count as covering.

A second pass on 2026-10-01 took the stage panes, measured the same way
on a release build with the stage on top. One pane full of characters
the font lacks (CJK, `⏺`, `★`) went from 37% of a core to 1.5% when only
its title's spinner turned, and from 38% to 3.5% with a line of output
every 50 ms. Two panes of Claude Code like output at ten frames a second
went from 8% to 5%.

- **Loose characters are laid out once.** A character the terminal font
  lacks is drawn from an `IDWriteTextLayout` kept in `Font` by text and
  style, not by a `DrawText` that looked for its fallback font on every
  paint. A new size or family starts the cache over, and so do more than
  2048 of them.
- **The glass's shade needs no layer.** The shade under the glass's top
  edge, and under a latched tab key's, is the rounded shape filled with
  the clamped gradient and clipped to the band, instead of a layer and a
  geometry made on every paint.
- **A spinner in a title is left out.** `shell::title` drops a braille
  glyph in front of a title, which is how Claude Code shows it works. The
  pane's header, the stage's caption and the window title no longer
  change on every turn of it.
- **A pane that would draw what it shows presents it again.** The pane
  keeps what it last drew (`Shown`: the frame, the header, the search,
  the veil, the size, the font) and only presents the kept frame when a
  paint would draw the same. Output that changes nothing on screen costs
  a comparison. Browser panes always draw.
- **A cluster paint copies its sessions once.** It reads them from the
  registry once, as `Rc`, and the tiles leaving and the `Still` it keeps
  share them.

A third look on 2026-10-13, with nine sessions in three projects, found
Horadric's own processes at about 260 MB. Each `claude` took 420 MB, a
plugin's MCP server started once per session took 240 MB more, and the
browser panes took 1.5 GB in nine renderers, most of them pages nobody
could see.

- **A page off the stage sleeps.** A tab behind the shown one, or in a
  project not on the stage, gets the low memory target and `TrySuspend`
  (`web::sleep`): its timers stop and Chromium gives back what it can.
  Showing it, an agent's call (`wake`) or a navigation wakes it first
  (`rouse`). Measured on a dev instance, a hidden tab ran 2 ticks of a
  100 ms timer in 15 seconds where the shown one ran 149.
- **Heavy commands take turns.** Several sessions running test suites at
  once share the CPU many ways and all finish late. `horadric heavy --
  <command>` runs one only while no other session on the machine runs
  one, and tells a waiter whose command it waits for. The slot is a file
  lock in the temp folder (`horadric-heavy\slot.lock`), which the system
  frees when its holder ends however it ends, so a killed session holds
  nobody up. The dev and installed Horadric share it. Every session is
  told to wrap its test suites and full builds, and never a dev server
  or a watcher. `horadric heavy` alone says who holds it. Since
  2026-10-17 it also keeps the command's output out of the agent's
  window: the whole of it goes to a file and a digest is printed, see
  [lore.md](lore.md) part 6. Not yet: the tile does not show a session
  waiting, and the slot is always one.

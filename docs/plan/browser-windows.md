# Browser windows

An agent testing a web page starts a browser of its own, through Playwright
or the Chrome DevTools MCP. Its window used to land anywhere, with nothing
saying which session opened it. Embedding a browser in Horadric was
considered and dropped at first: the agent drives its browser over a debug
protocol and never needs a window Horadric owns. So Horadric manages the
browser's window instead. The browser pane below came later, for the
user's own browsing and for pages user and agent look at together.

- **Which session.** Every agent starts inside a job object of its own
  (`horadric-pty`, `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so it is in the job from
  its first instruction), and everything it starts joins the job. A window
  belongs to the session whose job holds its process (`IsProcessInJob`).
  The job limits nothing and closing it ends nothing.
- **Which windows** (`browsers.rs`). A WinEvent hook, out of context, hears
  every top level window shown and every tracked one destroyed, and posts
  them to the app window. Only main windows count (no owner, a title bar,
  not a tool window) and only browsers, by executable name: Chrome, Edge,
  Firefox, Brave, Chromium, Vivaldi, Opera and Playwright's WebKit. An editor
  or an app under test is left alone, and so is a dev Horadric started inside
  a session, whose windows are in that session's job too.
- **Where it goes.** When it first appears, against the stage in the space
  beside the tiles and the stage together (`layout::beside_stage`), its own
  size where that fits and shrunk where it does not. Narrower than 360 DIPs,
  or opened minimised or maximised, it stays where it opened. After that it
  is the user's.
- **It follows its project.** When the stage switches project, that
  project's browsers come back just below the stage in the stacking order,
  and every other project's are minimised. One the user minimised stays
  minimised.
- **The tile mark.** A tile whose session has a browser open shows a small
  window glyph at the end of its second line. It is a button: a click
  restores that session's browsers and gives them the foreground.
- **Never block.** Every call on a browser window is asynchronous
  (`SWP_ASYNCWINDOWPOS`, `ShowWindowAsync`). The window belongs to another
  process, and a hung browser would otherwise freeze the UI thread.
- A URL opened in the user's own browser (`start https://...`) goes to the
  browser that is already running, outside every job, so it is not caught.
- Shrinking a browser changes the page's layout when it has no fixed
  viewport. With a fixed one, the Playwright library's default, the window
  size changes only what you see, not what the agent tests. Not yet checked
  against the MCP servers' own defaults.

Tested on screen with a dev instance on its own port and `cmd.exe` sessions
in two projects, each running `start msedge` with a profile of its own: each
Edge landed beside the stage, starting the second project minimised the
first one's, a tile click switched them back, the tile's button brought its
Edge to the front, and closing an Edge took the mark off its tile. The user's
own Chrome was never touched. The hook costs no CPU to speak of: 16 ms in 5
idle seconds.

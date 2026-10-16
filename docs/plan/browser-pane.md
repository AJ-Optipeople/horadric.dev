# Browser pane

Asked for because a browser beside the stage never fitted with the
sessions and tiles, and because the agent should see the page the user
means and drive it, instead of being pasted a screenshot. A first go
managed an Edge window with a profile per project, which meant logging in
again for every project; it was dropped the same day. This reverses the
settled "no web view" for web pages only: Horadric's own UI stays Direct2D.

- **A pane on the stage** (`web.rs`, `Console::web`, `App::webs`), one per
  project, after its sessions and the file view in the grid. Ctrl+Shift+B
  in a pane (`CharAction::Browse`, plain Ctrl+B stays the program's) or
  "Browser" in the project menu opens it and, when new, asks for an
  address (`web::address`, pure: a scheme kept, a local server over http,
  a host over https, anything else a search). The header is an address
  bar, as in any browser (`glyphs::bar_layout`, pure): back and forward,
  dim with nowhere to go (WebView2's `HistoryChanged`), reload, then the
  address in a field up to the cross. A click or Ctrl+L puts the keyboard
  in the field with the address selected (`Pane::address`, a
  `field::Field`); Enter goes, Esc or a click anywhere else leaves it. A
  page that takes the keyboard as it first loads, with no click to send
  it there, gives it back (`Pane::page_took`). With no pane on the stage,
  Go to still asks in a prompt. A right click on the header has Go to,
  Back, Forward, Reload, Open in your browser and Close. Ctrl+Shift+T in
  the page still opens a terminal: the page has the
  keyboard, so those two are caught by `AcceleratorKeyPressed` first.
- **WebView2**, through `webview2-com`. It is Edge's engine, part of
  Windows 11. The loader is linked statically, so there is no DLL to ship.
  A controller is up about 250 ms after it is asked for, so it is made
  asynchronously: the pane shows at once and the page arrives in it.
  Nothing waits on WebView2 in a handler, and no borrow is held across a
  call into it, since a call can raise an event that comes back.
- **The page outlives its pane.** The stage destroys its panes whenever it
  shows another project, and a destroyed WebView loses its page. So the
  controller is `web`'s: a pane lends it a window while it shows it
  (`attach`, the pane has `WS_CLIPCHILDREN` so its own drawing leaves the
  page alone), and on the pane's `WM_DESTROY` it goes, hidden, onto the
  app's window (`detach`). Closing the stage keeps it too. Only its cross
  or Close ends it.
- **Kept over a reload** (`SavedState::pages`, `web::pages`,
  `web::restore`). Every ship dropped the open pages, so each project's
  tabs are saved with their address, title and the one shown, and a
  start that resumes sessions (a reload, or after a crash) opens them
  again in their project's grid, off the stage like an agent's open. A
  blank tab is not kept (`SavedPages::of`, pure). After Quit they are
  not reopened, as sessions are not resumed. Only the address: WebView2
  has no way to give a page its back and forward history again. Tested
  with a dev instance on its own port and `APPDATA`: two saved tabs came
  back after `app --reload`, and again after `horadric reload`, the new
  process writing them, titles and the shown tab as they were.
- **One profile for everything**, `%LOCALAPPDATA%\Horadric\web`, for every
  project, session and Horadric, dev instances included, so a login made
  once stays. WebView2 lets two processes share a profile when they start
  it with the same arguments, which is why the DevTools port below is
  chosen once and kept in the profile folder rather than per instance.
- **Agents drive it** (`horadric mcp`, `drive.rs`). Every Claude Code
  session gets `--mcp-config=` a file naming `horadric mcp` (Codex gets
  `-c mcp_servers.horadric...`; Grok gets a `[mcp_servers.horadric]`
  table in `~/.grok/config.toml`, see below). The server finds its
  session by the `HORADRIC_SESSION` it inherits and posts each call to
  `/horadric/browser`, the one listener path that waits for the app's answer. The session names the project, so
  an agent only reaches its own project's page. The app does open, close,
  navigate, back, forward, reload, info, and any DevTools call on that
  page through WebView2's `CallDevToolsProtocolMethod`, no port needed.
  Every tool is made of those: `browser_snapshot` is an outline of the
  page with a ref per link, button and field, a click is a script that
  finds the element and scrolls it to the middle, then a real mouse press
  and release there; typing is `Input.insertText`; screenshots
  `Page.captureScreenshot`; the console is kept from the first page on by
  a script added at document creation. An agent's open never takes the
  stage or the keyboard: the page joins its project's grid. A hidden page
  draws nothing, and a screenshot waits for it to draw (90 s, then gave
  up), so a page off the stage is made visible on the app's hidden window
  while a call is in flight, at 1280 × 800 if it was never shown.
  Tested: a script through every tool against a test page and
  example.com, and a Haiku session told to fill in a form, which opened,
  typed, ticked, clicked, looked and closed by itself.
- **A call that fails did not act, or says it may have** (2026-10-12).
  On x.com most calls failed with os error 10054 or 10060 though they
  had run, and the agent's retries posted text twice. The listener read
  through a `try_clone` of the socket, which on Windows is an
  inheritable handle, so every git or session the app started during a
  call kept the connection open past the reply: the caller waited until
  that child exited, then was reset. It reads through a borrow now. A
  call the listener gave up on (90 s) is marked so the app does not
  start it late, and an error after the call reached Horadric says it
  may have run, so the agent looks before trying again.
- **Chosen over Chrome** (2026-10-07). Users saw agents start Chrome,
  Playwright or a headless browser while the pane sat unused. Claude
  Code defers MCP tools behind a search, so an agent that thinks of
  Playwright first never finds them. Every Claude Code session given the
  server is now told in its system prompt (`web::AGENT_PROMPT`) to use
  the pane in place of those, unless the user asks for one, and the
  server's instructions and `browser_open` say the same, naming them so
  a search for "playwright" or "chrome" finds the pane.
- **Grok's way in** (2026-10-01). Grok Build 1.0.44 has no per session
  way to be given a server: the TUI has no `--mcp-config` or
  `--plugin-dir` (only `grok agent` has the latter), its `GROK_CONFIG`
  overlay keeps only soft settings and drops `mcp_servers` and
  `plugins`, a plugin in `~/.grok/plugins` is put on the disabled list
  when first seen, and `--agent` swaps the whole system prompt. So
  `install` and `reload` write a `[mcp_servers.horadric]` table into
  `~/.grok/config.toml`, beside the hook file, and `uninstall` takes it
  out (`with_grok_mcp`, pure): the only part of that file Horadric
  touches, the dev instance never. Grok passes a stdio server its own
  environment, so the session's tag arrives. Every `grok` starts the
  server, so without a `HORADRIC_SESSION` it offers no tools and no
  instructions. Live check: the table pointing at a debug build, a dev
  instance on its own port, `horadric new --agent grok` told to open
  example.com; Grok found `horadric__browser_open` by `search_tool`, the
  pane for its project opened there, and `browser_snapshot` read it.
  `grok mcp doctor` from a shell outside Horadric shows 0 tools.
- The browser still listens for the DevTools protocol on 127.0.0.1, on
  the port in `web\devtools-port`, for tools outside Horadric. While it
  is open, any program on the machine can drive that logged in browser.
  Loopback only is the whole of the protection so far. Next is letting
  the user point at part of the page and send it to a session with a
  screenshot, the address and the element.
- **Tabs** (`web::Web::tabs`, `glyphs::tab_layout`, pure). Asked for so a
  second page does not mean losing the first. One pane per project still,
  with a strip of tabs along the top of its glass and the page below it;
  each tab is a WebView of its own, only the chosen one visible, so a tab
  keeps its page, history and scroll while another is shown. The `+` at
  the strip's end, Ctrl+T or New tab in the menu opens one and puts the
  keyboard in the address field. A tab's cross, a middle click on it,
  Ctrl+W or Close tab closes it, and closing the last closes the pane.
  Ctrl+Tab and Ctrl+Shift+Tab, Ctrl+PgDn and Ctrl+PgUp, and Ctrl+1 to 9
  pick one, as in a browser (`web::tab_key`, pure), caught in the page by
  `AcceleratorKeyPressed` and in the pane while the address is typed. A
  tab is named by its page's title, else its address (`web::tab_name`).
  An address Ctrl+clicked in a terminal opens in a new tab. The tabs are
  kept over a reload, see above.
- **A tab per agent** (`web::pick_tab`, pure, `Tab::driver`). Asked for
  because an agent drove whichever tab was shown, so the user showing
  another tab, or a second session opening a page, moved it off its
  work. Now each session keeps to a tab of its own. Its first call takes
  the shown tab when no other live agent has it (so "look at this page"
  works); otherwise opening a page gives it a new tab behind the shown
  one, and a read looks at the shown tab without taking it. A popup its
  click opened within 3 s becomes its tab and hands it back on closing.
  `browser_close` closes only its own tab. A tab an agent works in shows
  its session's initial on a disc in the colour of its phase, dim while
  idle, gone when it ends; the owner is kept over a reload
  (`SavedTab::driver`). A background tab an agent calls on draws on the
  app's hidden window meanwhile, as a page off the stage does. Tested
  over the listener with two fake sessions in one project: each navigated
  and read its own tab, a screenshot of the background tab came back,
  closing one left the other, and a `window.open` popup took its agent
  and gave it back. The badge was not seen on screen yet: the desktop
  was locked.
- Popups and links to a new window (a middle click, `target=_blank`, an
  OAuth login) open in a new tab: `NewWindowRequested` is deferred until
  the tab's WebView is made and then handed it, so the popup keeps its
  opener, and a page closing itself (`WindowCloseRequested`) closes its
  tab.
- **A size of its own** (`viewport.rs`, pure). Fitted, the page is the
  glass and changes size with the grid. Sized, from the size button after
  reload or Page size in the header's menu (Phone, Tablet, Laptop,
  Desktop, Resize by hand, or typed), it lays out at that many CSS pixels
  whatever the grid does, centred in the glass with its size under it.
  Where the pane is too small it is scaled down with WebView2's zoom
  factor, which divides the page's bounds to get the CSS viewport, so it
  still lays out at its own size; the zoom is worked out from the rounded
  pixel bounds so the width comes out exact. Grips just outside its right
  and bottom edges and at the corner resize it. Ctrl and the wheel stop
  zooming a sized page, since that would change the size it lays out at.
  Kept per project in the saved state (`page_sizes`). Only the size is
  kept: the pane still moves between cells as sessions come and go.
  Checked over DevTools: Phone gave `innerWidth` 390, a drag 680 × 497,
  Desktop scaled to 54% still 1920 × 1080, and Fit the whole glass.
- **Its place beside the grid** (`layout::docked_grid`, pure). Three
  buttons after the size button, drawn since the icon font has no dock
  on top, stand the browser pane on the left, on top or on the right of
  the stage, the whole of that side, with the sessions in a grid of their
  own beside it. So it keeps its place and size as sessions come and go.
  The lit one says where it is, and a click on it puts it back in the
  grid, as before. The size menu has the same four. The gap beside it is
  a seam: drag it to change its width, or height on top, kept per project
  with its side (`page_docks`, which still reads the width the first
  build wrote, as the right). It starts half and half with the grid, and
  stays half as the stage resizes until the seam is dragged (640 wide
  was too narrow to work in). It keeps its width going from left to
  right, and each side keeps at least 320. Tested with five `cmd.exe`
  sessions: left, top and right each put the browser along that whole
  side with the grid beside it, the top seam dragged 150 pixels made it
  that much taller, and the lit button put it back in the grid.

- **History and suggestions** (`horadric_core::history`, pure, kept in
  `browser-history.json` beside the state; `suggest.rs`). Asked for so
  the address field works as a browser's does. Every page a tab the user
  drives goes to is kept with its title, its visits and the last one, at
  most 5000; a tab an agent works in is not the user's history. Typing in
  the field finishes a site gone to before inline, the rest selected
  (`History::complete`), and drops a list of the pages that fit under it,
  every word typed in the address or title, a begun address first, then
  by visits that count for less the longer ago they were
  (`History::suggest`). An empty field, as on a new tab, lists the latest
  sites, one page each. The list is a window of its own that never takes
  the focus, so typing goes on; Up and Down move through it with the
  address in the field, Enter or a click goes, Esc closes it first, and
  Shift+Delete forgets the page. Deleting never finishes inline again.
  Tested with a dev instance on its own port and `APPDATA`, by messages
  posted to the pane: "git" became github.com with two pages listed,
  Down twice and Enter went to the second, `+` listed the latest six,
  and a click on one went there and counted a visit.
- **Zoom per site.** A zoom the user gives a fitted page, with Ctrl and
  the wheel or the keys, is kept for its host in the same file, and a
  page going to a site takes that site's zoom, or 100%. One Horadric sets
  itself (`Tab::own`) is not taken for the user's. Not seen on screen
  yet: a posted Ctrl and wheel does not zoom Chromium, which reads the
  real keys.
- **Site pictures** (`favicon.rs`). A tab shows its site's picture
  where no agent's badge is, and so does each suggestion. WebView2 hands
  it over as a PNG on `FaviconChanged`; WIC decodes it once to 32 pixels
  square, and the PNG is kept by host in `favicons\` beside the state, so
  a suggestion has it before its site is opened again. A tab going to
  another site shows the one kept for it until the page sends its own.
  Seen on screen: Hacker News and GitHub on their tabs and in the list.
- **Find and downloads.** WebView2's own find bar and download list were
  there all along, Ctrl+F in the page and the list a download opens. The
  header's menu now has Find in page, which asks for the words and opens
  the find bar on them (`ICoreWebView2Find`), and Downloads, as does
  Ctrl+J in the page. Seen on screen through a test hook since taken out:
  "points" found 30 times on Hacker News, all lit, with the bar's count
  and arrows, and the Downloads list opened over the page.

Tested on screen with a dev instance on its own port and `cmd.exe`
sessions in two projects: Ctrl+Shift+B put the pane in the grid and asked
for an address, `example.com` loaded with its title in the header, a
DevTools `Page.navigate` from a script moved it, and a value set in the
page over DevTools was still there after the stage switched to the other
project and back, so the page was parked and not reloaded. Two processes
started the WebView on one profile at once and shared it.

Tabs tested on screen with a dev instance on its own port and `APPDATA`
and a `cmd.exe` session: the strip showed the page's title, `+` and Ctrl+T
in the page opened a tab with the keyboard in its address field, Ctrl+L
and an address loaded it and named the tab, a click on the first tab
brought its page and address back, a middle click on a link opened it in
a tab of its own, a middle click on a tab and Ctrl+W closed tabs, and
Ctrl+W on the last closed the pane. Found on the way: the hidden grid's
focused block cursor, a fill rather than a caret, showed through the
strip, so a browser pane now draws none of its grid.

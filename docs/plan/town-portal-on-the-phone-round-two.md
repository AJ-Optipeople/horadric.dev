# Town Portal on the phone, round two

Asked for on 2026-10-12: the phone should feel complete, not like a
terminal squeezed onto a small screen. IDEAS.md said "an inbox first,
not a terminal", and the page today is a list and a terminal. Each item
below is a quest.

**Rules for every quest here.**

- The page lives in `../horadric.portal` (`public/app.js`, `style.css`,
  `sw.js`, `index.html`); the PC side in `crates/horadric-ui/src/portaling.rs`
  and its `portal_*.rs` neighbours. Commit each repo on its own `main`.
- New messages go into [PORTAL.md](../PORTAL.md) under "Added in 0.N" in the
  same commit, and stay additive: a page that does not know a message
  ignores it, and so does a PC. The page must still work, minus the new
  thing, against a PC that has not got it.
- Test the page in the browser pane, at phone size, against a dev
  instance through a local relay (`node server.js` in `../horadric.portal`
  and `"relay"` in the dev `portal.json`), or `test/fakepc.js` for page-only
  work. The relay's tests stay green.
- Bump `CACHE` in `sw.js` when the shell changes. The page deploys with
  `deploy.sh` only when the human says so, since every phone gets it.

**The inbox.**

1. *The PC says what a waiting session asks.* `sessions` entries gain
   `ask`: `{"kind":"permission"|"question"|"input","tool","detail",
   "options":[{"label","keys"}],"since"}`. Permission from the
   PermissionRequest or Notification hook (tool name and its input, the
   command for Bash, the path for an edit); question from AskUserQuestion's
   input, one option per choice; `keys` is what to type to pick it on that
   agent's prompt. `since` is when it began waiting. Pure and tested.
2. *The phone's inbox.* Waiting sessions on top of the list as cards: the
   project, the question, how long it waited, one button per option. A tap
   sends the keys as `input` and the card shows "Sent" until the phase
   moves. "Open" goes to the terminal. A count in the title ("2 waiting").

**Push.**

3. *Pushes that say what waits.* The PC encrypts a payload for each
   subscription (RFC 8291, `aes128gcm`, the subscription's `p256dh` and
   `auth`), so the push service still reads nothing: `{"id","project",
   "title","body","waiting":n}`. The service worker shows "horadric.dev:
   Allow Bash?" with the session as the tag, a click opens the page on
   that session (`/#s=<id>` read by the page), and `setAppBadge(waiting)`.
   The page sets the badge too while open, and clears it at none. Without
   a payload (an older PC) the worker shows today's text.
4. *Answer from the notification.* Where the browser offers notification
   actions (Android, desktop), the push carries up to two options and the
   worker shows them as actions. An action opens the page with the answer
   in the fragment, which the page sends once connected, then shows the
   session. iOS has no actions; there the click opens the card.

**The terminal.**

5. *Keys that fit the moment.* Esc, Ctrl+C, arrows and Tab always; y, n
   and the digits only while the session waits, and the digits only as
   many as the `ask` has options (all of 1 to 3 when no `ask`).
6. *Pinch to zoom* the screen text in place of A− and A+, the size kept
   as `horadric.textzoom` is today, anchored where the fingers are.
7. *A screen that holds still.* Render only lines that changed instead of
   all of `innerHTML` four times a second, so scrolling back and a
   selection survive an update. While scrolled up, a "Latest" button
   floats above the keys and new output does not move the view.
8. *Swipe between sessions.* A horizontal swipe on the screen goes to the
   next or previous session of the same project, with a short slide, and
   a dot row under the title shows where you are.
9. *The keyboard never covers the input.* On iOS, size the session view
   from `visualViewport` so the keys and the input sit on the keyboard,
   and the screen keeps its bottom line in view.

**Feel.**

10. *Transitions, haptics, a skeleton.* Views slide in and out (back
    slides the other way), `navigator.vibrate` ticks on send and on an
    answer where the phone has it, and a few grey rows stand in for the
    list while connecting instead of an empty page. Honour
    `prefers-reduced-motion`.
11. *Long press a session* for Rename, Stop and Copy last reply. New
    messages: `rename` (as the tile menu renames), `stop` (as closing its
    pane does, after a confirm), and `reply`, answered with the agent's
    last message from the Stop hook's transcript, which the page copies.

**Bigger.**

12. *What changed.* `{"t":"changes","id"}` answers the session's working
    tree's changed files with plus and minus counts (as the files tile
    has them), and `{"t":"diff","id","path"}` one file's diff, cut at
    200 KB. The page lists the files under the terminal's title and shows
    a diff coloured green and red, wrapped for the phone.
13. *Attach a photo.* A camera or library button by the input. The page
    shrinks the image to fit under the 1 MB frame, sends `{"t":"attach",
    "id","name","data"}`, the PC saves it under the data dir's
    `portal-attach\<session>` and answers its path, and the page puts the
    path into the message for the agent to read. Files older than a week
    are removed.
14. *Warriv and quests on the phone.* `{"t":"quests"}` answers each
    project's quest log (titles, state, the one a session holds) and
    whether Warriv drives; the page shows them per project and can `quest
    add` a title with notes. Read and add only; no driving from the phone
    yet.

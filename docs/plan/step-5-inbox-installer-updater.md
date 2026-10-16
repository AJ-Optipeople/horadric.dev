# Step 5: inbox, installer, updater

- The inbox, a list of the sessions waiting on you, is dropped (decided
  2026-09-26). The tiles, the hotkey that walks the waiting sessions (see
  The stage) and the notification already give the overview.
- A signed updater, planned below under "The updater". `horadric install`
  covers installing for now; NSIS only if a download for other people
  needs it.
- The tray icon exists (see Launchers). "Start with Windows" is a checked
  item in its menu, a value under the user's `Run` key that starts
  `horadricw.exe`. A dev instance has no such item, and `autostart` itself
  refuses to write the value under `HORADRIC_DEV`, so no path turns it on.
  "Show terminal" opens the stage again after its cross closed it, on the
  project it showed then, or the first cluster with a terminal when that
  one is gone, and brings it to the front when it is open. Greyed out
  with no terminal to show.

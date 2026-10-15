# Install

`horadric install` makes Horadric a normal per user app, no admin rights:

- Copies `horadric.exe` and `horadricw.exe` to `%LOCALAPPDATA%\Programs\Horadric`.
- Adds that folder to the user `PATH` (registry, then `WM_SETTINGCHANGE`).
- A Start menu shortcut to `horadricw.exe`, so Windows search finds Horadric.
- Points Explorer's "Open in Horadric" and Start with Windows at the copy.
- Installs the Claude Code hooks, then starts Horadric.

`horadric uninstall` reverses it and leaves the saved state. The executables
carry the icon and version information, baked in by `crates/horadric/build.rs`,
which writes the `.res` file itself from the tray icon's drawing code, so
Explorer, Start, the taskbar and Task Manager all say Horadric.

`horadric` with no arguments now starts the app hidden and returns. Running
the app inside a terminal meant closing the terminal ended every session.
`horadric app` is the old behaviour, for the log.

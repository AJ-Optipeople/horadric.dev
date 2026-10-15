# Reload

Shipping a build used to be four steps from a terminal outside Horadric and a
click on every tile. Now it is `target\release\horadric.exe reload`, run by
the agent when the human says ship.

- **The request.** `reload` posts `/horadric/reload` with the path of the
  binary it was run from, guarded like `/horadric/new` (its own
  `X-Horadric-Command` value, no `Origin`). `--now` skips the wait.
- **The wait.** The app hands over once no session in a Horadric terminal is
  mid turn (`Phase::mid_turn`, only `Working`). A session waiting on you
  is not mid turn; it resumes to the same question. The agent that ran
  `reload` finishes its turn first, which is why the command returns at
  once instead of waiting. The tray tooltip says a reload is pending.
- **The handover.** The app saves with `running` marked on every session
  whose process is alive and `on_stage` naming the session with the
  keyboard on the stage,
  starts `<new build> swap --pid <its pid>` with no window, and quits.
- **`swap`** waits for that process to exit, moves each installed binary
  aside as `horadric.old.exe` and `horadricw.old.exe` (a running binary can be
  renamed, not overwritten), copies the build in, rewrites the hooks as
  `install` would, and starts `horadric.exe app --reload`. It counts as up
  when the port answers and the process is still alive 3 seconds later,
  since the app listens before it builds its windows.
- **Rollback.** A build that is not up within 20 seconds is killed, the
  moved binaries go back, and the old build is started the same way. Only
  what was moved goes back, so a copy failing half way restores exactly
  that. `%APPDATA%\Horadric\reload.log` has the last reload.
- **`app --reload`** resumes the sessions saved as `running`, once each,
  without opening a window for each, then puts the stage back.
- **After a crash** the same happens without `--reload`. Every save writes
  `live: true`; only tray Quit writes it false, so a start that finds it
  true follows a crash, a kill or a logoff, and resumes the sessions saved
  as `running`. A start after Quit still leaves every session paused. The
  fuses: once each, never one already heard from since the start (its agent
  outlived the old Horadric), and never twice in a row. A start that
  resumes after a crash writes `recovering: true` for its first 60 seconds,
  and a start that finds both resumes nothing. Tested with a dev instance
  and two `cmd.exe` sessions: killed, both came back, one process each;
  killed again within the minute, both stayed paused.
- **A dev instance** restarts from its own build and copies nothing, which
  is how reloading itself gets tested.

Tested with a dev instance and `cmd.exe` sessions: two running sessions
came back as two, in about three seconds, with the stage showing the same
one; a faked working session held the reload until its `Stop`. The install
path and the rollback were tested against a fake install folder with its
own `APPDATA`, `LOCALAPPDATA`, home and port, the rollback by squatting the
port so the new build could not listen.

The installed Horadric has to know `/horadric/reload`, so the first build with
it is installed by hand. Since session hosts (below) the sessions no longer
end with the process: the new build attaches to them, and the wait for
idle sessions is gone. What this section says about resuming still holds
for a session whose host is gone.

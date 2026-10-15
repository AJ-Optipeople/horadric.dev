# Discord Activity

Asked for on 2026-10-01: Horadric shows on the human's Discord profile
what its agents are doing, the way a game shows "Playing". Discord calls
that line an activity, and a desktop app sets it through Rich Presence.
(Discord's other "Activities", web apps run inside a voice channel, are
not this: Horadric is not a web page and has nothing to embed.)

- **How it talks to Discord.** The desktop client listens on a named
  pipe, `\.\pipe\discord-ipc-0` up to `-9`, the first one free. Each
  message is a frame: an opcode and a length, both little endian `u32`,
  then that many bytes of JSON. Opcode 0 is the handshake
  (`{"v":1,"client_id":"<id>"}`, answered by a `READY` dispatch), 1 a
  command or its answer, 2 close, 3 and 4 ping and pong. The activity
  is set with `{"cmd":"SET_ACTIVITY","args":{"pid":<ours>,"activity":{...}},"nonce":"<n>"}`
  and cleared with `"activity": null`. No SDK and no dependency: the
  frames are `serde_json` and a `CreateFileW` on the pipe, which is less
  code than wrapping Discord's Game SDK DLL and keeps "pure Rust".
- **Its own thread.** The pipe is read and written on a thread of its
  own, so a slow or hung Discord never holds the UI thread. The app
  hands it the latest presence; it keeps only the latest, connects when
  it has one to show, tries the pipes again every 30 s while Discord is
  closed (a missing pipe is the normal case, not an error, and says
  nothing), and sends at most one update every 4 s, since Discord allows
  5 in 20 s and drops the rest. An unchanged presence is never sent.
- **What it says** (pure and tested, from the registry and the setting):
  `details` counts the sessions ("3 agents working, 1 waits for you"),
  `state` names the project on the stage, or the busiest, only when the
  human allowed names. The large image is Horadric's own, the small one
  the state of the most urgent session (waits, working, idle), the
  colours of the lamps. `timestamps.start` is when the current run of
  work began, so Discord counts up "for 1:12:04", and it holds while any
  session works instead of restarting at every turn. No session running:
  the activity is cleared, not "0 agents".
- **Off until turned on, names hidden until allowed.** Project names are
  the human's business and a Discord profile is public to their friends
  and servers. The tray has "Show on Discord" with Off (the default),
  "Without project names" and "With project names", checked as chosen and
  kept beside the other app settings. A dev instance keeps its own, so it
  stays off unless turned on there too, and two Horadrics showing at once
  fight over one activity.
- **Going away.** Quit, reload and the setting going Off clear the
  activity before the pipe closes, since Discord keeps showing a stale one
  for a while after the process is gone. A reload hands over: the old
  build clears, the new one sets it again.
- **The Discord application.** Rich Presence needs an application in
  Discord's developer portal, whose name is what the profile shows
  ("Playing Horadric") and whose id goes in the handshake. The id is
  public, so it is a constant in the code, `1555242626897416212`, made by
  the human on 2026-10-01; `HORADRIC_DISCORD_CLIENT_ID`
  overrides it for testing. Creating the application was the human's
  step. The images are uploaded to it as Rich Presence art assets and
  named by key, unless Discord takes an `https` URL for them, which the
  art quest checks first; then they live in this repository.

Built as quests, in the quest log under "Discord Activity": the pipe
client, what the presence says, the art and the tray setting side by
side in the shared tree, then wiring them together, then the check on
screen with a real Discord.

Done, all of it. Checked on screen on 2026-10-01 with a dev instance,
fake sessions posted to it and the real Discord desktop client, the
profile popout read from screenshots: "Without project names" showed
"Playing Horadric, 1 agent working" with no project line; a second
session working and the first asking for permission gave "1 agent
working, 1 waits for you"; both stopping gave "2 agents idle"; a new
turn after that kept the clock going (0:15, 0:45, 1:15 across all of
it) instead of starting at zero. Picking "With project names" in the
tray added "in beta" within the 4 s, Off took the card away at once,
and on again brought it back. Reload cleared it for the 3 s of the
handover and the new build set it again; Quit, with "Stop them", cleared
it within 2 s. Two things the check left: the images show as Discord's
question mark until `docs/discord` is on `main` on GitHub, since their
URLs are raw GitHub ones, and a reload started the clock again, since
the run of work was not handed over. It is now: the run is kept in the
saved state (its last work cut to the minute, so a working session does
not write the file every tick), and a start finds it there and carries
on, unless the ten minute gap passed meanwhile.

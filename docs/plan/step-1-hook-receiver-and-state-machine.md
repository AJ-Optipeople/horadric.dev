# Step 1: hook receiver and state machine

Claude Code `http` hooks post every lifecycle event to
`127.0.0.1:43117/horadric/hook`. The hook config carries `X-Horadric-Session:
$HORADRIC_SESSION`, so an event arrives already tagged with the session it
belongs to. A `claude` started outside Horadric sends an empty header and the
event is dropped, which is what keeps Horadric from instrumenting the whole
machine. Superset got this wrong and wrote it up in their
`HOOKS_INVESTIGATION.md`; the guard is the lesson.

The state machine is in `horadric-core/src/session.rs`. Prompt and tool events
mean working, permission and notification events mean waiting, `Stop` means
done, `SessionEnd` means ended. Subagent events and compaction restarts
change nothing.

The tag alone is not enough. Claude Code's background sessions (`claude
--bg`, and the left arrow in a session, which opens its agent view) are
started by one shared `claude daemon`, which copies the environment of
the session that started it. Every background session after that posts
under that session's `HORADRIC_SESSION`, whoever asked for it. Seen with
a probe on 2026-09-26: a `--bg` started with its own tag arrived under
the tag of the Horadric session that had spawned the daemon. So a tile
keeps the Claude `session_id` it first hears and ignores events, status
lines included, that carry another. A `SessionEnd` from `/clear` or
`/resume` lets go of the id, so the next conversation can take the tile.
Those background sessions get tiles of their own, see Background
sessions. `horadric run` posts a `HoradricRegister` event of its own before
starting Claude, because Claude Code sends no hook until the first prompt and
a fresh session would otherwise be invisible.

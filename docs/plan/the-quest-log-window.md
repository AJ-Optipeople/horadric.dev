# The quest log window

Asked for on 2026-10-03: a quest done leaves the tile and its session
closes, and there was no way back to what it did or how it ended short of
asking another session.

- **The chronicle** (`horadric_core::chronicle`, pure and tested) is
  `chronicle.jsonl` beside `state.json`, never trimmed, unlike the
  journal's week. A line for each quest accepted, marked (review, blocked,
  done, put back), summed up, added, each turn of its session ended (last
  message, conversation id, folder, tile name), its commits and its merge.
  The app writes most of them (`runner.rs`, `journal_phases`);
  `horadric quest done "summary"` writes the agent's own one line and
  `quest add` inside a session writes who added the quest, which is how a
  quest becomes another's child. Every prompt now asks for that summary.
- **Quests from before the chronicle** come from the list itself, every
  held item, with what the journal's week still knows of them.
- **The diagram** (`chronicle::graph`, pure and tested) is drawn like
  `git log --graph`, newest at the top: the main line is the trunk, each
  quest a lane from where it branched (the trunk, or the quest that added
  it, whose lane runs on until its children branch) until it converged
  back into the trunk when done, or stopped with a cap when blocked or put
  back. Dots take the tile's lamp colours.
- **The window** (`questlog.rs`, `render/questlog.rs`, `chronicler.rs`),
  one at a time, from "Quest log..." in the quests tile's mode menu, a
  quest's right click menu and the project menu. A row a quest: title,
  outcome, age and its one line result. Click one for the detail: times,
  where it came from, what came of it, notes, commits, merged branch.
  "Read the session" turns its transcript into text in
  `chronicle\<id>.md` and opens it read only on the stage; "Carry it on"
  resumes the conversation in a new tile, or shows the one that holds it.

Checked on screen with a dev instance and a dozen fake quests in dark,
light and narrow, and the recording with `cmd.exe` as the agent and a
fake `Stop`. Not clicked on screen: the three menu entries (the window was
opened by messages), and the `Commits` and `Merged` records.

**Conversations on the main line** (asked for on 2026-10-03, folding the
History menu in):

- **What shows.** The project folder's newest 40 conversations of every
  agent (`past_in`, the History menu's reader with its rules: untitled
  ones left out), less those a quest holds and those in the stash. A
  conversation a live tile holds stays: it is usually the main session
  the quests came from, and Carry it on shows its tile. They are read
  again every 30 ticks, since nothing says when one starts.
- **Where.** `chronicle::with_talks` (pure and tested) puts each among
  the quests as a `Quest` with `main` set, at when its transcript file
  was created, and `graph` gives it a row on the trunk and no lane. A
  dot of its own colour (the magic blue), a faint band behind its row.
- **A quest added in one** branches from its dot: `quest add` now records
  the conversation of the session that ran it (`CLAUDE_CODE_SESSION_ID`,
  which Claude Code gives its commands), and when that session worked no
  quest, the quest's lane starts on the conversation's row (`Row::forks`)
  and runs up to the quest. Quests added before this grow from the trunk
  as before.
- **Its detail**: the title, Started, Last touched, the quests it added,
  and Read the session and Carry it on as for a quest. Read is Claude
  Code's only; a Codex or Grok conversation can be carried on.
- **All conversations...** sits in the main line's band: a session on
  Claude Code's own picker of the folder's conversations.

Checked on screen with a dev instance and `cmd.exe` as the agent: the
tray's Quest log submenu opened the window for a project with no cluster
and for this one, whose real conversations lined the main line between
the quests; a conversation's detail, Read the session (the transcript on
the stage), Carry it on (`--resume <id>`) and All conversations
(`--resume`) all clicked; the project menu's Quest log... switched the
window; a fake `Added` with a conversation drew the fork from its dot and
listed it under the conversation. Not clicked: the start window's right
click, which was not up with clusters open.

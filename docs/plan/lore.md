# Lore

Asked for on 2026-10-15: agents that use fewer tokens to do the same
work, so more quests run on the same subscriptions and the caravan goes
further by itself. Tools like Graphify and codebase-memory-mcp claim
large savings by giving an agent a map of the code instead of letting
it grep and read. Measured honestly the savings are 2x to 10x on lookups,
not the 70x in the headlines. Horadric can do better than a tool inside
one session, because it sees every session of a project: their hook
events, the files they read and change, and the quest each one works.

The first saving needed no code. `docs/PLAN.md` was 366 KB, about 90k
tokens, and every session was told to read it. It is now an index with
the parts in `docs/plan/` (2026-10-15).

Six parts, built in this order. Each is a quest.

## 1. Measure

Nothing below can be judged without numbers. Read each session's
transcript (`horadric-hooks/src/transcript.rs` already reads its tail)
for the `usage` of every assistant message: input, cache read, cache
write and output tokens. Sum them per session and per quest, and split
by the tool whose result the tokens paid for (Read, Grep, Glob, Bash,
the rest). Keep the totals with the quest when it is done, so the quest
log window can show what a quest cost and a comparison can be made
across weeks. Pure parsing and summing, tested on transcript fixtures.

Built 2026-10-15. `horadric-core/src/spend.rs` reads a transcript: a
message counts once (its lines repeat its usage), and the whole call is
charged to the tools whose results started it, split evenly, or to
"Prompt" (a subagent's own transcript: "Agent"). MCP tools go by server.
`horadric quest done` and `blocked` sum every conversation the chronicle
saw a turn of under the quest, with their subagents, and append a
`spent` record of one sitting each; the last one is the quest's. The
quest log's detail shows "Spent" and "Went on". A quest marked done any
other way (by the human, the runner or Warriv) gets its `spent` record
from the app when it sees the mark, unless one came since the quest was
last taken, with the quest's sessions still running counted too. A
conversation with no Claude Code transcript is looked for among Codex's
rollouts, whose `token_count` events give each call's usage, charged to
the tool outputs before it the same way.

## 2. A context pack per quest

When a quest starts, the agent explores from nothing. The hooks already
say which files each session read and changed (`overlap.rs` tracks
them). Record that per quest when it is done. When a new quest starts,
find the finished quests most like it (title and notes words, the same
`--after` chain, the same plan parts named) and put a short pack in its
prompt: the plan parts and files they needed, at most a dozen, each with
why. Warriv may write or correct a pack when it files a quest. A pack is
a hint, never a rule: the agent still reads what it needs.

Built 2026-10-15. `horadric_core::pack` reads a transcript for the files
it read (Read, a Grep of one file, a path named in a Bash or PowerShell
command, since agents `cat` as often as they Read) and changed (the
writer tools), relative to the session's tree, so a worktree's paths
match the main tree's. `quest done` keeps them as a `touched` record. A
quest that starts gets the files of the three finished quests most like
it, changes weighing double reads, files its notes name first, at most
12, files gone from the tree left out, and a `packed` record says it
had one. Alike is shared title and notes words weighed by rarity, one
`After:` the other or both the same quest, and the same plan parts
named. Warriv writes or corrects a pack through the quest's notes: a
path named there leads the pack. `horadric quest pack "title"` shows
what a quest would get, the memory's strong recall of it too, asked of
the running app (it says so when no app answered).

`horadric quest spend` sets done quests started with a pack beside
those without, and first fills in the spend and files of quests done
before either was kept, from their last conversation. The baseline on
2026-10-15, 63 quests without a pack: median 6.5M tokens and 60 calls,
Read 8%.

The comparison on 2026-10-16, once 10 quests had run with a pack: with
a pack, median 4.9M tokens and 45 calls, Read 6%; without (73 quests by
then), median 6.3M tokens and 55 calls, Read 7%. That is about 22%
fewer tokens and 18% fewer calls. Read's share barely moved, so the
saving is less reading than less looking: fewer searches before the
agent knows where to work. Ten quests is a small sample, and they are
the latest ten, run while more of the lore (notes, the code index) came
in beside the pack, so the pack alone does not own the whole drop. It
points the right way, and the pack stays on. Run `quest spend` again
past 30 packed quests before tuning the pack's size or weights.

## 3. Notes shared between sessions

Five sessions in one project each find out the same things. Two tools on
Horadric's MCP server: `lore_note` (a fact, the files it is about) and
`lore_recall` (a question or a file, answered with the notes that match).
Notes live per project in `%APPDATA%\Horadric\lore\<project>.json`, not
in the repository. A note names its files and the commit it was true at;
when one of those files changes after that, the note is shown as maybe
stale. Matching is plain words and file paths. Embeddings come later,
see [memory.md](memory.md) (2026-10-16).

Built 2026-10-15. `horadric_core::lore` holds the notes and the
matching; `crates/horadric/src/lore.rs` the file, a lock file so
sessions take turns writing, and git for the project and commit. The
file is `lore\<folder>-<hash of the project key>.json` in the state
folder, so a dev instance keeps its own. Choices made along the way:

- Staleness is a hash of each file's bytes when the note was taken, not
  a git diff from its commit, so a change not yet committed counts too,
  and a session in a worktree checks its own copy of the file.
- `lore_note` takes `replaces`, ids of notes it corrects, which leave
  the notes for the history, marked with when they stopped being true
  (memory part 7). There is no forget tool: one tool fewer in every session's
  list, and a wrong note is best answered with the right one.
- A project keeps at most 500 notes, the oldest dropped first. A recall
  answers with at most 10.
- The lore tools are offered to every session the MCP server runs in,
  Horadric's or not, since they need no app; the browser tools still
  only to Horadric's.

Changed after a review on 2026-10-15: an agent mid task rarely stops to
call `lore_note`, so most notes should not wait for one. Horadric takes
them itself when a quest ends: the done summary, the files the quest
changed, and what Warriv learned on a wake, each as a note naming its
files. The tools stay for what only the agent knows.

Built 2026-10-15. `horadric quest done` keeps `Quest "<title>" done:
<summary>` naming the files the quest changed: in a worktree, what its
branch changed since it left main, committed or not; in the main tree,
where other sessions' changes mix in, the files its own conversations
wrote with Edit and Write. At most 30 files, Horadric's own left out. A
quest done again replaces its note. When Warriv rests, each rule it
added to its memory becomes a note naming the project files it
mentions; Lately is what it did, not what it learned, so it is left
out. The note store moved to `horadric_hooks::lore` so the app can
write too.

## 4. A code index

Symbols and who calls whom, per project, built once and kept fresh as
files change, served by `lore_find` (where is `x` defined), `lore_calls`
(who calls `x`, what does `x` call) and `lore_outline` (a file's symbols
without its bodies). Shared by every session of the project, so it is
built once and not per agent.

Code is read with tree-sitter (decided 2026-10-15): the `tree-sitter`
crate and one grammar crate per language, starting with Rust,
TypeScript and JavaScript, and Python. Accurate, fast, and what the tools
this idea comes from use. Language servers were turned down because they
differ per language and are slow to start; a ctags style regex outline
because it misses calls. The quest that builds it adds tree-sitter to
the dependency list in CLAUDE.md, with the reason.

Order, changed after a review on 2026-10-15: `lore_outline` first. The
largest saving is a big file's symbols without its bodies (`app.rs` is
8,000 lines), and nothing else gives that. Claude Code already has an
LSP tool (go to definition, find references) backed by rust-analyzer,
so before building `lore_find` and `lore_calls`, check what it answers
in a Rust and a TypeScript project, and build only what it does not.

Built 2026-10-15, all three. The review reached the quest after the
index was written, so the check came last: in this repository Claude
Code's LSP tool answered nothing, as its rust-analyzer crashed three
times and was given up on. A language server has to be installed per
language, set up per machine and indexed per session; the lore's index
needs none of that and is shared. So `lore_find` and `lore_calls` stay,
and where an LSP does work it is the surer answer for a method call,
which the index can only match by name.

`crates/horadric/src/code.rs` reads a file into its symbols (each with
its kind, lines, head and the symbol it is in) and its calls (each with
the symbol it is made in), with one query per language written for the
index rather than the grammars' own tags queries, and answers the three
questions. `crates/horadric/src/lore.rs` lists the working tree's files
(`git ls-files`, so ignored folders are left out) and keeps the index.
Choices made along the way:

- The index is kept per project in `lore\<folder>-<hash>.code.json`,
  each file by its language and the hash of its bytes, not by its path.
  So a file is read once whichever worktree has it, and a worktree whose
  copy differs reads its own without the two trading places. A file no
  tree has had for 30 days is dropped.
- Fresh without a watcher: each question stats the tree's files and
  reads again only those whose time or size changed since this process
  last looked. On this repository (4 MB of code) the first question of a
  session takes about 2 s in a release build, the rest under half a
  second.
- Calls are matched by name, as tree-sitter knows no types. What helps:
  a call written with a path (`Lock::take`, `code::read`, `Self::x`,
  `self.x()`) resolves to that type or module; a bare call only to free
  functions; a call on any other value is listed apart as "maybe the
  project's"; and of several definitions the one in the caller's own
  file wins.
- `lore_outline` reads its one file each time instead of using the
  index: as quick, and never stale.
- Files over 1 MB, `.min.js` and `.d.ts` are left out as generated.

## 5. A shorter quest log

`.horadric/tasks.md` was 95 KB on 2026-10-15, about 24k tokens, most of
it done quests, and Warriv and quest sessions are told to read it. Done
quests older than a few days move to `.horadric/tasks-done.md`, keeping
their `@id`, notes and spent records, so the quest log window, the
chronicle and lookups by id still find them. The live file holds what is
open plus the last few days.

Built 2026-10-15. `tasks::sweep` takes the done quests out, notes and
all, each under the heading it had; `horadric quest done` runs it after
marking, so the log is tidied as often as quests end. A quest is old when
the chronicle's last record of it is more than a day back (three until 2026-10-17)
(`KEEP_DONE`), or when the chronicle never heard of it. The done file
sits beside the list, `quests-done.md` beside `quests.md`. Choices made
along the way:

- A done quest that a quest staying in the log names in `After:` stays
  too, or that name would match nothing and its quest would need the
  human. `quest add --after` an archived quest drops the line, since
  there is nothing left to wait for.
- A heading whose section the sweep empties goes with it; one with prose
  under it stays.
- Every write of the list (`tasks::update` in `horadric-hooks`) now takes
  a lock file beside it, `.horadric/tasks.lock`, the same `Lock` the
  lore notes use. Before, two writers at once could lose one's change;
  with the sweep, two could also archive the same quests twice. An
  agent's own Edit of the file takes no lock and never did.
- The quest log window reads the done file before the list. The spent
  records were always in the chronicle, by id, so they need no move.

## 6. The window

Asked for on 2026-10-17: more saving than the pack gives. The spend
records say where to look. One quest of that week: 108 calls, 14.5M
tokens, 98% of them read from the cache, and 90% charged to Bash. Almost
nothing a quest spends is fresh input. The cost is the context window
sent again with every call, so a quest costs its window times its calls,
and Bash gets the blame only because nearly every call is one. Reading
less trims the window a little, which is why the pack gives a fifth and
not five times. The next savings attack the two factors themselves.

Built 2026-10-17, the measures and the first lever:

- **The window is measured.** A spend record now keeps the window of a
  conversation's first call (the system prompt, the instructions, the
  tool list, the quest's prompt and pack: what a session starts with
  before it has done anything) and of its last, and the quest log's
  detail shows "Window: 57k at the start, 180k at the end" and the
  tokens a call beside the total. `horadric quest spend` gives the
  medians, and filled the window in for the 102 quests recorded before
  it was kept. The baseline on 2026-10-17, 103 quests: median 108k to
  113k tokens a call, and **52k to 57k at the start**. Half of an average
  call's window is there before the agent has read a line. What fills
  it, itemised below the same day.
- **`horadric heavy` prints a digest.** A `cargo test` or a build prints
  thousands of lines, and every line the agent sees stays in its window
  for every call after. The command's output now goes whole to a file in
  the temp folder (`horadric-heavy\<time>-<pid>.log`, kept a day) and
  the agent is shown the last 12 lines when it passed, or when it failed
  the lines that say what went wrong (errors, failed tests, panics, up
  to 60) and the last 40, each with its line number in the file, and the
  file's path to grep. Output no longer than the digest is shown whole.
  `--full` prints everything, for the human watching. The system prompt
  tells sessions to use it for any long output, CPU or not. Standard
  output and error go into the one file so they keep their order, which
  the live terminal could not promise either.
- **The quest log keeps a day of done quests, not three.** At the rate
  quests end now, three days held a hundred of them and 83 KB, about 21k
  tokens in the window of every session or Warriv wake that opened the
  file instead of asking `quest list`.

Built 2026-10-17, later the same day, the two measures that were still
missing and what they showed:

- **The start is itemised.** `horadric quest start` weighs each piece of
  a session's first window Horadric can read back (its own prompts, its
  MCP instructions and tool list, the two CLAUDE.md files, the memory
  index, and the skills and agents each enabled plugin lists in every
  session, from their front matter), against the median first window,
  and calls what is left Claude Code's own. On this machine: Horadric's
  prompts and MCP server together about 2k, the project's CLAUDE.md 3k,
  the enabled plugins 6k (Vercel and Figma most of it), and **39k of 53k
  is Claude Code's own prompt and built-in tools**. So the start is not
  where Horadric can save much, and neither are the plugins: the guess
  that they were a large part was wrong, and the itemised count is what
  says so.
- **Results are measured.** A spend record now counts the characters
  each tool's results put into the window, the quest log's detail shows
  "Results: about 110k tokens of results: Read 78k, Bash 22k", and
  "In subagents" gives the share spent in Agent. Over the 102 quests
  with records: **Read is 71% of all result text, Bash 22%, Horadric's
  MCP tools 3%**, and the median share spent in subagents is 0%, no
  quest above 5%. Whole files read are what fills the window; the
  browser snapshot, `lore_recall` and `quest list` together are the 3%,
  so trimming them was not worth a line of code. What was: the lore
  instructions every session gets now say that a Read stays in the
  window for the session, to outline a file first and Read a range, and
  to send a many file search to a subagent. Whether words move the
  number, `quest spend` will say in thirty quests: Read's share of
  results and the Agent share are the two to watch.

Not yet:

- If the words do not move it, a harder lever: a PreToolUse hook that
  rewrites a Read of a whole file over a few hundred lines into an
  outline, or refuses it with the outline in the reason. Built and
  benched on 2026-10-17, see part 7: it never fired, and is not on.
- Compaction. Claude Code compacts when the window nears its limit; a
  quest that would end at 180k can go on from a 40k summary after its
  exploration. Whether Horadric can ask for one at the right moment
  (after the pack is read, before the edits) is an open question of
  Claude Code's interface, not of Horadric.

## 7. The bench

Asked for on 2026-10-17, after a search of what the internet claims:
20x to 30x fewer tokens from code graphs, output compressors and rule
files. Read closely, those numbers are one tool's output or one lookup,
measured by the tool's own author. Independent end to end tests found
20% to 43% at best, and three code graph servers that made sessions
cost 22% to 27% more, since the agent mostly ignored their tools. The
way to know is to measure on this project's own work, and Horadric can,
since it already reads what a session spent.

Built 2026-10-17. `horadric bench <spec>` runs each task of a spec under
each variant, a few times each and round robin, as a headless `claude
-p` in a clone of its own, and reports each variant's median tokens,
calls, tokens a call, window at the end and result text, beside the
first variant's. A variant is a line added to the system prompt,
Claude Code settings (hooks, with `{horadric}` for this build), extra
environment, its own model, or the user's plugins. A task is a prompt
with a check: a shell command that must exit 0 in the clone, or words
the answer must hold. Choices made along the way:

- A clone, not a worktree. CLAUDE.md tells an agent in a worktree to
  merge it into main, and a clone with no remote has no main to merge
  into and nowhere to push.
- The runs read only project settings (`--setting-sources
  project,local`), so the user's hooks do not post them to the running
  Horadric as sessions, and their plugins are off unless a variant asks.
  The lore's MCP server runs from a copy of the build with the dev
  state, so the runs' notes stay out of the project's.
- One shared build folder, filled once by the spec's `prepare` command,
  so no run pays for compiling the dependencies, and dropped when the
  bench ends. Every run is appended to `results.jsonl` as it ends, with
  its answer and diff beside it, so a bench that is stopped keeps what
  it measured; `horadric bench report` reads it again.

The first bench, `docs/bench/window.json`, Sonnet, two question tasks
and one edit (`--json` for `quest spend`, checked by the tests and the
diff), five variants: none, lean rules in the system prompt, the read
guard, the shell guard (`horadric guard`, see below), and all of them.
25 of 30 runs finished before the machine ran low on memory and the
bench was stopped; every one passed.

| Task | Baseline | Rules | Read guard | Shell guard | All |
|---|---|---|---|---|---|
| pack (question) | 149k | -19% | -17% | -18% | -18% |
| stage (question) | 278k | +30% | -6% | -9% | +45% |
| json (edit, one run each) | 328k | -26% | +33% | +34% | +52% |

What it says:

- **The guards never fired.** Not one run read a whole long file or ran
  a build outside `heavy`: given the lore's wording (part 6) and the
  heavy prompt, the agent outlines, reads ranges and digests by itself.
  So the guard variants were the baseline under another name, and
  their spread, from -18% to +34%, is **the noise between two runs of
  the same thing**. Any claim of a saving under about 40% from one or
  two runs is inside it. The guard stays built and off.
- **Rules in the prompt are inside the noise too**: -26% to +30%.
- **These runs cost a tenth of a real quest.** 120k to 500k tokens, 4
  to 13 calls, a window of 30k to 45k, against a quest's median of 5M,
  46 calls and 110k. The task is smaller, but not ten times smaller,
  so the gap is the lever worth finding. What differs: the model (real
  quests run on Opus), the start (plugins, the browser tools, the
  quest's prompt and pack: 53k against 30k), and that a real quest is
  driven by a person or Warriv over many turns.

`horadric guard [read|shell]`, the hook benched here: a Read of a whole
file over 300 lines, or a plain `cat` of one, is turned down with the
file's outline as the reason (on `app.rs`, 23k characters in place of
354k), and a plain build or test is rewritten to run through `heavy`.
A turned down read costs a call, which carries the whole window, so the
guard only pays where a session reads whole long files, which these
did not.

Next: `docs/bench/where.json`, the edit and a question under Sonnet and
Opus, each with and without the user's plugins, one run at a time to
spare the memory. It says how much of the tenfold gap is the model and
how much the start. Not run yet: the machine was short of memory when
the first bench was stopped, so it waits to be started by the human.

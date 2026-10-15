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

Four parts, built in this order. Each is a quest.

## 1. Measure

Nothing below can be judged without numbers. Read each session's
transcript (`horadric-hooks/src/transcript.rs` already reads its tail)
for the `usage` of every assistant message: input, cache read, cache
write and output tokens. Sum them per session and per quest, and split
by the tool whose result the tokens paid for (Read, Grep, Glob, Bash,
the rest). Keep the totals with the quest when it is done, so the quest
log window can show what a quest cost and a comparison can be made
across weeks. Pure parsing and summing, tested on transcript fixtures.

## 2. A context pack per quest

When a quest starts, the agent explores from nothing. The hooks already
say which files each session read and changed (`overlap.rs` tracks
them). Record that per quest when it is done. When a new quest starts,
find the finished quests most like it (title and notes words, the same
`--after` chain, the same plan parts named) and put a short pack in its
prompt: the plan parts and files they needed, at most a dozen, each with
why. Warriv may write or correct a pack when it files a quest. A pack is
a hint, never a rule: the agent still reads what it needs.

## 3. Notes shared between sessions

Five sessions in one project each find out the same things. Two tools on
Horadric's MCP server: `lore_note` (a fact, the files it is about) and
`lore_recall` (a question or a file, answered with the notes that match).
Notes live per project in `%APPDATA%\Horadric\lore\<project>.json`, not
in the repository. A note names its files and the commit it was true at;
when one of those files changes after that, the note is shown as maybe
stale. Matching is plain words and file paths, no embeddings.

Built 2026-10-15. `horadric_core::lore` holds the notes and the
matching; `crates/horadric/src/lore.rs` the file, a lock file so
sessions take turns writing, and git for the project and commit. The
file is `lore\<folder>-<hash of the project key>.json` in the state
folder, so a dev instance keeps its own. Choices made along the way:

- Staleness is a hash of each file's bytes when the note was taken, not
  a git diff from its commit, so a change not yet committed counts too,
  and a session in a worktree checks its own copy of the file.
- `lore_note` takes `replaces`, ids of notes it corrects, which are
  dropped. There is no forget tool: one tool fewer in every session's
  list, and a wrong note is best answered with the right one.
- A project keeps at most 500 notes, the oldest dropped first. A recall
  answers with at most 10.
- The lore tools are offered to every session the MCP server runs in,
  Horadric's or not, since they need no app; the browser tools still
  only to Horadric's.

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

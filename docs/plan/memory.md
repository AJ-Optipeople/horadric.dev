# Memory

Asked for on 2026-10-16: a long term memory built into Horadric, in
Rust, that finds things by meaning and stays valid for years. It grows
out of the lore ([lore.md](lore.md)), whose notes match on shared words
only. That line ("no embeddings") was written with the first lore plan
on 2026-10-15 without a reason; it is lifted here.

The prompt was MemPalace (github.com/MemPalace/mempalace), a Python
memory for coding agents. What it gets right, and what Horadric takes:

- **Verbatim, not summaries.** It keeps conversations word for word and
  finds them by meaning. Its 96.6 % recall at 5 on LongMemEval comes from
  the raw text with no LLM anywhere. Summaries lose what nobody knew
  would matter.
- **Hybrid retrieval.** Meaning plus keywords plus time, fused.
- **Facts with validity windows.** A fact is true from a time, until
  another replaces it.

What it gets wrong, and Horadric leaves out: AAAK, its "compressed
language" for memories, was sold as 30x lossless and is lossy (84.2 %
against 96.6 % raw on the same benchmark, by its own README). Horadric
compresses bytes on disk, never words. Also left out: ChromaDB, Python,
and 47 MCP tools.

## Decisions

- **The model runs in Horadric, with candle** (decided 2026-10-16).
  Hugging Face's pure Rust ML crate: no DLL to ship, works on macOS
  later. Turned down: `ort` (a native DLL, fastest), Windows ML (no new
  crate, but old ONNX support and Windows only), an Ollama or LM Studio
  endpoint (memory would only work while that service runs). candle and
  whatever tokenizer crate it needs are added to the dependency list in
  CLAUDE.md, with the reason, by the quest that brings them.
- **It holds four things** (decided 2026-10-16): the notes and quest
  summaries the lore has now, transcripts, code, and a reach across
  projects.
- **Text is the truth, vectors are a cache.** Every vector names the
  model and version that made it. A new model means embedding again in
  the background from the kept text, the old vectors serving until it is
  done. This is what makes the memory last years: models come and go,
  the text stays.
- **The app owns it.** One model in memory, not one per MCP server
  process (each session runs its own, and a 300 MB model times ten
  sessions is not acceptable). The MCP server asks the app on its port;
  when the app is not running, `lore_recall` falls back to the word
  matching it has today.
- **One tool, not more.** `lore_recall` searches everything and says
  what each hit is (note, quest, transcript, code), when it was true and
  whether it may be stale. A `kind` argument narrows it. `lore_note`
  stays as it is.

## Long term numbers

Measured on this machine on 2026-10-16: 1,864 Claude Code transcripts
since 2026-08-03, 1.96 GB, of which 19 MB is the dialogue (user and
assistant text). The rest is tool output, which the memory does not keep
(it names the tool and the files instead). That is roughly 90 MB of text
a year.

In chunks of about 1,000 characters, that is 90k chunks a year, a
million in ten years. At 768 dimensions a vector is 768 bytes as int8
and 96 bytes as bits. Ten years: under 1 GB of int8 vectors and 100 MB
of bit vectors. A search scans the bit vectors first (Hamming distance,
a few ms for a million), then scores the best few thousand in int8. No
vector database and no approximate index are needed at this size; if
they ever are, the segments below allow one to be added.

Claude Code deletes old transcripts (`cleanupPeriodDays`, 30 days by
default), so Horadric keeps its own copy of the dialogue from the start.

## The parts

Each is a quest, in this order.

### 1. The model

Run an embedding model with candle, in `horadric-core` or a new crate if
the build time calls for one. The candidates: EmbeddingGemma 300M
(multilingual, 768 dimensions, cut to 256 or 128 by Matryoshka, which
candle may need its pooling and dense layers written for) and a small
BERT one (bge-small or all-MiniLM, 384 dimensions, which candle runs
today). Weights are fetched once from Hugging Face into the state
folder's `models\`, checked by hash, never shipped in the installer.

Before choosing, build the yardstick: 50 or more real questions from
this project's transcripts and notes, each with the passage that answers
it. Measure recall at 5 for words only (today), each model, and both
fused; the time per chunk on this CPU; and the memory used. Keep the set
in the repository so every later change is measured against it.

**Built 2026-10-16.** The crate is `horadric-memory` (candle takes a
minute to build, so it stays out of `horadric-core`). The yardstick is
in `crates/horadric-memory/yardstick/`: 1,686 chunks (705 from the docs,
16 lore notes, 965 of dialogue from September and October transcripts)
and 65 hand written questions. Run it with `cargo run --release -p
horadric-memory --example yardstick`. The models are read from the dev
state folder's `models\`: MiniLM is fetched there when missing, the
other two by hand.

Measured on this machine (i7-11700K, CPU shared with other sessions),
chunks of 759 characters on average, recall at 5:

| | alone | fused with BM25 | fused with words | ms a chunk | memory |
|---|---|---|---|---|---|
| words (the lore today) | 63.1 % | | | | |
| BM25 | 67.7 % | | | | |
| all-MiniLM-L6-v2, 384 | 76.9 % | **87.7 %** | 80.0 % | 74 | 92 MB |
| bge-small-en-v1.5, 384 | 76.9 % | 76.9 % | 72.3 % | 164 | 133 MB |
| EmbeddingGemma 300M, 768 | 80.0 % | 84.6 % | 83.1 % | 232 | 858 MB |
| EmbeddingGemma, cut to 256 | 70.8 % | 80.0 % | 75.4 % | | |
| EmbeddingGemma, cut to 128 | 70.8 % | 73.8 % | 75.4 % | | |

Fusion is reciprocal rank fusion. "Words" is the lore's matching today,
a count of shared words; its scores tie so often that the order ties
are broken in moved its own recall and every fusion with it by several
points, which is one more reason part 3 writes BM25. The time is with
candle using every core. Memory is the working set the loaded model
adds. Gemma's peak while loading is 2.8 GB, with its token table already
kept in half precision (a fifth of a billion numbers; full precision
adds 400 MB).

**Chosen: all-MiniLM-L6-v2** (decided 2026-10-16). Fused with BM25, as
recall will run, it and EmbeddingGemma are a tie: 87.7 % against
84.6 % is two questions of 65, within what a set this size can tell
apart. On cost they are not close: MiniLM takes a tenth of the memory
and a third of the time a chunk, so a year of dialogue (90k chunks)
embeds in under two hours of the whole CPU, against six. Gemma alone is
better on dialogue (81.8 % against MiniLM's 72.7 %), and that is the
part that will grow most, so a larger yardstick may reopen this. MiniLM
is English only; the human writes to the agents in English. Gemma stays
runnable in the crate and the yardstick, and since vectors are a cache
tagged by model, changing later costs one background re-embed.

Weights: sentence-transformers/all-MiniLM-L6-v2 at commit
`1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, SHA-256 of `model.safetensors`
`53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db`,
`tokenizer.json` `be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037`,
`config.json` `953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41`
(Apache 2.0). Google's own EmbeddingGemma repository is gated behind a
licence click; the measurement used the ungated `unsloth` copy, whose
output matches Google's model card to three decimals.

**Fetched 2026-10-16.** `horadric_memory::models::ensure` puts the three
files in `models\all-MiniLM-L6-v2\` from the pinned commit, checks each
against the hashes above before it takes its own name, and writes a
`verified` mark so later starts check sizes only. The download is passed
in: part 3 hands it WinHTTP (`horadric-ui`'s `net::get`), the yardstick
curl. SHA-256 is written out in the module, its one use, instead of a
crate.

### 2. The store

Per state folder, `memory\`: append only segment files, one per month,
each holding the verbatim text of its chunks and their metadata
(project, kind, time, session, quest, commit, files and their hashes),
with the vectors beside it in a file of their own per model. Old months
never change, so they are cheap to back up and safe to read while a new
one is written. The note store's lock file pattern guards writers. A
format version in each file header, and a test that reads files written
by every earlier version.

**Built 2026-10-16**, in `crates/horadric-memory/src/store.rs`. A
segment is `memory\2026-10.jsonl`: a header line naming the format and
its version, then one JSON line per chunk (time, project, kind, session,
quest, commit, files with their hashes, the text). A chunk's id is its
month and line, `2026-10#12`, and never changes. Each model's vectors
are `2026-10.<tag>.vec`: a header with the version, the number of
dimensions and the model's tag, then the line number and the vector in
int8 per chunk (a quarter of f32, and the cosine within 0.01). The tag
is the model and its weights' commit, `all-MiniLM-L6-v2@1110a243`.

Decided while building it: a chunk goes into the month it is **written**
in, not the month it speaks of, so an ended month's text never changes (its vector files do when a
new model catches up: they are a cache); a
backfill lands in the month it ran, and the chunk's own time says when
it was said. A write that died half way leaves a torn tail, ignored when
read and cut by the next write. A line that cannot be read keeps its
place, so the ids after it hold. A file from a newer format (a reload
rolled back) is skipped when read and refused when written, never
misread, and holds up no other month. A tag is letters, digits, `-`,
`_` and `@` only, so it is its own file name. Writers take turns by the
lore's lock file; only the app writes, so its three second takeover of a
stale lock is enough. `Store::catch_up` embeds what a model has not yet, a few at a
time so the background work can stop between calls, and
`Store::forget` drops an old model's vectors once the new one has caught
up. `tests/formats/v1` is a store written by version 1, frozen, and
`tests/formats.rs` reads it; a version 2 adds a folder beside it. The
search is a scan of every int8 vector; the bit vector prefilter waits
until a store is big enough to need it.

### 3. Recall by meaning

`lore_recall` scores each hit by words (BM25, written here, it is small)
and by meaning, fuses the two ranks (reciprocal rank fusion), then
weighs: the asking project above others, notes and quest summaries above
dialogue, recent above old, maybe stale below fresh. Measured against the
yardstick before and after.

**Built 2026-10-16.** The ranking is `crates/horadric-memory/src/recall.rs`,
pure and tested; `keeper.rs` is the store and the model as the app runs
them. Every candidate is ranked by BM25 (its text and its files' paths),
by meaning (cosine at least 0.2, the best 50) and, when files are asked
for, by the lore's file match; the ranks are fused with reciprocal rank
fusion (60), then multiplied by a weight: another project's 0.6, a note
or quest summary 1.0, code 0.95, dialogue 0.9, age `0.7 + 0.3 * 0.5^(days
/ 90)`, maybe stale 0.8 (checked on the best 30 only, since it reads
their files). With no words and no files, the newest come, as before.
A quest summary is a note that starts `Quest "..." done`.

The lore's notes stay where they are; the app copies each into the store
as a chunk of kind note or quest with the note's id (`note`, a field
added to the format without a new version: older builds skip it), so it
gets a vector. A copy whose text is not the note's is not used, so a lore
started over cannot pass old vectors off as new notes. Replaced notes
keep their copy, the start of part 7's history.

The app's memory is its own thread (`horadric-ui/src/memory.rs`), started
by the first recall: it fetches MiniLM if missing and loads it beside,
answering by words until then. A recall embeds the asking project's new
notes first, newest first, for at most 3 seconds; the rest, and anything
else the store lacks, is embedded two at a time with a 100 ms rest while
nothing is asked. `horadric mcp` posts to `/horadric/recall` (header
`recall`, no session needed) and waits 12 seconds; with no answer it
ranks the notes itself with BM25 and the weights, no meaning. Only the
asking project's passages are candidates unless part 6's switch is on. `kind` narrows to
note, quest, dialogue (or transcript) or code. Each hit says what it is:
`#12 (note, 3 days ago, at 1a2b3c4d)` for a note, `2026-10#5 (dialogue,
session 8a8cb053, a minute ago)` for a passage of the store.

Measured on the yardstick (`recall` rows; docs weighed as notes, no
ages), recall at 5:

| | all | doc | note | dialogue |
|---|---|---|---|---|
| words, the lore before | 63.1 % | 52.2 % | 70.0 % | 68.2 % |
| BM25 alone | 67.7 % | 60.9 % | 80.0 % | 63.6 % |
| recall by words (the fallback) | **73.8 %** | 69.6 % | 90.0 % | 63.6 % |
| MiniLM fused with BM25, unweighed | 87.7 % | 87.0 % | 100 % | 77.3 % |
| recall with MiniLM | **90.8 %** | 91.3 % | 100 % | 81.8 % |

The dialogue weight was set by it: 0.75 sank answers that were dialogue
(83.1 % in all, dialogue 59.1 %), 0.9 and 0.95 tie at the top for meaning
and 0.9 is better by words, 1.0 drops by words to BM25's 67.7 %. The
cosine floor cost nothing on the set (the best 50 are all above it).

Not yet: the background embeds with every core, which is fine for notes
and will want a gentler pace once the backfill's chunks are waiting for
vectors (part 4 only keeps the text, and rests between files).

### 4. Transcripts, verbatim

The app reads each session's transcript as it grows (it is told by the
Stop, PreCompact and SessionEnd hooks) and keeps the dialogue: the
human's prompts and the agent's text, with each tool call reduced to its
name and the files it named. Codex rollouts the same way. A backfill
reads what is on disk now. Anything that looks like a secret (see
[secrets.md](secrets.md)) is left out before it is written, since this
lives for years.

**Built 2026-10-16.** The reading is `crates/horadric-memory/src/transcripts.rs`
(pure, tested), the keeping is `Keeper::ingest`, and `horadric-ui/src/memory.rs`
is the thread that runs both. On Stop, PreCompact and SessionEnd the app
hands the transcript's path and folder to the memory thread (not for a
subagent's hooks, not for Grok). A transcript's cursor, bytes read to the
end of the last whole line, is kept in `memory	ranscripts.json`, so each
read takes only what was added, a restart goes on where it was, and a
file rewritten shorter is read again. What is kept: the human's prompts
and the agent's text, `You: ...` and `Agent: ...`, each tool call as
`Agent: used Edit: src/a.rs` (the name and the files its input named, never
the input or the output), packed into chunks of about a thousand
characters of kind dialogue with the session id and the time of the last
thing said. Left out: tool results, meta lines, Claude Code's own compact
summaries, the reminders hooks append to a prompt, `/clear` and
interrupt notes. Codex is read from its rollouts (an event or an item per
message, said once; `function_call` and `apply_patch` as tools).

The backfill is the same ingest over every transcript on disk, newest
first, one file each time nothing else is asked, with the 100 ms rest, so
it shares the CPU politely: at the first start of the memory it reads
`~/.claude/projects/*/*.jsonl` and Codex's `sessions`, and the cursors make
a second start free. A chunk's project is the one git says the transcript's
folder belongs to (the same key as the lore's); a worktree since deleted
keeps its own folder as a key, a loose end for part 6.

Secrets are cut before a chunk is written (`redact`): the well known token
shapes (GitHub, Slack, Stripe, AWS, npm, Hugging Face, Google, JWTs), PEM
private keys, a value after a name that sounds like a secret (`TOKEN=`,
`"api_key":`, `password:`), a bearer token, a password in a link, and any
long mixed case run of letters and digits. Pure hex is left, since commits
and hashes are worth keeping. It cannot catch a password that reads as a
word, which is why `docs/plan/secrets.md` keeps agents from seeing them.
It does not yet know the vault's own values.

The model is now fetched by the first recall, not the first keep: an app
nobody recalls in keeps its dialogue without the download, and its chunks
are embedded when a recall first loads the model.

### 5. Code by meaning

Each symbol in the lore's code index (its head and doc comment, not its
whole body) gets a vector, refreshed when its file's hash changes, so
"where do we roll back a failed reload" finds the function.

Done: the index reads each symbol's doc comment (Rust `///`, JS and TS `/** */`, Python docstrings), and the app's memory thread, once a recall has come in, syncs the project's code when idle: one `code` chunk per symbol (path, kind, name, head, doc), added for a file whose hash the store lacks and left alone otherwise. A chunk of a file that has since changed or gone is no longer an answer. The old chunks stay in the store and are embedded too, a small waste a later compaction can remove. The binary hands the index over with `horadric_ui::memory::set_code_source`.

### 6. Across projects

Every project's memory is searchable from every other, behind a switch
in Settings, off by default. Hits from another project say which.

**Built 2026-10-16.** The switch is "Recall across projects" in Settings,
Privacy, kept in the saved state as `recall_across` and told to the memory
thread (`memory::set_across`). Off, only the asking project's passages are
candidates, as before. On, every other project's chunks in the store are
too (dialogue, code, and the copies of notes that project's recalls made),
weighed 0.6 as already built, and a hit says `from <project>` in its head.
A project whose notes were never recalled has no copies yet, so its notes
only appear once something has recalled in it.

### 7. Validity and consolidation

Transcripts are a record of what was said then, not of what is true now.
Facts are notes. So when Warriv does its daily look back, it reads the
day's new dialogue and writes what was decided or learned as notes, each
linking the chunks it came from, replacing notes it contradicts. A note
keeps when it became true and, once replaced, when it stopped; recall
shows the current one and can be asked for history. This is the step
that makes the memory intelligent rather than a pile, and it is done by
an agent in the open, with its notes readable, never by a hidden
rewrite of the kept text.

**Built 2026-10-16.** A note keeps `since`, when what it says became
true, and `sources`, the ids of the passages it came from
(`horadric_core::lore`). `lore_note` takes `sources`: the passages are
looked up in the store before the note is kept, an id that names none
is refused, and the note is true from the earliest of them. A note that
`replaces` others no longer drops them: they move to the lore's `past`,
each with `until` (when the new note was written) and `by` (its id), at
most 1,000 kept, and ids are never given again. `lore_recall` answers
with current notes only; `history: true` adds the replaced ones,
weighed at 0.7 so what is true now comes first, each saying `REPLACED
by #12 a day ago`. A current hit says `true since ...` when that is
before it was written, and `From: 2026-10#3, ...` for its sources.

The look back (`horadric_core::lookback`, Warriv's first round after
04:00) is now handed the day's dialogue too: the app reads the months
of the store since the last look back (`Store::text_since`, text only,
no vectors), keeps the project's dialogue said since, leaves out the
conversations of Warriv's own wakes and errands (they talk about the
quest log, not the project), and writes it to
`memory\day\<project>.md`, each passage headed by its id, the newest
160,000 characters (about 40k tokens) when the day said more. The
prompt names the file and asks Warriv to keep what was decided and
learned with `lore_note`, one fact a note, recalling first, with its
sources, replacing what it contradicts. The pure parts are
`horadric_memory::distill`.

Fixed after review the same day. The look back picks passages by where
they were stored, not when they were said: a passage said before 04:00
but stored after (a backlog still reading, a turn still running) was
missed by both look backs. `memory\day\<project>.mark` keeps the last
chunk id read, written from `.next` once the look back is told; the
next one reads what was stored after it, said no more than a day before
the last look back, so a backfill of old transcripts is no day's news.
Warriv's talk is left out by every conversation the chronicle ever
named for its wakes and errands, and by those of its sessions running
now, so a running errand and a wake named only on an older line are
skipped too. A Warriv session starts with `--add-dir` on the day's
folder, which lies outside the project: without it Claude asks before
reading the file. With the across switch, another project's note copy
comes in only while that project's lore holds the note, same text, as
current: a replaced one is not what that project knows now.

Not yet: the older passages of a day too long to fit are left out for
good, since the next look back starts where this one did; the
yardstick has no questions about validity yet.

### 8. Horadrim, the written tongue

Asked for on 2026-10-16: a compressed language of Horadric's own, aiming
lower than AAAK's claimed 30x but losing nothing that recall needs.

**Search never reads it.** AAAK lost recall because the shortened text
was what got stored and searched. Here the kept text stays verbatim and
is what recall scores. Horadrim is only how a hit is written when it is
handed to an agent, which is where the tokens are spent: a recall's
answer lands in the agent's context, a megabyte on disk costs nothing.
So recall cannot drop by construction, and what is measured is the
other half: tokens per hit, and whether an agent reading the Horadrim
form answers the yardstick's questions as well as one reading the
original (judged per question, not by feel).

**Saving where the repetition is, not in new words.** A tokenizer already
packs plain English tightly, and an invented abbreviation often costs
more tokens than the word it replaces; AAAK's ratio was counted in
characters. The savings are in what repeats:

- **Sigils.** A project's most repeated long names (file paths, crates,
  types, quest titles, Warriv, the dev instance's state folder) each get
  a sigil, a short word that the token counter confirms is one or two
  tokens. Which names earn one is counted from the memory itself: length
  in tokens times how often hits carry it.
- **The Lexicon.** The sigils and what they stand for, given to a
  session once, at the head of its first recall, where prompt caching
  makes it nearly free for the rest of the session. A Lexicon changes
  only by adding, so an old hit never means something new; a sigil whose
  name is gone from the project is retired, not reused.
- **Stripped, not paraphrased.** Greetings, restated questions, "Let me
  check" and tool chatter go. No sentence is reworded. Each hit keeps its
  chunk id, and `lore_recall` with that id returns the original: the
  scroll unrolled.

Expected at first: 2x to 3x fewer tokens per hit. If the yardstick shows
any loss in answers, the stripping goes first and sigils stay.

**Built 2026-10-16**, in `crates/horadric-memory/src/horadrim.rs` (pure,
tested). The keeper inscribes after the ranking, so recall's scores never
see the inscribed form. A sigil is `§` and a word of up to seven letters,
two of Claude's tokens (measured with `claude -p` usage; a crate's path
is fourteen). A name earns one when it is five or more tokens, occurs
three times or more in the project's memory and would save 24 tokens
there in all; the memory is counted at most once an hour, at most 96
sigils are in use. A file's sigil retires while its file is gone and
comes back with it; no sigil ever takes another name.

Decided while building it:

- **Each sigil is told once, at the head of the first answer that uses
  it**, not the whole Lexicon at the first recall. A sigil is used where
  the session was told it, or where the answer holds its name twice,
  which pays for telling it. The whole Lexicon up front cost more than a
  few recalls save.
- **The session's memory of sigils is `horadric mcp`'s**, one process per
  session: it sends what it was told (`sigil=name`, so a sigil could never
  be read with another name) and the app answers with what it told. A
  context compacted or cleared loses them while the process remembers, so
  an answer leaning on an old sigil carries `(§ forgotten? unroll
  "lexicon")`, and `lore_recall` with `unroll: "lexicon"` tells them all.
- **Only the agent's narration is stripped**: short "Let me check"
  sentences naming no file, "Okay." and the like, tool lines that named
  no file (`Agent: used Bash`), and the quest wrapper line. The human's
  words are never touched (their "Ok." may be the approval), nor code in
  fences.
- `lore_recall` with `unroll` and a hit's id (`#12`, `2026-10#5`) returns
  it word for word; it works with the app down too, reading the store.
- The Lexicon is `memory\horadrim.json`, never written over when it
  cannot be read (an empty one saved over it would rebind sigils). The
  off switch is the file `memory\horadrim.off`, its own so the Scroll
  never races the keeper.

**Measured.** The yardstick (`horadrim` lines and `yardstick/judge.py`):
recall's ten hits per question, answered by claude-haiku-4-5 from each
form and judged against the answer chunks by Sonnet.

| | answered right | tokens a hit (Claude, median) |
|---|---|---|
| verbatim | 52 of 65 | 236 |
| inscribed, a session's first answer | 59 of 65 | 236 |
| inscribed, five answers a session | 54 of 65 | 244 |

No loss in answers (the differences either way are the answering
model's own spread), and no savings either. On a real store of part 4's
dialogue (`--example tongue`, the installed Horadric's memory, nine
projects) the tongue saves 1.05x to 1.29x, 1.11x on Horadric's own: about
a tenth, stripping most of it.

**Where the 2x to 3x went missing.** Recall's hits are prose. A passage
of 250 tokens carries few long names, and seldom the same one twice, so a
sigil rarely pays for being told; on the yardstick most answers use none.
The chatter this part counted on is mostly gone already: part 4
keeps no tool output, and the agent's narration is short. What is left
to strip is the tool lines and the odd "Let me check". Claude's tokenizer
packs English tightly, as this part guessed, so no shortening that keeps
every word can save much. The tongue stays on, since it costs nothing in
answers; the gap is not chased here.

**The theme, a little.** The Horadrim were the order that kept Tal
Rasha's secrets in scrolls, so: a hit is inscribed, the original is
unrolled, a long name is bound to a sigil. The Lexicon opens with one
line in Deckard Cain's voice and is plain after it; flavour that costs
tokens on every call is not flavour, it is spend. Runes, the Cube,
Transmute and Identify already mean other things in Horadric, so the
tongue does not borrow them.

**The Scroll.** A section in the Settings window, drawn as an unrolled
scroll, where a person reads about the tongue: what it is and why it
keeps the original, the current project's Lexicon (each sigil, its name,
how often it was used), and the numbers (tokens saved this week, the
yardstick's answer score in Horadrim against verbatim). It is read only;
a switch to turn the tongue off sits at its foot, for when an answer
seems wrong and the human wants to see if the tongue is why.

### 9. Recall without being asked

Asked for on 2026-10-16. An agent has to remember to call `lore_recall`,
and the cheapest recall is the one Horadric makes for it. Hooks already
put text before the agent (`overlap.rs` answers PostToolUse with
`additionalContext`), so the same pipe carries memory:

- **On every prompt.** The UserPromptSubmit hook recalls on what the
  human typed and adds the best hits only when they score above a bar
  set from the yardstick, so most prompts get nothing. A hit already
  shown to the session is not shown again. Inscribed in Horadrim like
  any answer.
- **At quest start.** A recall on the quest's title and notes goes into
  its prompt beside the context pack. Done 2026-10-16: `After:` and
  `Model:` lines are left out of what is asked, the app's memory gets
  half a second to embed and the window waits a second and a half for
  the answer, and only hits at `recall::STRONG` come. That bar is on
  the fused score before it is weighed, so an old note or dialogue is
  not shut out by its age: about the first three places of words and
  meaning at once. The yardstick prints the bar's trade (MiniLM: 40 of
  65 questions get a hit, 1.5 each, 63% of them an answer; at 0.028
  it is 4.5 hits and 31%). Without the model nothing is strong, so an
  app that has not loaded it yet adds nothing. Since then the recall is
  asked for ahead on a thread of its own (when the brief opens, or the
  runner first sees the item) and taken when ready, so no window waits
  on it, and the model is loaded at the app's start when its files are
  already on disk, so the first quest has it.
- **Before a search.** When an agent greps or globs for a name, what the
  lore knows of it (where it is defined, a note naming it) comes with
  the result, so the next grep is not needed. Done 2026-10-16: the
  listener answers PreToolUse for Grep and Glob (`additionalContext`,
  waits 1.5 s at most) when the pattern is a name, asking the app's
  memory for the strong hits only, once per name per session, and adds
  nothing when the lore knows nothing.

Judged by `horadric quest spend`: tokens and calls per quest with these
against without, and how often an injected hit's files or ids show up in
what the agent did next.

**On every prompt, built 2026-10-16.** The listener answers a Claude
Code `UserPromptSubmit` of a session Horadric started by asking the
app's memory, after handing the event on so the tile turns at once, and
waits at most 3 seconds (the hook has 5; the memory spends at most one
of them embedding notes it has not seen). What it gets back goes before
the agent as `additionalContext`, headed by a line saying it is
Horadric's memory, recalled unasked. A slash command is not recalled on,
nor more than the first 2,000 characters of a prompt. An event the
installed Horadric passes to a dev instance waits for the dev one's
answer. Nothing changed in the installed hooks: `UserPromptSubmit` was
one of them already, and `reload` writes them again anyway.

The bar is two: the fused floor the quest's recall uses (`STRONG`,
words and meaning agreeing on a hit) and the cosine of hit and prompt,
0.45 or more; so without the model nothing is offered. Recall ranks as
for `lore_recall` with the floor, then keeps at most 3 hits at that
cosine that the session was not shown (the
app keeps the shown ids and told sigils by the agent's session id while
it runs, so a `/clear` starts afresh), and never the session's own
dialogue, which it was just told. Measured on the
yardstick (`unasked` lines, MiniLM, at most 3 hits a prompt) with 30
prompts it has nothing for, `chatter.jsonl`:

| bar | answer offered | offered hits that answer | chatter offered anything |
|---|---|---|---|
| cosine 0.45 | 67.7 % | 41.3 % | 6.7 % |
| cosine 0.50 | 53.8 % | 43.7 % | 3.3 % |
| cosine 0.55 | 35.4 % | 50.0 % | 0 % |
| floor alone | 49.2 % | 63.3 % | 16.7 % |
| floor and cosine 0.40 | 44.6 % | 62.5 % | 3.3 % |
| **floor and cosine 0.45** | **44.6 %** | **66.0 %** | **3.3 %** |
| floor and cosine 0.50 | 38.5 % | 69.8 % | 0 % |

Most of what a human types is chatter ("ok", "commit it"), so the bar
is set where one such prompt in thirty gets anything, and two in three
hits offered answer the question. The floor alone, right for a quest's
title, lets through one chatter prompt in six; the cosine alone offers
more but fewer that answer. The middle column is a floor too: a hit not
on a question's list may still help.

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
state folder's `models\`, fetched by hand for now.

Measured on this machine (i7-11700K, CPU shared with other sessions),
chunks of 759 characters on average, recall at 5:

| | alone | fused with words | ms a chunk | memory |
|---|---|---|---|---|
| words (the lore today) | 61.5 % | | | |
| BM25 | 67.7 % | | | |
| all-MiniLM-L6-v2, 384 | 73.8 % | **87.7 %** | 82 | 92 MB |
| bge-small-en-v1.5, 384 | 76.9 % | 84.6 % | 159 | 134 MB |
| EmbeddingGemma 300M, 768 | 80.0 % | 84.6 % | 234 | 858 MB |
| EmbeddingGemma, cut to 256 | 70.8 % | 83.1 % | | |
| EmbeddingGemma, cut to 128 | 72.3 % | 81.5 % | | |

"Fused with words" is reciprocal rank fusion with the lore's word
matching; fused with BM25 instead it is about the same (MiniLM 87.7 %,
Gemma 84.6 %). Memory is the working set the loaded model adds. Gemma's
peak while loading is 2.8 GB, with its token table already kept in half
precision (a third of a billion numbers; full precision adds 400 MB).

**Chosen: all-MiniLM-L6-v2** (decided 2026-10-16). Fused, which is how
recall will run, it finds the most, at a tenth of Gemma's memory and a
third of its time a chunk: a year of dialogue (90k chunks) embeds in
about two hours of one core's spare time, against six. Gemma alone is
better on dialogue (81.8 % against 68.2 %), and fused that gap shrinks
to 77.3 % against 81.8 %, two or three questions out of 65, which is
within what a set this size can tell apart. It is English only; the
human writes to the agents in English. Gemma stays runnable in the crate
and the yardstick, so a larger set or a need for other languages can
reopen this with numbers, and since vectors are a cache tagged by model,
changing it later costs one background re-embed.

Weights: sentence-transformers/all-MiniLM-L6-v2 at commit
`1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, SHA-256 of `model.safetensors`
`53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db`,
`tokenizer.json` `be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037`,
`config.json` `953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41`
(Apache 2.0). Google's own EmbeddingGemma repository is gated behind a
licence click; the measurement used the ungated `unsloth` copy, whose
output matches Google's model card to three decimals. Fetching the
weights into `models\` and checking these hashes is a quest of its own,
before part 3.

### 2. The store

Per state folder, `memory\`: append only segment files, one per month,
each holding the verbatim text of its chunks and their metadata
(project, kind, time, session, quest, commit, files and their hashes),
with the vectors beside it in a file of their own per model. Old months
never change, so they are cheap to back up and safe to read while a new
one is written. The note store's lock file pattern guards writers. A
format version in each file header, and a test that reads files written
by every earlier version.

### 3. Recall by meaning

`lore_recall` scores each hit by words (BM25, written here, it is small)
and by meaning, fuses the two ranks (reciprocal rank fusion), then
weighs: the asking project above others, notes and quest summaries above
dialogue, recent above old, maybe stale below fresh. Measured against the
yardstick before and after.

### 4. Transcripts, verbatim

The app reads each session's transcript as it grows (it is told by the
Stop, PreCompact and SessionEnd hooks) and keeps the dialogue: the
human's prompts and the agent's text, with each tool call reduced to its
name and the files it named. Codex rollouts the same way. A backfill
reads what is on disk now. Anything that looks like a secret (see
[secrets.md](secrets.md)) is left out before it is written, since this
lives for years.

### 5. Code by meaning

Each symbol in the lore's code index (its head and doc comment, not its
whole body) gets a vector, refreshed when its file's hash changes, so
"where do we roll back a failed reload" finds the function.

### 6. Across projects

Every project's memory is searchable from every other, behind a switch
in Settings, off by default. Hits from another project say which.

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

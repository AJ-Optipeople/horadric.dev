# The yardstick

A fixed retrieval test for Horadric's memory. It holds a corpus of chunks
and questions that each name the chunks that answer them. Every way of
finding things (words only, each embedding model, both fused) is scored
on it by recall at 5: the share of questions with at least one answer
chunk in the top five hits.

See "1. The model" in [docs/plan/memory.md](../../../docs/plan/memory.md)
for why it exists.

## Files

- `corpus.jsonl`: one chunk per line,
  `{"id": "...", "kind": "doc" | "note" | "dialogue", "text": "..."}`.
- `questions.jsonl`: one question per line,
  `{"q": "...", "answers": ["<chunk id>", ...], "from": "doc" | "note" | "dialogue"}`.
  `from` is the kind of chunk the question was written from. `answers`
  lists every chunk that answers it, of any kind.
- `build.py`: makes `corpus.jsonl`. Run it with `python -I build.py`.

## Where the corpus came from

Built on 2026-10-16.

- **doc**: every markdown file tracked in the repository, split on
  paragraphs into chunks of 400 to 1,000 characters. A heading starts a
  new chunk when the one before is long enough. Ids look like
  `doc:docs/plan/lore.md#4`.
- **note**: the lore notes of this project, one note per chunk, with a
  `Files:` line when the note names files. Ids look like
  `note:horadric.dev#12`, the number being the note's own id.
- **dialogue**: Claude Code transcripts of this project, September and
  October 2026 (older ones had been cleaned up by Claude Code). Only the
  human's typed prompts and the agent's text are kept, as `Human: ` and
  `Agent: ` lines. Tool calls, tool output, system reminders and command
  wrappers are dropped. Consecutive messages of one session are packed
  into chunks. Two in three sessions are kept, and one in four of
  Warriv's wake rounds, in time order. Ids look like
  `dialogue:8a8cb053#2`, the first part being the session's id.

A chunk is left out whole when it looks like a secret (keys, tokens,
bearer headers, long hex or base64), holds an email address, a phone
number or an IP address, or touches something personal and not about
building Horadric. When unsure, the chunk was dropped. The ids of the
chunks around it do not move.

## How the questions were written

By hand, by reading the corpus. Each is something a later session on
Horadric would ask: why was this decided, where does that happen, what
broke and why, how do I test it. Most are worded the way someone who does
not know the answer would ask, in other words than the chunk. Some share
almost no words with their answer, to test meaning. Some share many, to
keep the test fair to word matching. For each question the corpus was
searched for every chunk saying the same fact, such as a plan part and a
note, and all of them are listed.

## The rule

The set is only ever added to. A question or an answer is never edited
or removed to make a model score better. A wrong answer list may be
fixed, with the reason in the commit message.

To grow the corpus, run `python -I build.py --append`. It keeps every
existing line and adds only chunks whose ids are new. A plain rebuild
reads the transcripts as they are now, so a session that was still
running can get different chunks, and a transcript Claude Code has since
deleted is lost.

## Running it

`cargo run --release -p horadric-memory --example yardstick [gemma|bge|minilm]`
scores words only, BM25, each model alone and each fused with words.
The models are read from `%APPDATA%\Horadric-dev\models\<folder>`.
Vectors are cached in `target\yardstick` under a hash of the corpus, so
a second run only scores. The first results and the model chosen from
them are in docs/plan/memory.md, part 1.

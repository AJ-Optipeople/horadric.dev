# Small local models

From the research pass of 2026-10-24. Two jobs want a model on the
machine that costs next to nothing: embedding everything the lore and
memory keep, and the "Stay a while and listen" sentence per session.
Today `horadric-memory` runs EmbeddingGemma, BGE small or MiniLM through
candle, which is a full transformer pass for every piece of text.

## Static embeddings for the first pass

Model2Vec distils a sentence model into a lookup table: a text's vector
is the mean of its tokens' rows, no forward pass. MinishLab keeps an
official Rust crate, `model2vec-rs` (https://docs.rs/crate/model2vec-rs),
and small models such as `potion-base-8M`.

The idea: embed everything with the static model, which is cheap enough
to index all the lore and every transcript, and rerank the top hits with
the candle model already there. That cuts first run download and CPU.

To try before deciding:

- Run the yardstick (`crates/horadric-memory/examples/yardstick.rs`)
  with a static model alone and as the first pass before the current
  one. Recall must not drop noticeably.
- Time both on this machine. The speedup is expected, not measured.
- It is a new dependency. Check what `model2vec-rs` pulls in; it may be
  small enough to write the lookup ourselves on top of `tokenizers`,
  which is already here.

## A small model for the session sentence

The loose end "a sentence per session on what it did" needs a small
language model. Options found:

- **Candle, already a dependency.** A Qwen or Gemma of about 0.5B runs
  on `candle-transformers`. One Linux report gives about 35 tokens a
  second on CPU for 0.6B, plenty for one sentence. Unmeasured on
  Windows. No new dependency, so try this first.
- **Foundry Local** (GA 2026-04-09,
  https://devblogs.microsoft.com/foundry/foundry-local-ga/). Microsoft's
  local runtime with a Rust SDK, runs on the NPU or GPU where there is
  one. It ships ONNX Runtime binaries, which breaks "no DLL to ship", and
  its crate failed to build on docs.rs at 1.2.3. Only if candle is too
  slow.
- **Phi Silica** (Windows' built in summarizer). Needs a Copilot+ PC, a
  Limited Access token and a packaged app. Out unless Horadric is ever
  packaged.

Decide which before building: the model, when it runs (on `Stop`, or
only for a session that ended), and where the sentence shows.

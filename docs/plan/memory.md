# Memory

The long term memory (`crates/horadric-memory`, the app's memory thread
in `horadric-ui/src/memory.rs`) is built. What was decided, measured and
found while building it is in the lore; ask `lore_recall`. This page
holds only what is still not done.

## Not done

- **The bit vector prefilter.** Search scans every int8 vector. Once the
  store is big (hundreds of thousands of chunks), scan 96 byte bit
  vectors by Hamming distance first and score only the best few thousand
  in int8.
- **A gentler pace for background embedding.** It embeds two chunks at a
  time with a 100 ms rest, using every core while it runs. Fine for
  notes; the backlog of transcript chunks waiting for vectors wants a
  pace that leaves the sessions their CPU.
- **Redaction does not know the vault's own secrets.** `redact` catches
  token shapes and secret-sounding names, but not the values kept in the
  vault (see [secrets.md](secrets.md)). It should cut those too.
- **Compaction of old code chunks.** A code chunk of a file that changed
  or went is no longer an answer, but it stays in the store and is still
  embedded. A compaction should drop them.
- **An over-long day loses its older passages for good.** The look back
  hands Warriv the newest 160,000 characters of a day, and the next look
  back starts after this one's mark, so the rest is never read.
- **The yardstick has no validity questions.** Nothing measures recall
  of replaced notes, `history`, or "what is true now" against "what was
  said then".
- **Judging recall without being asked.** The plan was to compare quests
  with and without the injected memory in `horadric quest spend`, and to
  count how often an injected hit's files or ids show up in what the
  agent did next. Neither is built.
- **A deleted worktree's dialogue keeps its own key.** A transcript's
  project is the one git names for its folder; a worktree since deleted
  falls back to its own folder, so its dialogue is not found under the
  main project.
- **The model choice may reopen.** EmbeddingGemma beat MiniLM on dialogue
  alone (81.8 % against 72.7 %), the part that grows most. A larger
  yardstick with more dialogue questions should decide whether to switch
  (one background re-embed).

# Lore

The lore (spend measuring, context packs, shared notes, the code index,
the shorter quest log, the window measures, `horadric heavy` and the
bench) is built. What was decided and measured is in the lore itself;
ask `lore_recall`. This page holds only what is still not done.

## Not done

- **The read guard is built but off.** `horadric guard` turns down whole
  reads of long files with the outline, and rewrites plain builds to run
  through `heavy`. In the bench it never fired. Turn it on only if
  `quest spend` shows the lore's wording (outline first, read ranges,
  send many-file searches to a subagent) did not move Read's share of
  results (71 %) or the Agent share (median 0 %) after about thirty
  quests. That check has not been made.
- **Compaction at the right moment.** A quest that ends at a 180k window
  could go on from a 40k summary after its exploration. Whether Horadric
  can ask Claude Code to compact then (after the pack is read, before the
  edits) is an open question of Claude Code's interface.
- **The `docs/bench/where.json` bench is not run.** It runs the edit and
  a question under Sonnet and Opus, each with and without the user's
  plugins, one run at a time, to say how much of the tenfold gap between
  a bench run and a real quest is the model and how much the start. The
  machine was short of memory when the first bench was stopped, so it
  waits for the human to start it:
  `target\debug\horadric.exe bench docs\bench\where.json`.
- **Tuning the context pack.** Run `horadric quest spend` again once more
  than 30 quests have run with a pack, before changing the pack's size or
  weights.

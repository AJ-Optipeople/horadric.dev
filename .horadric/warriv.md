# Warriv's memory

## Rules
- Do not wait for main's checks before casting a ship stone: nothing runs them between landings. Ship Public runs all three first (RELEASING.md step 2) and Ship Local does too since 5170f91; both stop on red (lore #30).
- Keep a chain of dependent steps in one quest; split into quests only where parts can run side by side. The human finds handovers slow (lore #34).
- A quest session that says it waits on its own background work (judge, review agent, checks) is not stuck: no tell, no hand on (lore #31).
- A quest session silent for hours with an empty worktree after two nudges is dead and I can not restart it: hand it to the human with "restart it", who does so (settled on "OSC 7501: plain terminals", which then landed).

## Lately
- 2026-10-16 Round (13:15, main at fbc3e9f): "Recall off the window thread" landed. The 13:06 Ship Local has not reloaded yet (last reload 13:04) and a session started 13:08 is busy, its checks queued behind the quest pack's clippy, so it carries fbc3e9f. No second cast. Three quests under way.
- 2026-10-16 Round (main at 1227150): "Backup: encrypted snapshots to a folder, and restore" landed, tree clean apart from Horadric's own files, so cast "Ship Local". "Recall off the window thread" committed 39f180f on its branch, tests running: not stuck, noted, no tell. Three Backup and recall quests still under way.
- 2026-10-16 "Backup: encrypted snapshots to a folder, and restore" stopped a sixth time, checks queued behind another clippy before its fast-forward. Not stuck (lore #31), noted, no tell.
- 2026-10-16 Round (human left, main at 8485c6d): the Ship Local reloaded fine at 13:04 with e81cbca. Four quests under way, Backup Settings waits on part 1. Nothing to do.
- 2026-10-16 "Memory: horadric quest pack shows the quest's recall too" stopped again, review fixes made, checks queued behind a ship build. Not stuck (lore #31), noted, no tell.
- 2026-10-16 "Memory: horadric quest pack shows the quest's recall too" stopped, checks queued behind a ship build and a review agent reading. Not stuck (lore #31), noted, no tell.
- 2026-10-16 Round (12:58, main at e81cbca): Ship Local runs the three checks first (5170f91) and the "not nudged" quest (e81cbca) landed. The 12:45 Ship Local has not reloaded (release exe 03:54) and a `cargo test --workspace` started 12:58, after both landings, so it carries them. No second cast. Backup part 1, recall on every prompt, quest pack recall and recall off the window thread under way; Backup Settings waits on part 1.
- 2026-10-16 Wake right after (12:52, main still at 43b29d6): log unchanged, the 12:45 Ship Local still building. Nothing to do.
- 2026-10-16 Round (main at 43b29d6): "Memory: what the lore knows comes with a Grep or Glob" landed (9318873, 12:49). The 12:45 "Ship Local" cast has not reloaded yet and cargo started 12:51, after the landing, so it carries it. No second cast. Gave "a quest's recall off the window thread" Model: sonnet. Public still waits for the rest of Backup and recall. Last reload attempt in reload.log (04:03) failed, old Horadric did not exit: check the next ship reloads.
- 2026-10-16 Round (main at 31f5505): "Memory: a recall in each quest's prompt" landed (c7eec09, 05a75c9), so cast "Ship Local" to get it to the human now; public still waits for all six Backup and recall quests. Backup part 1 stopped a fifth time, re-merging main and rerunning checks before its fast-forward: not stuck, noted, no tell.
- 2026-10-16 "Backup: encrypted snapshots to a folder, and restore" stopped a fourth time, checks running after merging main, then fast-forward. Not stuck (lore #31), noted it, no tell.
- 2026-10-16 "Memory: recall on every prompt, from the UserPromptSubmit hook" stopped with work committed on its branch, waiting on its review agent before merging. Not stuck (lore #31), noted it, no tell.
- 2026-10-16 "Backup: encrypted snapshots to a folder, and restore" stopped a third time, full checks running in its background before commit and merge. Not stuck (lore #31), noted it, no tell.
- 2026-10-16 "Backup: encrypted snapshots to a folder, and restore" stopped again, checks green, waiting on its review agent before committing. Not stuck (lore #31), noted it, no tell.
- 2026-10-16 "Backup: encrypted snapshots to a folder, and restore" stopped while its tests and review agent run. Not stuck (lore #31), noted it, no tell.
- 2026-10-16 "Backup: Google Drive with one click sign in" stopped while its review agent checks the diff. Not stuck (lore #31), noted it, no tell.
- 2026-10-16 Look back (29 done, 26 merged, 51 Warriv sessions): the day gave 5 notes (#30 to #34). The 14 unreported stops were sessions waiting on their own background work, so filed "Quest sessions waiting on their own background work are not nudged" (sonnet). The 2 conflicts were both crates/horadric/src/task.rs on the spend quest's fix-ups, already landed; warned the Grep/Glob quest it shares hooks with "recall on every prompt". Found ship local sessions skip the checks, filed "Ship Local runs the three checks first and stops on red" (haiku) and fixed my rule.
- 2026-10-16 Round (main at 06e257c): 6 Backup and recall quests filed by the human's session, 4 under way. Gave Model: sonnet to "Backup: the Settings section" and "Grep or Glob". Nothing landed, nothing to ship.
- 2026-10-16 Round after the human left (main at 6791d8a): Ship Public landed as v0.28.0, tagged and reloaded fine. 91 done, none open, no aim. Nothing to do.
- 2026-10-16 Round (main at daf3727): "Memory: the Scroll" landed (32fe982), the last Memory quest, tree clean apart from Horadric's own files, so cast "Ship Public" for all of Memory (last tag v0.27.0). 91 done, none open, no aim.
- 2026-10-16 Round (main at d9b8dd7): "Memory: Horadrim, sigils and the Lexicon" landed (63eb633), tree clean apart from Horadric's own files, so cast "Ship Local". Held public one more quest: the Scroll (its Settings section, under way) finishes Memory, then one release for it all.
- 2026-10-16 Round (main at 229fa02): "Memory: the look back misses no dialogue" landed (60c4e32), tree clean apart from Horadric's own files, so cast "Ship Local". Horadrim still under way, the Scroll waits (sonnet). Public still held for Horadrim.
- 2026-10-16 Wake: "Memory: Horadrim, sigils and the Lexicon" stopped a fourth time, judge.py still running in its background before part 8 numbers and merge. Not stuck, no tell, noted.
- 2026-10-16 Round (human left, main at 9ce75ba): nothing landed since the 0752547 ship; Horadrim and the look back work in their worktrees, the Scroll waits (sonnet). Nothing to do.
- 2026-10-16 Round (main at 0752547): "Memory: Warriv distills the day" landed, tree clean apart from Horadric's own files (Horadrim and the look back work in worktrees), so cast "Ship Local". Gave the look back quest "Model: sonnet". Public still held for Horadrim.
- 2026-10-16 Wake: "Memory: Horadrim, sigils and the Lexicon" stopped a third time, judge.py running in its background before part 8 numbers and merge. Not stuck, no tell, noted.
- 2026-10-16 Earlier: many wakes for Memory and Backup quests waiting on their own checks, review agents or benchmarks (not stuck, no tell). Ship Local cast after lore_recall by meaning, transcripts and code by meaning, across projects, Warriv distills, the look back; Ship Public cast for all of Memory and landed as v0.28.0. Gave models to the Memory and Backup quests. Look back filed the "not nudged" and "Ship Local runs the checks" quests.
- 2026-10-16 Early: embedding model, store and model fetch landed as crates the app did not use yet, so no ship until lore_recall by meaning. Ship Public cast for the Arcane Sanctuary after the sky quest landed (v0.27.0); held earlier ships while sky files sat uncommitted in the shared tree.
- 2026-10-15 Lore and Tristram landed through the evening with a Ship Local after each step and one Ship Public for both (v0.26.0). Hand-marked spend needed two merge fix-ups in task.rs (sonnet). Several rounds held a second cast while a ship already building would carry the landing.
- 2026-10-13 Only docs and memory landed after 0.24.0, no ship. The human settled cloud tiles: wait for `claude agents --json`. The plain terminals answer became a rule.
- 2026-10-12 Town Portal's second round landed quest by quest, a Ship Local after each (each reloaded fine); iOS keyboard landed in horadric.portal. Cloud tiles waited on the human.
- 2026-10-11 Claude Code 2.1.287 to 2.1.296 series: held ships while quests were half done in the shared tree, shipped once it was clean. "OSC 7501: plain terminals" went silent and was handed to the human to restart. Many idle hourly rounds before that.
- 2026-10-06 to 2026-10-10 Mostly idle rounds (no quests, no aim). Settings window series landed 10-07 and shipped public as v0.16.0; Mac quests shipped as v0.17.0; v0.18.0 and v0.19.0 were cut by hand.

## Open
- Once quests run with a pack, the pack comparison resumes by itself.
- The human's session said it will ship public when all six Backup and recall quests land ("released asap"). If it has not when they land, cast "Ship Local" and leave public to the human.

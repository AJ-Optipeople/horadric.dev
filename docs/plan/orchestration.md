# Orchestration

What is still open of orchestration. Dependencies, Warriv, auto merge,
parallel quests and prompt bypass in worktrees are built; their
decisions and traps are in the lore.

- **A Warriv restored after a restart.** A Warriv session that was
  awake when Horadric stopped comes back paused like any other session,
  and the next event starts a fresh Warriv beside it, so a project can
  show two. A reload is fine, since session hosts outlive it; this is a
  real restart (Quit, crash, reboot). Either close the paused one when
  a new wake starts, or do not restore Warriv sessions at all, since a
  wake is never carried on.
- **Which model Warriv runs on.** Its turns are short and many, which
  says a small one, but a wrong call costs more than the turn saves.
  Most of the traps found so far (commands behind `cd` or `&`, the
  PowerShell tool, relative paths, reading a brief as leave to build)
  were Haiku's. Not decided.

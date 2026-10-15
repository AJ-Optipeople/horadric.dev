# Step 4: worktrees and the git glance

- `git worktree add` per session, branch named from the session name.
- `.horadric/config.json` per repo with `setup` commands, copied into each new
  worktree because gitignored files do not come along. Superset learned this
  the hard way; their worktrees break without it. Allocate a port range per
  worktree too, so dev servers do not collide.
- `git diff --numstat` for the changed file list with plus and minus counts,
  uncommitted versus committed.
- A button that opens the worktree in VS Code (`code <path>`).
- **The project key has to change.** It is the working directory today, so
  every worktree would become its own cluster. It must resolve to the parent
  repository, via `git rev-parse --git-common-dir`.
- Worktrees must be optional per project. A session that wants the shared
  working tree, or is not in a repo at all, has to keep working.

Built so far: the project key resolves to the main working tree, a project
works in trunk mode or with a branch per session, a session in a worktree
has a tile that shows what it changed, and sessions in one tree are told
when they edit the same file.

- **Where.** A new session started from a repository's main working tree
  (the plus, the project menu, the start window, `horadric new`) runs `git
  worktree add -b <branch>` from what the main tree has checked out. The
  branch is the session's name made into a slug, `session` when it has
  none, with 2, 3 and on after it when taken. The folder goes beside the
  main tree, `app.fix-login` for `app`, so the main tree's watchers never
  see it. A session started in a subfolder starts in the same subfolder of
  the worktree. Carrying on a past conversation (`--resume`, `--continue`)
  keeps the folder it was held in, since Claude Code finds it by that. A
  folder already in a linked worktree, outside a repository, or a git that
  refuses, keeps the shared tree.
- **Config.** `.horadric/config.json`, read from the main tree since it may
  be kept out of git: `"worktrees": {"enabled": true, "setup": [...],
  "ports": 10}`. Without `enabled` the project is in trunk mode, see work
  modes below, and `setup` and `ports` apply to the worktrees it asks for.
- **Work modes.** A worktree for every session was the default, and it
  got in the way. The human mostly works on one branch, committing and
  pushing often, and says "build", "show on dev" or "ship" expecting all
  of today's work. With a worktree per session that work sat on branches
  not merged yet, or not committed, and it was hard to tell which. So a
  project now works in one of two modes, picked under Work mode in the
  project menu. **Trunk**, the default: every session starts in the main
  tree, and its system prompt says the tree is shared, to commit small and
  often on the branch that is checked out, to stage only its own files by
  name and never `git add -A`, to push when there is an upstream, and not
  to make branches unasked (`worktree::trunk_prompt`). A worktree is had by
  asking: "New session in its own worktree" in the project menu, tombs,
  and task items run side by side, which get one in either mode since
  they would edit the same files. **Branch per session**: every new
  session gets a worktree, as below. Switching writes `enabled` into the
  config and changes only the sessions started after.
- **Overlaps.** In a shared tree two agents can overwrite each other. The
  listener claims each file a session edits (`Edit`, `Write`,
  `MultiEdit`, `NotebookEdit`) for it until it commits or ends, or four
  hours pass (`overlap::Claims`, pure and tested). An edit to a file
  another session claims is answered, in the reply to that `PostToolUse`
  hook, with `additionalContext` telling the agent another session
  changed the file and has not committed, to look at `git diff` before
  changing it further, keep what is theirs and stage only its own
  changes. Once per file and set of others, so an agent editing a file
  ten times hears it once. The app shows a toast with both sessions'
  names, and a click shows the one that edited last. Paths are compared
  whole, so sessions in worktrees of their own never overlap. The
  installed Horadric hears a dev instance's hooks first and answers them,
  and toasts only for sessions it holds.
- **Setup.** The commands run in the session's own pane before the agent,
  through `horadric setup <program> <args>`, which runs each with `cmd /d /s
  /c` and then starts the agent with its arguments exactly as given (a
  batch file in between would have had to quote them for cmd). A command
  that fails says so and the agent starts anyway, since it can read the
  error and put it right. Only on the first start: a resume goes straight
  to the agent.
- **Ports.** Each worktree gets the lowest free range of `ports` ports on a
  step from 4100, above the defaults dev servers pick (3000, 5173, 8080),
  which the main tree keeps. The session gets `PORT` and
  `HORADRIC_PORT_FIRST` and `HORADRIC_PORT_LAST`, and is told the range.
  The worktree and its range are saved with the session, so a resume gets
  the same ones.
- **Telling the agent.** Its system prompt says where its worktree is, its
  branch, where the main tree is and not to edit there, to commit on its
  branch, and its ports. Joined with the task list's and the hosts' prompt.
- **Ending a session** removes its worktree with plain `git worktree remove`
  and then `git branch -d`. Git refuses the first while the tree has
  changed or untracked files and the second while the branch has commits
  the main tree lacks, so nothing is lost: what refuses stays for the human.
  Quitting pauses and removes nothing.
- **Sweeping merged worktrees.** Ending a session cannot clean up a
  worktree whose branch was merged later, nor one an agent added by hand,
  and those piled up: eight of them beside this repository after one
  evening of parallel branches. So the app sweeps each project with a
  session at startup and after any of its sessions is heard from, at most
  every 10 s a project, and only once the main tree's HEAD has moved, since
  nothing is merged while it stays put. A linked worktree goes, with `git
  worktree remove` and then `git branch -d`, when its branch is merged into
  what the main tree has checked out, `git status` in it says nothing, no
  session (running or paused, or about to start) has its folder there, and
  the branch's reflog shows at least one commit on it. The last keeps a
  worktree just added and not yet worked in, which counts as merged. A
  locked or detached worktree is left alone. Gitignored files in a swept
  worktree go with it. Tested with a dev instance on a scratch repository
  of five worktrees: the merged one went with its branch, the busy, dirty,
  fresh and unmerged ones stayed, and merging the unmerged one got it swept
  at the next hook. That dev instance still held sessions in this
  repository, so its first sweep also removed the eight leftovers here,
  and kept the four worktrees in use.
  A dev instance knows only its own sessions, so to it a worktree another
  instance's agent works in looks free, and a test session in a worktree
  of this repository once swept other agents' merged, clean worktrees
  from under them. So a dev instance leaves a `horadric-dev` file in the
  git folder of each worktree it adds (git keeps that folder per worktree
  and removes it with the tree) and sweeps only worktrees that carry it.
- **The task runner** holds one item at a time in the shared tree, unless
  `"tasks": {"parallel": 3}` in the config lets it hold several. Then each
  item it takes (or a click takes) gets a worktree of its own, its branch
  named after the item, and the runner fills a free place with the next
  open item, one start every 10 seconds. A blocked item or one whose
  session is gone still stops the list, and a paused one holds it until a
  click resumes it. With worktrees off, or outside a repository, `parallel`
  counts as 1, since items side by side in one tree edit the same files.
  The list lives only in the main working tree: the app reads it there,
  the agent is told its path and that it is the one file in the main tree
  it may change, and `horadric task` finds it through `HORADRIC_TASKS`,
  which a session in a worktree gets, since the worktree has no list or an
  old copy. A finished item's session ends as always; its branch stays
  until the human merges it, so an item no longer builds on the one before
  unless that one was merged first. Tested with a dev instance and
  `HORADRIC_AGENT=cmd.exe`: with `parallel` 2 in auto mode, item one
  started in `par.item-one`, item two 10 s later in `par.item-two`, and
  item three waited. `task done` run from inside `par.item-one` with its
  session's environment marked the main tree's list, the session closed,
  its clean worktree went away and item three started in its own. Three
  `cmd.exe` the whole time (two items and one plain session). Not checked
  on screen: that `HORADRIC_TASKS` reaches the agent's shell, which goes
  the same way as the ports.
- **Choosing how many at once.** The mode menu lists "One at a time" and
  2 to 4 at once below the modes, the current one checked, and writes
  `parallel` into the config beside the mode (one takes the key out). A
  project without a worktree per session shows one greyed line saying
  why instead. Where the runner runs, the mode key says the number too,
  "Review ×3". Asked for because the setting lived only in the config
  and the user never found it. Tested on screen with a dev instance and
  `HORADRIC_AGENT=cmd.exe`: review mode with `parallel` 3 started three
  items 10 s apart, each in its own worktree, and the key read "Review
  ×3". The menu itself was not clicked, since a synthetic click opens it
  behind other windows.
- **Merging a finished item.** When a session ends and `git branch -d`
  refuses its branch, the app hears of it, and if the branch is a done
  item's (its title's slug, or that with 2, 3 and on after it:
  `worktree::finished`, pure and tested) a notification says "Finished:
  <item>, click to merge <branch> into main". The click asks yes or no,
  and yes runs `git merge --no-ff --no-edit` in the main tree, then `git
  branch -d`. A merge that stops (a conflict, or local changes in the way)
  is undone with `git merge --abort`, so main is never left half merged,
  and the notification says why and that the branch stays. The project
  menu lists "Merge <branch> into main" for every done item's branch not
  merged yet, from `git branch --no-merged HEAD`, so a missed notification
  or a restart loses nothing. Tested on screen with a dev instance and
  `HORADRIC_AGENT=cmd.exe`: an item with a commit on its branch got the
  notification, the click asked, yes gave a merge commit on main and the
  branch went; a second item whose change conflicted with one on main left
  main clean and kept its branch. The project menu's entries were not
  clicked, the installed Horadric's column stood over the dev one. The
  lists are now read from the project's folder rather than a session's:
  a project whose sessions were all in worktrees showed an empty list,
  since a worktree has none.
- **What it changed.** A tile whose session has a worktree shows the lines
  added and removed, `+9 −1` in the files tile's green and red, beside the
  last line, and a `</>` button that opens the worktree in VS Code. Its
  right click menu has a Changes submenu: each file with its counts, those
  not committed apart from those committed on the branch, and a click
  shows the file on the stage. Not committed is `git diff --numstat HEAD`
  plus untracked files, all their lines added; committed is `git diff
  --numstat <main tree's HEAD>...HEAD`, from where the branch left. The
  count (`diff.rs`, pure and tested) runs on a thread after the agent did
  something, at most every 3 s, and again as the menu opens. Nothing
  polls, so an edit made outside the agent shows at its next hook. Tested
  on screen with a dev instance: a worktree with a changed file, an
  untracked one and a commit showed `+9 −1`, the menu split them as git
  does, and a click showed the worktree's copy of the file. The VS Code
  button was not clicked, it uses the same launch as the project menu.

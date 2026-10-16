# The files tile

Every cluster whose project is in git ends in a files tile: the project's
tree as VS Code's explorer shows it, with the git changes coloured in.
Asked for so the changes in a project can be seen at a glance, without an
editor.

- **What it shows.** Every file git knows (tracked and untracked, ignored
  ones hidden) plus deleted ones, folders first, names sorted without
  regard to case. Names take VS Code's dark theme colours and letters: M
  modified, A added, U untracked, D deleted, R renamed, C conflict. A
  folder takes the most important change inside it and a dot. A chain of
  folders with one child each shares a row (`crates/horadric-ui/src`). The
  header counts changed files.
- **Folders.** One holding a change opens by itself, so a new change is in
  view without a click. A click opens or closes any folder and is
  remembered by path, so it survives the tree being rebuilt.
- **Files.** A click shows the file on the stage (see The file viewer).
  Ctrl+click opens it in VS Code instead, in the window with the project
  (`Code.exe <project> <file>`, found beside `bin\code.cmd` on `PATH`, so
  no console flashes). Without VS Code, whatever Windows opens it with.
- **Size.** Its column decides (see The columns): the files tiles in a
  column share the height the rest leaves, and a longer tree scrolls with
  the wheel, with a thin bar. Room left under the last row stays empty.
  The header folds it to one row, kept in `state.json` as
  `files_collapsed` per cluster. It used to be 14 rows, or as tall as its
  bottom edge was dragged, which made every cluster a different height
  and let a change in one move the rest. It sits below the `+`, which
  stays the slot where the next session tile goes.
- **Where the data comes from** (`watch.rs`). One thread per project runs
  `git ls-files` and `git status` and hands the tree (`files.rs`, pure and
  tested) to the cluster with a window message. Then it sleeps in
  `ReadDirectoryChangesW` until a file changes: nothing polls, so a quiet
  project costs no CPU. Changes in ignored folders are dropped, from `git
  ls-files --ignored --directory`, or every `cargo build` would rerun git;
  inside `.git` only `index` and `HEAD` count. A burst is waited out (300
  ms quiet, at least 1 s between scans, at most 3 s of waiting).
- **A git folder elsewhere.** A session in a subfolder of a repository,
  or in a worktree whose `.git` is a file, has its index outside the
  project folder. The thread then also watches the top of `git rev-parse
  --absolute-git-dir`, not its subfolders, and names what changes there as
  if it were in `.git`, so the same rule applies and commits are heard.
- `git --no-optional-locks` is required: a plain `git status` may rewrite
  the index, which the watcher would see, and scan again forever.
- A folder outside git gets no tile and no thread.

Tested on screen with a dev instance on this repo: 43 changes shown
coloured, a new file at the root appeared as U within a second and went
when deleted, five writes to `target\` caused no scan, folding, the wheel
and the header fold worked, and a click opened `main.rs` in VS Code. A scan
takes about 100 ms in a debug build, most of it starting git. A session
started in a subfolder of a scratch repository showed a new file, rescanned
on a commit made at the repository root, and did not rescan on `git
status`, `git log` or `git gc`.

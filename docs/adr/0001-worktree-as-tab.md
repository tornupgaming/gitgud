# A worktree is a tab

gitgud's architecture gives every tab one repository path (`RepoTab` owns a `PathBuf`; the worker owns one `git2::Repository` per `RepoId`), and both libgit2 and the git CLI open a linked worktree path as an ordinary repository. We decided that "standing in" a worktree means having its tab open: creating or jumping to a worktree opens-or-focuses a normal repo tab at that path, and the worktree bar is pure navigation. The rejected alternative — one tab per repository with an intra-tab worktree switcher — would have forced every `RepoTab` snapshot (staged/unstaged, history, selection, in-flight counts) and the worker's `RepoId → Repository` map to grow a worktree dimension, a large refactor for no behavioural gain; with worktree-as-tab, every existing flow (checkout, merge, sync, diff, conflict resolution) works inside a worktree unchanged.

## Consequences

- Worktree membership is discovered per refresh and stored as plain snapshot data on the tab; sibling tabs of the same repository each hold their own copy.
- Removing a worktree must also close its tab if one is open, since the tab's path stops existing.

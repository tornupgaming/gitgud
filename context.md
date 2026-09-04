
## Language

### Flow

A named, self-contained sequence of steps that carries out a user-initiated action from start to completion, including any validation, decisions/branching, dialogs, and side effects involved. A flow is defined once and is independent of its invocation point: the same flow executes identically regardless of which UI element, command, or shortcut triggered it. A flow owns the orchestration of an action (what happens, in what order, under what conditions) but not necessarily the low-level operations it delegates to.

#### Example

The Checkout Flow handles switching branches — checking for uncommitted changes, prompting the user to discard them (or cancel) if needed, performing the checkout, and reporting the result. It behaves the same whether invoked from the branch list, the command palette, or a keyboard shortcut.

### Modal

An overlaying element obscuring the main view.

#### Synonyms

Modal. Dialog. Window. 

### Branch-only commits

Commits reachable from HEAD but from no other branch tip — the work that exists only on the current branch. The current branch's own upstream does not count as "another branch": commits already pushed to it are still branch-only, since pushing doesn't change whose work it is. Computed by the worker per history load and rendered in the commit graph with a glowing node.

#### Example

After branching `feature` off `main` and making three commits, those three are branch-only. Once `feature` is merged back into `main` (or another branch is created at its tip), they stop being branch-only.

### Branch focus

The commit graph view mode that dims every commit that is not a branch-only commit, so the current branch's own work stands out against the shared history. Toggled from the COMMIT GRAPH header; inactive (a no-op) when there are no branch-only commits, since dimming the whole graph would read as a broken display.

### Stale branch

A local or remote branch whose latest commit is older than the user's configured stale-branch cutoff. It remains visible in its original position and keeps every normal branch action, but the branch list dims it and adds a `STALE` indicator. The checked-out branch is never presented as stale, preserving its armed-row treatment.

Distinct from a **stale worktree**, which is missing from disk.

### Stale Branch Cleanup Flow

Deleting stale local branches from a preview. The preview lists every stale local branch that is neither the checked-out branch nor checked out in any worktree; remote branches and remote-tracking references are never candidates and never touched. Opening the preview changes nothing; only confirming deletes, and only the ticked branches. An **unmerged** branch (its tip is reachable from neither HEAD nor its own upstream, so deleting it loses commits) is listed but starts unticked, and ticking it is that one branch's acknowledgement — never another's. A branch git refuses to delete stays in the list with git's reason beside it.

#### Synonyms

Cleanup. *Avoid:* prune (reserved for `git worktree prune` and remote pruning).

### Branch Delete Flow

Deleting one branch from its row in the branch list, on one side or both. A local row offers **Delete branch** (the local) and, when it has an upstream, **Delete local & remote**; a remote row offers **Delete remote branch** and, when a local tracks it, **Delete remote & local**. Only a branch nobody stands on qualifies: not the checked-out branch, not checked out in any worktree, and for a remote branch not the checked-out branch's upstream. The entries only arm a confirmation that names each side and, for a local branch, its **unmerged** warning; confirming is that branch's acknowledgement. The remote side is a push (`git push <remote> --delete <branch>`) and goes first, so a failed push leaves the local branch intact. While the deletion runs its rows are **deleting**: struck through and inert, so they cannot be checked out or acted on until the reply arrives.

#### Synonyms

*Avoid:* remove (reserved for worktrees), prune.

### Worktree

A checkout of the repository with its own directory: the original clone is the **main worktree**, additional checkouts created via `git worktree add` are **linked worktrees**, and a linked worktree whose directory no longer exists on disk (but which git still lists) is **stale**. A branch can be checked out in at most one worktree at a time. The bare word "worktree" always means this concept — never the staged/unstaged panel (see Working tree).

#### Example

Parking a long-running `feature` branch in a linked worktree at `../gitgud-feature` lets the user build or test it while the main worktree stays on `main`.

### Working tree

The files of a single checkout as they exist on disk — what the staged/unstaged panel inspects. Distinct from a worktree: a worktree is a checkout location; the working tree is the state of the files within one.

#### Synonyms

Working directory. *Avoid:* worktree (reserved for the checkout-location concept above).

### Uncommitted changes

Any modification to files in the working directory that have not yet been saved to the repository's history with a commit. This includes staged and unstaged changes. This includes dirty submodules.

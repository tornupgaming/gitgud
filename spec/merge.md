# Merge Update

## Currently

The user double clicks a branch in the branches panel to open a dialog that asks whether to merge or checkout the branch. Merge always merges the selected branch into the currently checked out branch.

## Proposed changes

Split the two actions apart. Double-click becomes pure checkout; merge moves to the right-click context menu and gains a direction choice. The combined "switch or merge or cancel" dialog is discarded.

Out of scope for this update (deliberately): predicting conflicts before merging, pulling a branch up to its upstream before merging, auto-fetching a remote ref before merging, and stashing uncommitted changes. The merge uses the tip the user clicked. Conflicts are handled by the existing conflict-resolution flow once they occur. When a flow is blocked by uncommitted changes, it asks the user to resolve them rather than stashing on their behalf — gitgud has no stash.

### Checkout

Double-clicking a branch invokes the **Checkout Flow**. It no longer offers to merge.

#### Checkout Flow

Preconditions, checked in order; the first failure shows a warning Modal. Most failures only let the user exit; the uncommitted-changes case additionally offers a way to proceed (below).

1. **Uncommitted changes.** If the working tree has uncommitted changes — staged, unstaged, or dirty submodules, per `context.md` — show a warning Modal offering two actions: **Discard & continue** and **Cancel**. Untracked files do **not** count: git carries them across a checkout untouched, so blocking on them would surprise the user.
   - **Discard & continue** clears the blocking changes — a hard reset of tracked staged and unstaged changes back to HEAD, plus a reset of dirty submodules to their recorded commits — then proceeds with the flow. Untracked files are left alone (they never blocked). This is **not** the existing per-file `Discard` job, which does the opposite of what's needed here: it keeps staged changes, deletes untracked files, and skips submodules. Discard & continue therefore needs its own worker operation (a reset, not a per-path checkout). It is destructive, so the button is labelled as such and styled like other destructive actions; the explicit click is the confirmation (no second arm-then-confirm step).
   - **Cancel** exits the flow with no change. The user can instead commit via the existing commit box and re-invoke.
   - The flow never stashes (out of scope; gitgud has no stash).
2. **Merge in progress.** If a merge is already in progress, warn ("finish or abort the current merge first") and exit.

These checks read the already-loaded repo snapshot (worktree status and `merging` state), so they are synchronous — no probe round-trip and no loading state.

If both pass, the selected branch is checked out without further confirmation. On success, submodules are updated automatically (the existing submodule-update step) so a moved submodule pointer doesn't leave the worktree dirty — which would otherwise block the next flow.

Double-clicking the currently checked-out branch is a no-op.

#### Remote branches

Double-clicking a remote-tracking branch (e.g. `origin/feature`) must not detach HEAD:

- If no local branch of that name exists, create one tracking the remote (`git checkout -b feature --track origin/feature`) and check it out.
- If a local branch of that name exists, check that one out.
- The preconditions above still apply.

### Merging

Right-clicking a branch that is **not** the currently checked-out branch adds a **Merge** item to the context menu, below "Create branch from here". It opens the merge Modal (it does not act immediately) — mark it with the icon used elsewhere for "opens a modal".

#### Merge Branch Flow

Preconditions, checked in order; the first failure shows a warning Modal (the uncommitted-changes case offers **Discard & continue** exactly as in the Checkout Flow; the others only let the user exit). Like the Checkout Flow's, these are synchronous snapshot reads, so the merge Modal opens directly into the direction choice with no loading state:

1. **Uncommitted changes.** Same check as the Checkout Flow.
2. **Merge in progress.** Same check as the Checkout Flow — if a merge is already in progress, warn ("finish or abort the current merge first") and exit. (Starting a second merge on top of an in-progress one is undefined.)
3. **Detached HEAD.** If HEAD is detached, alert the user with no resolution offered, and exit. (Merging requires a branch to merge into / advance.)

If all pass, the Modal offers a direction:

- **Merge selected into current** — merge the selected branch into the currently checked-out branch. Simple `git merge <selected>`; HEAD does not move.
- **Merge current into selected** — check out the selected branch, merge the previous (original) branch into it, and finish on the **selected** branch (now advanced). The Checkout-Flow preconditions already guarantee a clean switch. The flow records the original branch name so an abort can return to it (below).

A clean merge commits automatically (git's default) and, for "merge current into selected", leaves the user on the advanced selected branch as intended. On conflict, the flow hands off to the existing conflict-resolution flow: the repo enters the in-progress merge state, conflicted files surface in the worktree, and the user resolves them there or aborts the merge.

For "merge current into selected", the whole operation undoes as a unit on abort: the user returns to the **original** branch. Note `git merge --abort` alone does **not** do this — by then HEAD is on the selected branch, so abort lands there. The flow therefore restores the original branch by checking it back out after the abort. The worktree is clean once the merge is aborted, so that checkout is safe.

On a successful merge, submodules are updated automatically (same step as the Checkout Flow), for the same reason: a merge can move submodule pointers, and a stale pointer would leave the worktree dirty and block the next flow.

Non-conflict results surface git's own output through the existing `Acted` result surface — no bespoke copy: "Already up to date." when there was nothing to merge, git's fast-forward note when no merge commit was created (distinct from a merge-commit result), and git's stderr verbatim on any other non-conflict failure (e.g. the checkout step of "merge current into selected" failing).

#### Worker contract

Today's `Job::Merge { id, branch }` merges only into HEAD — it can't express direction. The merge job gains the direction (into-current vs. into-selected); no other flags are needed, since auto-commit, conflict pre-check, and pull-latest are all out of scope.

"Merge current into selected" is a compound sequence — check out the selected branch, then merge the original branch — but it stays one job → one `Update`, per the architecture invariant. The abort-restore is **not** part of the merge job: it is the existing `AbortMerge` followed by a `Checkout` of the remembered original branch, each its own job. The original branch name is held in app state for the lifetime of an into-selected merge, and only the abort path consumes it (a clean merge, or resolve-and-commit, leaves the user on the selected branch and discards it).

#### Remote branches

A remote-tracking branch (e.g. `origin/feature`) is a valid merge source. Merging it never requires a checkout and never detaches HEAD.

- Only one direction is offered: **merge the remote branch into the currently checked-out branch.** "Merge current into selected" is hidden — you cannot merge into a remote ref, and the two-direction choice therefore applies only to local branches.
- The merge uses the last-fetched tip of the remote-tracking ref. (Auto-fetching the latest first is out of scope; the user can fetch via the existing flow if they want the newest tip.)

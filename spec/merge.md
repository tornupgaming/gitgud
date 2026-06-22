# Merge Update

## Currently

The user double clicks a branch in the branches panel to open a dialog that asks whether to merge or checkout the branch. Merge always merges the selected branch into the currently checked out branch.

## Proposed changes

Split the two actions apart. Double-click becomes pure checkout; merge moves to the right-click context menu and gains a direction choice. The combined "switch or merge or cancel" dialog is discarded.

Out of scope for this update (deliberately): predicting conflicts before merging, pulling a branch up to its upstream before merging, and auto-fetching a remote ref before merging. The merge uses the tip the user clicked. Conflicts are handled by the existing conflict-resolution flow once they occur.

### Checkout

Double-clicking a branch invokes the **Checkout Flow**. It no longer offers to merge.

#### Checkout Flow

Preconditions, checked in order; the first failure shows a warning Modal and exits the flow:

1. **Uncommitted changes.** If the working tree has uncommitted changes (staged, unstaged, or dirty submodules — per `context.md`), warn and exit. The user must commit them via the existing commit flow first.
2. **Merge in progress.** If a merge is already in progress, warn ("finish or abort the current merge first") and exit.

If both pass, the selected branch is checked out without further confirmation.

Double-clicking the currently checked-out branch is a no-op.

#### Remote branches

Double-clicking a remote-tracking branch (e.g. `origin/feature`) must not detach HEAD:

- If no local branch of that name exists, create one tracking the remote (`git checkout -b feature --track origin/feature`) and check it out.
- If a local branch of that name exists, check that one out.
- The preconditions above still apply.

### Merging

Right-clicking a branch that is **not** the currently checked-out branch adds a **Merge** item to the context menu, below "Create branch from here". It opens the merge Modal (it does not act immediately) — mark it with the icon used elsewhere for "opens a modal".

#### Merge Branch Flow

Preconditions, checked in order; the first failure shows a warning Modal and exits:

1. **Uncommitted changes.** Same check as the Checkout Flow.
2. **Detached HEAD.** If HEAD is detached, alert the user with no resolution offered, and exit. (Merging requires a branch to merge into / advance.)

If both pass, the Modal offers a direction:

- **Merge selected into current** — merge the selected branch into the currently checked-out branch. Simple `git merge <selected>`; HEAD does not move.
- **Merge current into selected** — check out the selected branch, merge the previous branch into it, and finish on the **selected** branch (now advanced). The Checkout-Flow preconditions already guarantee a clean switch.

A clean merge commits automatically (git's default). On conflict, the flow hands off to the existing conflict-resolution flow: the repo enters the in-progress merge state, conflicted files surface in the worktree, and the user resolves them there or aborts the merge. For "merge current into selected", an abort restores the original branch.

#### Remote branches

A remote-tracking branch (e.g. `origin/feature`) is a valid merge source. Merging it never requires a checkout and never detaches HEAD.

- Only one direction is offered: **merge the remote branch into the currently checked-out branch.** "Merge current into selected" is hidden — you cannot merge into a remote ref, and the two-direction choice therefore applies only to local branches.
- The merge uses the last-fetched tip of the remote-tracking ref. (Auto-fetching the latest first is out of scope; the user can fetch via the existing flow if they want the newest tip.)

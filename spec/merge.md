# Merge Update

## Currently

The user double clicks a branch in the branches panel to open a dialog that asks the user whether to merge or checkout the branch. Merge will always merge the selected branch into the currently checked out branch.

## Proposed changes

### Checkout

Double clicking a branch will now automatically invoke the "checkout flow". It will no longer present the user with the option to merge a branch. The dialog that previously gave the user the option to switch branch or merge or cancel can be discarded.

#### New Flow: Checkout Flow

- A check is made for uncomitted changes. If there are any, the user is presented with a warning and the flow exits without continuing.
  - The user must commit their changes in the existing commit flow.
- The selected branch is now checked out.
- If there are no uncommitted changes, the checkout happens without confirmation.

#### Remote branches

- Double-clicking a remote-tracking branch (e.g. `origin/feature`) must not detach HEAD.
  - If no local branch of that name exists, a local branch is created tracking the remote (`git checkout -b feature --track origin/feature`) and checked out.
  - If a local branch of that name already exists, that local branch is checked out.
- The uncommitted-changes check above still applies.

### Merging

When a user right-clicks a branch that is not currently checked out, the right-click context menu will now give the user the option to merge the branch. Clicking merge will invoke the "merge branch flow". The option will appear below the "Create branch from here" option.
The right-click command for merge should have an icon on the right to show that it opens a modal and does not perform an immediate operation.


#### New Flow: Merge Branch Flow

- A check is made for uncomitted changes. If there are any, the user is presented with a warning and the flow exits without continuing.
- A check is made for whether HEAD is in a detached state. If so, the user is presented with a dialog to alert them of this with no option given to resolve this. The flow exits here and does not allow further flows to continue.
- The user is presented with the choice of:
  - merging the selected branch into the currently checked out branch
    - simple merge into the current branch
  - merging the currently checked out branch into the selected branch
    - checkout selected branch -> merge previous branch into current branch -> stay in current branch
- The user is presented with a checkbox to auto-commit the merge if it's clean
  - The checkbox is enabled by default
- Both choices are checked for eligability first.
  - Will there be merge conflicts?
    - Warning to the user on the files that contain conflicts. This is just a heads-up to let them know before they enter into a conflict.

#### Remote branches

A remote-tracking branch (e.g. `origin/feature`) is a valid merge source. Merging it never requires a checkout and never detaches HEAD.

- Only one direction is offered for a remote branch: merge the remote branch into the currently checked out branch. The "merge currently checked out branch into the selected branch" option is hidden — you cannot merge into a remote ref.
  - The two-direction choice and its sub-steps above therefore apply only to local branches.
- "Latest commit" guarantee: before merging a remote branch, an automatic fetch of that ref runs first, so the merge uses the branch's true tip rather than the last-fetched cache.
  - If the fetch fails (e.g. offline), fall back to merging the last-fetched tip and warn the user it may be stale.

#### Pull latest (local branches with an upstream)

When the selected *local* branch has upstream changes available, the merge modal shows a checkbox to pull those latest upstream changes before merging.

- The checkbox is disabled (unchecked) by default; the default merge uses the branch's current local tip.
- When checked, the selected branch is brought up to its upstream's latest before the merge, so the merge includes the latest commits.
- The checkbox only appears when the selected branch's upstream actually has changes available. Local branches with no upstream, or already up to date, do not show it.

We do not silently retarget a local branch to its upstream: with the checkbox unchecked, "merge `feature`" always means the local tip the user clicked. The remote-tracking row remains available for merging the remote tip directly.
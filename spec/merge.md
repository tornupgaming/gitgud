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
   - **Discard & continue** clears the blocking changes — a hard reset of tracked staged and unstaged changes back to HEAD, plus a reset of dirty submodules to their recorded commits — then proceeds with the flow. Untracked files are left alone (they never blocked). This is **not** the existing per-file `Discard` job, which does the opposite of what's needed here: it keeps staged changes, deletes untracked files, and skips submodules. Discard & continue therefore needs its own reset logic at the ops layer (a reset, not a per-path checkout); it is not a standalone job but rides on the `discard` flag of the job it unblocks (see Worker contract), so the reset and the checkout/merge land as one atomic job → one `Update`, and a failed reset never half-applies into a checkout. It is destructive, so the button is labelled as such and styled like other destructive actions; the explicit click is the confirmation (no second arm-then-confirm step).
   - **Cancel** exits the flow with no change. The user can instead commit via the existing commit box and re-invoke.
   - The flow never stashes (out of scope; gitgud has no stash).
2. **Merge in progress.** If a merge is already in progress, warn ("finish or abort the current merge first") and exit.

These checks read the already-loaded repo snapshot (worktree status and `merging` state), so they are synchronous — no probe round-trip and no loading state.

If both pass, the selected branch is checked out without further confirmation. On success, submodules are updated automatically (see **Submodule update** in the Worker contract) so a moved submodule pointer doesn't leave the worktree dirty — which would otherwise block the next flow.

Double-clicking the currently checked-out branch is a no-op.

#### Remote branches

Double-clicking a remote-tracking branch (e.g. `origin/feature`) must not detach HEAD:

- If no local branch of that name exists, create one tracking the remote (`git checkout -b feature --track origin/feature`) and check it out.
- If a local branch of that name exists, check that one out.
- The preconditions above still apply.

### Merging

Right-clicking a branch that is **not** the currently checked-out branch adds a **Merge** item to the context menu, below "Create branch from here". It opens the merge Modal (it does not act immediately); label it `Merge…` — the trailing ellipsis is the codebase's existing "opens further UI" affordance (e.g. the commit-box hint), so no new icon is introduced.

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

**Job changes.** Two existing jobs change; no new `Job` or `Update` variant is added.

- `Job::Merge { id, branch }` gains `direction: MergeDirection` (`IntoCurrent` | `IntoSelected`). No other merge flags are needed — auto-commit, conflict pre-check, and pull-latest are all out of scope.
- `Job::Checkout` and `Job::Merge` each gain `discard: bool`. When set, the worker runs the reset (below) as the job's first step, then proceeds. This keeps Discard & continue atomic: reset and checkout/merge are one job → one `Update`, and a failed reset replies `Err` without ever checking out or merging — no half-applied state, and the UI thread holds no deferred-intent across an async round-trip.

**The reset (Discard & continue).** A new ops-layer function, not a job. It runs `git reset --hard HEAD` (drops tracked staged + unstaged changes, leaves untracked) followed by `git submodule update --recursive --force --no-fetch` (resets dirty submodules to their recorded commits). `--no-fetch` keeps it local. This deliberately also resets a staged/unstaged submodule **pointer** change; that does not contradict the per-file `Discard`'s refusal to reset a lone gitlink (a silent footgun), because here the user has explicitly asked to discard the whole worktree, so moving the submodule to match HEAD is the intended result.

**Submodule update (after checkout / merge).** There is no pre-existing automatic submodule step — checkout and merge today only trigger `Refresh` + `LoadHistory`. Rather than chain the network `UpdateSubmodules` job after every switch (a per-switch network round-trip with its own ordering and busy-state to manage), fold a local `git submodule update --recursive --no-fetch` into the tail of the Checkout and Merge **ops** themselves, after the checkout/merge succeeds and before the op returns, and only when the repo has submodules. `--no-fetch` guarantees it stays local; if a recorded commit genuinely isn't present locally the submodule is left at its old commit (dirty pointer) and the user can update it from the SUBMODULES panel, which fetches. Keeping it inside the op preserves one job → one `Update` and removes the sequencing question entirely.

**Uncommitted-changes gate.** The synchronous snapshot read counts a change as blocking when: `staged` is non-empty, **or** `unstaged` contains any entry whose kind is not `Untracked`, **or** any `SubmoduleInfo.dirty` is true. Untracked entries in `unstaged` are ignored (git carries them across a checkout). A submodule **pointer** change (`ChangeKind::Submodule` in staged/unstaged) does count — it is a real uncommitted change — and Discard & continue's reset clears it.

**"Merge current into selected" + abort restore.** This direction is a compound op — check out the selected branch, then merge the original — but still one job → one `Update`. The abort-restore is **not** part of the job: it is the existing `AbortMerge` followed by a `Checkout` of the remembered original branch, each its own job. The app holds the original in `RepoTab::merge_return: Option<String>`:

1. Dispatching *any* checkout or merge flow first clears `merge_return` (fresh slate — stops a stale value from a prior into-selected merge bouncing a later, unrelated abort).
2. An into-selected merge sets `merge_return = Some(original)` when its job is dispatched.
3. On `Acted(AbortMerge, Ok)`: if `merge_return` is `Some(orig)`, send `Checkout { branch: orig, discard: false }` (the worktree is clean after an abort) and `take()` the field.
4. On `Acted(Commit, Ok)`: `take()` the field — resolve-and-commit has concluded the merge on the selected branch.

A clean into-selected merge leaves `merge_return` set but inert: it is consumed only on abort, and abort requires merge-in-progress state, which a clean merge does not produce. Rule 1 clears it at the next flow. No follow-up `Refresh` has to be inspected to decide when to clear.

#### Remote branches

A remote-tracking branch (e.g. `origin/feature`) is a valid merge source. Merging it never requires a checkout and never detaches HEAD.

- Only one direction is offered: **merge the remote branch into the currently checked-out branch.** "Merge current into selected" is hidden — you cannot merge into a remote ref, and the two-direction choice therefore applies only to local branches.
- The merge uses the last-fetched tip of the remote-tracking ref. (Auto-fetching the latest first is out of scope; the user can fetch via the existing flow if they want the newest tip.)

### Resolved questions

- **Stale snapshot.** The gate trusts the last `Refresh`; the tree can change on disk afterward. Accepted — the gate is a fast UX guard, not a safety boundary. git refuses an unsafe checkout/merge on its own and the error surfaces through `Acted`. No pre-flight probe is added.
- **into-selected with nothing to merge.** It still checks out the selected branch (the merge is then "Already up to date."). Intended: the result equals a checkout plus a no-op merge, and the user ends on the selected branch as the direction promises.
- **Context-menu gating.** The **Merge…** item shows only when the row is not HEAD (`is_head == false`). For a remote row (`is_remote == true`) the Modal offers only the into-current direction.

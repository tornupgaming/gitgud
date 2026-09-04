//! The data contract between the UI thread and the git worker thread.
//!
//! Architecture (think C#: a background worker + two message queues):
//! the UI thread never touches a `git2::Repository` directly. It sends a
//! [`Job`] over a channel, the worker thread executes it against libgit2,
//! and sends back exactly one [`Update`]. The UI drains updates once per
//! frame. This keeps the UI at full framerate no matter how big the repo is.
//!
//! Every type here is plain owned data (`String`, `Vec`) so it can be moved
//! across threads without locks -- Rust's ownership model makes this a
//! compile-time guarantee, not a convention.

pub mod graph;
pub mod ops;
pub mod worker;

use std::path::PathBuf;

/// Handle for one open repository tab. Assigned by the UI, used by both sides.
pub type RepoId = u64;

/// How many commits one `LoadHistory` page returns.
pub const HISTORY_PAGE: usize = 250;

/// A request from the UI to the worker. Each job (except `Close`) produces
/// exactly one `Update` -- the UI counts in-flight jobs to show a busy state.
#[derive(Debug, Clone)]
pub enum Job {
    /// Open the repo at (or above -- discovery walks up) `path` and register
    /// it under `id`.
    Open {
        id: RepoId,
        path: PathBuf,
    },
    /// Drop the repo handle. The one job with no `Update` reply.
    Close {
        id: RepoId,
    },
    /// Reload HEAD, branches, and working-tree status.
    Refresh {
        id: RepoId,
    },
    /// Load one page of commit history starting at `skip`.
    LoadHistory {
        id: RepoId,
        skip: usize,
    },
    /// Compute the diff identified by `key`; the reply echoes the key.
    LoadDiff {
        id: RepoId,
        key: DiffKey,
    },
    /// `path: None` means "all files".
    Stage {
        id: RepoId,
        path: Option<String>,
    },
    Unstage {
        id: RepoId,
        path: Option<String>,
    },
    /// Throw away unstaged working-tree changes; deletes untracked files.
    /// `path: None` means "all files". Destructive -- the UI confirms first.
    Discard {
        id: RepoId,
        path: Option<String>,
    },
    /// Commit the index. Inside a merge this records MERGE_HEAD as the
    /// second parent and clears the merge state.
    Commit {
        id: RepoId,
        message: String,
    },
    /// `branch` is a local name (`main`) or remote name (`origin/main`,
    /// which creates a local tracking branch first). `discard` runs a hard
    /// reset of tracked + submodule changes first ("Discard & continue"), so
    /// the reset and checkout land as one job.
    Checkout {
        id: RepoId,
        branch: String,
        discard: bool,
    },
    /// Create a new local branch `name` pointing at `start_point` (a commit
    /// id or an existing branch name). With `checkout`, also switch HEAD to
    /// it -- the "create branch from here" action on a commit or branch.
    CreateBranch {
        id: RepoId,
        name: String,
        start_point: String,
        checkout: bool,
    },
    /// Merge `branch` via the git CLI (respects the user's merge config and
    /// hooks). `direction` picks which way: `IntoCurrent` merges `branch` into
    /// HEAD; `IntoSelected` checks out `branch` and merges the previous branch
    /// into it, finishing on `branch`. `discard` hard-resets first, same as
    /// `Checkout`. A conflicted merge is a *successful* job: the repo enters
    /// the merge state and the conflicts surface through `Refresh`.
    Merge {
        id: RepoId,
        branch: String,
        direction: MergeDirection,
        discard: bool,
    },
    /// `git merge --abort`: restore the pre-merge state. Destructive to
    /// resolution progress -- the UI confirms first.
    AbortMerge {
        id: RepoId,
    },
    /// Parse one conflicted file into its marker segments.
    LoadConflict {
        id: RepoId,
        path: String,
    },
    /// Write a conflict resolution to the working tree and stage it.
    ResolveConflict {
        id: RepoId,
        path: String,
        resolution: Resolution,
    },
    /// Runs `git fetch --all --prune` via the CLI on its own thread.
    Fetch {
        id: RepoId,
    },
    /// Pushes the current branch to origin via the CLI on its own thread.
    /// First fetches and checks for divergence: if origin has commits the
    /// local branch lacks *and* the local branch has its own, the worker
    /// replies [`Update::Diverged`] instead so the UI can ask the user how to
    /// reconcile (the push would otherwise be rejected as non-fast-forward).
    Push {
        id: RepoId,
    },
    /// Adds `url` as the `origin` remote, then pushes — the confirm action of
    /// the no-origin dialog armed by [`Update::NoOrigin`]. Network op, runs on
    /// its own thread like push.
    AddOrigin {
        id: RepoId,
        url: String,
    },
    /// Pulls the current branch from its upstream via the CLI on its own
    /// thread. Fast-forwards when it cleanly can; on divergence replies
    /// [`Update::Diverged`] so the user chooses merge vs rebase rather than
    /// the app silently picking one.
    Pull {
        id: RepoId,
    },
    /// Reconciles a diverged branch with the user's chosen strategy (after a
    /// [`Update::Diverged`] prompt), then pushes when the flow began at the
    /// PUSH button. Network op, runs on its own thread.
    Integrate {
        id: RepoId,
        strategy: SyncStrategy,
        push_after: bool,
    },
    /// `git submodule update --init --recursive` (`path: None` = all). Clones
    /// or checks out submodules to their recorded commits. Network op (it may
    /// fetch), so it runs on its own thread like fetch/push/pull.
    UpdateSubmodules {
        id: RepoId,
        path: Option<String>,
        init: bool,
    },
    /// `git worktree add`: create a linked worktree at `path` checking out
    /// `branch` -- with `create_branch`, a new branch of that name off HEAD.
    /// Local CLI op; the UI opens the new worktree as its own tab on success
    /// (see ADR 0001: a worktree is a tab).
    WorktreeAdd {
        id: RepoId,
        path: PathBuf,
        branch: String,
        create_branch: bool,
    },
    /// `git worktree remove`: delete a linked worktree's directory and its
    /// bookkeeping; the branch it had checked out is kept. Without `force` a
    /// worktree with uncommitted changes replies [`Update::WorktreeDirty`]
    /// instead of acting, so the UI can escalate to Discard & remove -- the
    /// app never force-removes on its own.
    WorktreeRemove {
        id: RepoId,
        path: PathBuf,
        force: bool,
    },
    /// `git worktree prune`: drop the records of stale worktrees (listed by
    /// git but whose directories no longer exist on disk).
    WorktreePrune {
        id: RepoId,
    },
    /// Delete local branches -- the confirm action of the Stale Branch Cleanup
    /// Flow. Each entry is tried on its own (`git branch -d`, or `-D` only for
    /// a branch whose unmerged work the user acknowledged) so one refusal never
    /// stops the rest; the reply is [`Update::BranchesDeleted`]. Never touches
    /// a remote or remote-tracking ref.
    DeleteBranches {
        id: RepoId,
        branches: Vec<BranchDelete>,
    },
    /// The Branch Delete Flow's confirm action: delete one branch on the
    /// chosen sides. `local` is `git branch -d`/`-D` as in
    /// [`Job::DeleteBranches`]; `remote` is `git push <remote> --delete
    /// <branch>` -- a network op, run off-thread like a push. When both are
    /// given the remote goes first so a failed push leaves the local branch
    /// (and its commits) untouched. Replies [`Update::Acted`] with
    /// [`ActionKind::DeleteBranch`].
    DeleteBranch {
        id: RepoId,
        local: Option<BranchDelete>,
        remote: Option<RemoteBranch>,
    },
}

/// A branch on a remote, addressed the way `git push` wants it: the remote's
/// name and the branch's name there (`origin` + `feature`, never the
/// remote-tracking form `origin/feature`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranch {
    pub remote: String,
    pub branch: String,
}

/// One local branch to delete. `force` is the user's per-branch
/// acknowledgement that the branch holds unmerged commits: it turns the safe
/// `-d` (which git refuses for unmerged work) into `-D`. The UI only sets it
/// from that branch's own checkbox -- acknowledging one branch never covers
/// another.
#[derive(Debug, Clone)]
pub struct BranchDelete {
    pub name: String,
    pub force: bool,
}

/// A reply from the worker. `Result<_, String>` is Rust's standard
/// success-or-error type (like a discriminated union of value | message).
/// Every reply carries the `id` of the tab it answers; the UI routes on it
/// and ignores ids for tabs that have since closed.
#[derive(Debug)]
pub enum Update {
    Opened {
        id: RepoId,
        result: Result<RepoMeta, String>,
    },
    Refreshed {
        id: RepoId,
        result: Result<RefreshData, String>,
    },
    History {
        id: RepoId,
        result: Result<HistoryPage, String>,
    },
    /// Echoes the `key` so the UI can drop replies for stale selections.
    Diff {
        id: RepoId,
        key: DiffKey,
        result: Result<DiffData, String>,
    },
    /// Echoes the `path` so the UI can drop replies for stale selections.
    Conflict {
        id: RepoId,
        path: String,
        result: Result<ConflictFile, String>,
    },
    /// Outcome of a mutating action; `Ok` carries a status-bar message.
    Acted {
        id: RepoId,
        kind: ActionKind,
        result: Result<String, String>,
    },
    /// A push or pull found the current branch diverged from its upstream:
    /// each side has commits the other lacks. The UI prompts the user to pick
    /// a reconcile strategy, then sends [`Job::Integrate`]. `kind` records
    /// which button opened the flow (and so whether a push should follow).
    Diverged {
        id: RepoId,
        kind: SyncKind,
        /// Commits the upstream has that the local branch lacks.
        behind: usize,
        /// Commits the local branch has that the upstream lacks.
        ahead: usize,
    },
    /// A push found no `origin` remote configured: there is nowhere to push
    /// until one exists. The UI prompts for a URL, then sends
    /// [`Job::AddOrigin`] — the app never invents a remote on its own.
    NoOrigin { id: RepoId },
    /// An unforced [`Job::WorktreeRemove`] found the worktree dirty
    /// (uncommitted changes). Terminal reply: the UI escalates its confirm
    /// modal to Discard & remove, which re-sends the job with `force`.
    WorktreeDirty { id: RepoId, path: PathBuf },
    /// Outcome of a [`Job::DeleteBranches`]: the branches git deleted and the
    /// ones it refused, each with git's reason. Partial success is normal here
    /// (deletions run one by one), which is why this is not a plain `Acted`:
    /// the UI refreshes for the deletions *and* surfaces every refusal.
    BranchesDeleted {
        id: RepoId,
        deleted: Vec<String>,
        failed: Vec<(String, String)>,
    },
    /// A mid-flight progress report from a long-running network op, streamed
    /// from the worker's network thread *before* the terminal [`Update::Acted`]
    /// (or [`Update::Diverged`]). Several arrive per op; they drive the progress
    /// modal's step checklist and console tail. `op` is the op label
    /// (`PUSH`/`PULL`/`FETCH`/`SUBMODULES`) so the UI routes the event to the
    /// right entry when ops overlap. Unlike every other reply this is *not*
    /// terminal: it neither answers the job's in-flight count nor closes the op.
    Progress {
        id: RepoId,
        op: &'static str,
        event: ProgressEvent,
    },
}

/// One unit of live progress for a network op. A [`Step`] announces or updates
/// a checklist row (re-sent with the same `key` to change its state); a
/// [`Console`] line is one piece of raw subprocess output for the tail readout
/// so the user can see the remote actually responding.
///
/// [`Step`]: ProgressEvent::Step
/// [`Console`]: ProgressEvent::Console
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// Add the step under `key`, or update it in place if `key` already exists.
    Step {
        key: String,
        label: String,
        state: StepState,
    },
    /// One line of subprocess output (stdout or stderr), already split on
    /// `\n`/`\r` so transfer-progress redraws arrive as they happen. Bounded
    /// per line in [`ops`] and capped in the UI ring buffer.
    Console(String),
}

impl ProgressEvent {
    /// Shorthand for a [`Step`] event from string slices.
    ///
    /// [`Step`]: ProgressEvent::Step
    pub fn step(key: &str, label: &str, state: StepState) -> Self {
        ProgressEvent::Step {
            key: key.to_string(),
            label: label.to_string(),
            state,
        }
    }
}

/// A checklist step's lifecycle: `Running` shows a spinner, `Done` a check,
/// `Failed` a cross. A step is born `Running` and re-sent once with its outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Failed,
}

/// How to reconcile a diverged branch, chosen by the user in the sync dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStrategy {
    /// `git pull --no-rebase`: join the two histories with a merge commit.
    Merge,
    /// `git pull --rebase`: replay local commits on top of the upstream.
    Rebase,
}

/// Which button opened the sync flow. Decides whether a push follows the
/// reconcile and tailors the dialog copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    Push,
    Pull,
}

/// Which way a merge started from the branch list runs. `IntoCurrent` merges
/// the selected branch into HEAD (HEAD stays put); `IntoSelected` checks out
/// the selected branch and merges the previously-current branch into it,
/// finishing on the (now advanced) selected branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDirection {
    IntoCurrent,
    IntoSelected,
}

/// How to settle one conflicted file. `Ours`/`Theirs` take a whole side from
/// the index (also covers binary and delete-vs-modify conflicts); `Content`
/// carries the text assembled from per-block choices in the conflict view.
#[derive(Debug, Clone)]
pub enum Resolution {
    Ours,
    Theirs,
    Content(String),
}

/// Which job an `Acted` reply answers. The UI keys follow-up behaviour on
/// this -- every kind triggers a `Refresh`, and history-changing kinds
/// (commit, checkout, create-branch, fetch, push, pull) also re-send
/// `LoadHistory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Stage,
    Unstage,
    Discard,
    Commit,
    Checkout,
    CreateBranch,
    Merge,
    AbortMerge,
    Resolve,
    Fetch,
    Push,
    Pull,
    UpdateSubmodules,
    WorktreeAdd,
    WorktreeRemove,
    WorktreePrune,
    DeleteBranch,
}

#[derive(Debug, Clone)]
pub struct RepoMeta {
    /// Directory name, used as the tab label.
    pub name: String,
    pub workdir: PathBuf,
}

/// Everything the sidebar shows. Each refresh replaces the previous
/// snapshot wholesale -- no incremental diffing of state.
#[derive(Debug)]
pub struct RefreshData {
    /// Current branch name, or short commit id when detached.
    pub head: String,
    pub head_detached: bool,
    /// Locals first, then remotes, each group sorted by name.
    pub branches: Vec<BranchInfo>,
    /// Changes already in the index (will be part of the next commit).
    pub staged: Vec<FileChange>,
    /// Working-tree changes not yet staged (includes untracked files).
    pub unstaged: Vec<FileChange>,
    /// `Some` while a merge is in progress (MERGE_HEAD exists), whether it
    /// was started here or in an external terminal.
    pub merging: Option<MergeInfo>,
    /// Submodules registered in the repo, for the sidebar panel. Empty for
    /// repos without submodules (the panel then hides itself).
    pub submodules: Vec<SubmoduleInfo>,
    /// Every worktree of this repository (main first, then linked), for the
    /// worktree bar. The bar hides itself while there is only the main one.
    /// Empty when listing fails (e.g. no git CLI) -- the feature just hides.
    pub worktrees: Vec<WorktreeInfo>,
}

/// One worktree of a repository, parsed from `git worktree list --porcelain`.
/// The UI treats the entry whose `path` matches the tab's own as "where I am".
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Absolute path of the worktree's root directory.
    pub path: PathBuf,
    /// Checked-out branch name, or the short commit id when detached.
    pub branch: String,
    pub detached: bool,
    /// The original checkout (always listed first by git; never removable).
    pub is_main: bool,
    /// Stale: git still lists it but its directory is gone from disk. The bar
    /// offers PRUNE instead of jump for these.
    pub stale: bool,
    pub locked: bool,
}

/// One submodule's state for the SUBMODULES panel. Pointer changes (the parent
/// recording a new submodule commit) surface separately as `ChangeKind::Submodule`
/// rows in the working tree; this drives the overview and the open/update actions.
#[derive(Debug, Clone)]
pub struct SubmoduleInfo {
    /// Repo-relative path, also the open target and the row label. Confined
    /// to the worktree (no `..`/absolute components) before it reaches here.
    pub path: String,
    /// Short id of the submodule's checked-out HEAD, empty when uninitialized.
    pub head_short: String,
    /// The submodule directory is populated (checked out), not just registered.
    pub initialized: bool,
    /// Working tree or index inside the submodule is dirty (uncommitted work).
    pub dirty: bool,
    /// The submodule's HEAD moved past the commit the parent's index records
    /// -- an unstaged pointer change waiting to be staged.
    pub new_commits: bool,
    /// The parent's index records a different submodule commit than its HEAD
    /// -- a pointer change already staged.
    pub staged: bool,
}

#[derive(Debug, Clone)]
pub struct MergeInfo {
    /// Short id of MERGE_HEAD, for the banner readout.
    pub head_short: String,
    /// Suggested commit message from `.git/MERGE_MSG` ("Merge branch 'x'").
    pub msg: String,
}

#[derive(Debug, Clone)]
pub struct BranchInfo {
    /// `main` for local, `origin/main` for remote branches.
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    /// Unix timestamp of the branch tip commit. `None` only when the ref
    /// cannot be peeled to a commit; such a branch is never called stale.
    pub latest_commit_time: Option<i64>,
    /// Local branches only: the tip is reachable from neither HEAD nor the
    /// branch's own upstream, so deleting it would lose commits -- the same
    /// test `git branch -d` applies before refusing. Always false for remotes.
    pub unmerged: bool,
    /// Commits ahead/behind the upstream branch. Both 0 when no upstream.
    pub ahead: usize,
    pub behind: usize,
    /// Local branches only: the upstream's remote-tracking name
    /// (`origin/feature`), i.e. the `name` of the remote row it tracks.
    /// `None` when no upstream is configured or it no longer exists.
    pub upstream: Option<String>,
    /// Remote branches only: where the branch lives, so it can be deleted
    /// there (`git push` addresses the remote and the branch separately).
    /// `None` when the remote-tracking ref maps to no configured remote.
    pub remote: Option<RemoteBranch>,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    /// Path relative to the repo root, `/`-separated (git convention).
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Typechange,
    Untracked,
    Conflicted,
    /// A submodule gitlink whose recorded commit changed. Distinct from a file
    /// so the UI can stage it but withhold discard (resetting a gitlink without
    /// moving the submodule checkout is a footgun).
    Submodule,
}

impl ChangeKind {
    /// One-letter status badge, mirroring `git status --short`.
    pub fn glyph(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::Typechange => 'T',
            ChangeKind::Untracked => '?',
            ChangeKind::Conflicted => '!',
            ChangeKind::Submodule => 'S',
        }
    }
}

#[derive(Debug)]
pub struct HistoryPage {
    /// The `skip` this page was requested with; `skip == 0` replaces the list.
    pub skip: usize,
    pub commits: Vec<CommitInfo>,
    /// True when the walk hit the root commit -- hides the "load more" row.
    pub reached_end: bool,
    /// Ids of commits reachable from HEAD but from no other branch tip
    /// (the current branch's upstream excepted): the work that exists only
    /// on the current branch. Drives the graph's branch-focus dimming and
    /// node glow. Independent of paging, so only the first page
    /// (`skip == 0`) carries it; later pages leave it empty.
    pub head_only: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Full 40-char hex id.
    pub id: String,
    /// First 7 chars, for display.
    pub short: String,
    pub summary: String,
    pub author: String,
    pub email: String,
    /// Unix timestamp (seconds). UI renders it as a relative time.
    pub time: i64,
    pub is_merge: bool,
    /// Branch names pointing at this commit, for decoration chips.
    pub refs: Vec<String>,
    /// Full 40-char ids of the parent commits, first parent first. Drives
    /// the graph lane layout in [`graph`].
    pub parents: Vec<String>,
}

/// Identifies what the diff panel is showing. `PartialEq` lets the UI check
/// "is this reply still for the current selection?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKey {
    /// One file from the working tree; `staged` picks index-vs-HEAD or
    /// workdir-vs-index.
    Workdir { path: String, staged: bool },
    /// Every file touched by one commit (vs its first parent).
    Commit { id: String },
    /// One conflicted file, shown in the resolution view instead of a diff.
    Conflict { path: String },
}

/// One conflicted file, classified by what resolution UI it supports.
#[derive(Debug, Clone)]
pub enum ConflictFile {
    /// Both sides changed a text file; the working tree holds marker blocks
    /// that can be resolved per block.
    Text {
        /// Labels from the markers: `<<<<<<< HEAD` / `>>>>>>> branch`.
        ours_label: String,
        theirs_label: String,
        segments: Vec<ConflictSegment>,
        /// Whether the original file ended with a newline, so reassembly
        /// does not dirty the last line.
        trailing_newline: bool,
    },
    /// No markers to pick through; whole-file ours/theirs only.
    Binary,
    /// Deleted on our side, modified on theirs: ours = stay deleted.
    DeletedByUs,
    /// Modified here, deleted on theirs: theirs = delete the file.
    DeletedByThem,
}

/// A run of lines from a conflicted file: either shared context or one
/// `<<<<<<<` ... `>>>>>>>` block. A diff3/zdiff3 `|||||||` base section is
/// consumed by the parser but not kept -- resolution is choice-driven, so
/// the base never reaches the output.
#[derive(Debug, Clone)]
pub enum ConflictSegment {
    Context(Vec<String>),
    Conflict {
        ours: Vec<String>,
        theirs: Vec<String>,
    },
}

/// A whole diff, pre-flattened for the UI. `adds`/`dels` are totals across
/// all files. Line output is capped (see `ops`) so a pathological diff
/// cannot stall the render -- a truncated diff ends with a marker line.
#[derive(Debug, Clone)]
pub struct DiffData {
    pub files: Vec<FileDiff>,
    pub adds: usize,
    pub dels: usize,
}

/// One file's slice of a diff; `adds`/`dels` count this file only.
/// Binary files have `binary` set and no lines.
#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Line numbers in the old/new file; 0 means "no number" (e.g. an added
    /// line has no old number).
    pub old_no: u32,
    pub new_no: u32,
    /// Line content without the trailing newline.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Ctx,
    Add,
    Del,
    /// `@@ -a,b +c,d @@` hunk separator.
    Hunk,
}

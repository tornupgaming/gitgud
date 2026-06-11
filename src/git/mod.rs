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
    Open { id: RepoId, path: PathBuf },
    Close { id: RepoId },
    /// Reload HEAD, branches, and working-tree status.
    Refresh { id: RepoId },
    /// Load one page of commit history starting at `skip`.
    LoadHistory { id: RepoId, skip: usize },
    LoadDiff { id: RepoId, key: DiffKey },
    /// `path: None` means "all files".
    Stage { id: RepoId, path: Option<String> },
    Unstage { id: RepoId, path: Option<String> },
    Commit { id: RepoId, message: String },
    /// `branch` is a local name (`main`) or remote name (`origin/main`,
    /// which creates a local tracking branch first).
    Checkout { id: RepoId, branch: String },
    /// Runs `git fetch --all --prune` via the CLI on its own thread.
    Fetch { id: RepoId },
}

/// A reply from the worker. `Result<_, String>` is Rust's standard
/// success-or-error type (like a discriminated union of value | message).
#[derive(Debug)]
pub enum Update {
    Opened { id: RepoId, result: Result<RepoMeta, String> },
    Refreshed { id: RepoId, result: Result<RefreshData, String> },
    History { id: RepoId, result: Result<HistoryPage, String> },
    /// Echoes the `key` so the UI can drop replies for stale selections.
    Diff { id: RepoId, key: DiffKey, result: Result<DiffData, String> },
    /// Outcome of a mutating action; `Ok` carries a status-bar message.
    Acted { id: RepoId, kind: ActionKind, result: Result<String, String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Stage,
    Unstage,
    Commit,
    Checkout,
    Fetch,
}

#[derive(Debug, Clone)]
pub struct RepoMeta {
    /// Directory name, used as the tab label.
    pub name: String,
    pub workdir: PathBuf,
}

#[derive(Debug)]
pub struct RefreshData {
    /// Current branch name, or short commit id when detached.
    pub head: String,
    pub head_detached: bool,
    pub branches: Vec<BranchInfo>,
    /// Changes already in the index (will be part of the next commit).
    pub staged: Vec<FileChange>,
    /// Working-tree changes not yet staged (includes untracked files).
    pub unstaged: Vec<FileChange>,
}

#[derive(Debug, Clone)]
pub struct BranchInfo {
    /// `main` for local, `origin/main` for remote branches.
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    /// Commits ahead/behind the upstream branch. Both 0 when no upstream.
    pub ahead: usize,
    pub behind: usize,
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
}

#[derive(Debug)]
pub struct DiffData {
    pub files: Vec<FileDiff>,
    pub adds: usize,
    pub dels: usize,
}

#[derive(Debug)]
pub struct FileDiff {
    pub path: String,
    pub adds: usize,
    pub dels: usize,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug)]
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

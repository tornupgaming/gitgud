//! Application state: repo tabs, worker wiring, persistence, and the
//! per-frame loop that turns worker updates and UI clicks into jobs.
//!
//! The UI thread never touches `git2` -- repo work is a `Job` sent to the
//! worker and an `Update` handled when it comes back. Clicks made during
//! panel layout can't send directly (the active tab is borrowed mutably),
//! so they collect into a frame-local `Vec<Job>` flushed after layout.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, CentralPanel, Frame, Key, Margin, Panel, Stroke};
use notify::Watcher;

use crate::git::worker::GitWorker;
use crate::git::{
    ActionKind, BranchInfo, ChangeKind, CommitInfo, DiffData, DiffKey, FileChange, Job, MergeInfo,
    ProgressEvent, RepoId, StepState, SubmoduleInfo, SyncKind, Update, WorktreeInfo, graph::Graph,
};
use crate::theme::{self, ACCENT, BG1};
use crate::ui::{self, TopBarAction, conflict::ConflictView, diff::DiffView};

/// Minimum gap between watcher-triggered refreshes. Bursty writers (saves,
/// builds) collapse into one status scan per window instead of one per event.
const AUTO_REFRESH_GAP: Duration = Duration::from_millis(800);

/// A branch-creation awaiting a name, armed by "create branch from here" on a
/// commit or branch. `start_point` is what the new branch points at (a commit
/// id or branch name); `label` is its short display form; `name` is the
/// in-progress text from the dialog.
pub struct NewBranch {
    pub start_point: String,
    pub label: String,
    pub name: String,
    /// Set when the dialog opens so the name field grabs focus once.
    pub needs_focus: bool,
}

impl NewBranch {
    pub fn new(start_point: String, label: String) -> Self {
        Self {
            start_point,
            label,
            name: String::new(),
            needs_focus: true,
        }
    }
}

/// A worktree-add awaiting the modal's confirm (the Worktree Add Flow).
/// Armed by the sidebar's "Open in new worktree…" (branch fixed) or the bar's
/// ADD button (branch typed). The job only fires on confirm.
pub struct NewWorktree {
    /// Branch fixed by the sidebar entry; `None` means the modal's branch
    /// field decides (an existing local branch checks out, a new name creates).
    pub fixed_branch: Option<String>,
    /// In-progress branch text when not fixed.
    pub branch: String,
    /// Editable destination path; re-derived from the branch until edited.
    pub path: String,
    /// The user touched the path field -- stop re-deriving the suggestion.
    pub path_edited: bool,
    /// Set when the dialog opens so the first field grabs focus once.
    pub needs_focus: bool,
    /// BROWSE folder picker in flight (rfd blocks, so it runs on a thread);
    /// resolves to the new parent directory for the destination.
    pub picker: Option<Receiver<Option<PathBuf>>>,
}

impl NewWorktree {
    pub fn new(fixed_branch: Option<String>) -> Self {
        Self {
            fixed_branch,
            branch: String::new(),
            path: String::new(),
            path_edited: false,
            needs_focus: true,
            picker: None,
        }
    }
}

/// A worktree removal awaiting confirmation (the Worktree Remove Flow).
/// `dirty` escalates the modal to Discard & remove after git refused the
/// unforced attempt ([`Update::WorktreeDirty`]).
#[derive(Clone)]
pub struct PendingWorktreeRemove {
    pub path: PathBuf,
    pub dirty: bool,
}

/// UI-side state for one open repository: the worker's latest snapshots plus
/// view state (selection, armed confirmations, in-flight accounting). The
/// worker owns the matching `Repository`; this holds only plain data.
pub struct RepoTab {
    pub id: RepoId,
    pub name: String,
    pub path: PathBuf,
    pub head: String,
    pub head_detached: bool,
    pub branches: Vec<BranchInfo>,
    /// Branch-tree folders the user has collapsed, keyed by full folder path
    /// (`feature`, `origin/feature`). Absent = expanded, so the default-open
    /// behaviour needs no seeding and survives refreshes (which replace
    /// `branches` but not this).
    pub collapsed_folders: HashSet<String>,
    /// The working-tree and commit-detail file lists render as a path tree
    /// when set; the TREE/LIST header toggle flips it.
    pub tree_view: bool,
    /// Collapsed folders in the file trees, keyed by section-prefixed folder
    /// path (`staged/src`, `unstaged/src/ui`, `commit/assets`). Same absent =
    /// expanded convention as `collapsed_folders`.
    pub collapsed_file_folders: HashSet<String>,
    /// Submodules for the SUBMODULES panel; empty hides it.
    pub submodules: Vec<SubmoduleInfo>,
    /// Worktrees of this repository, main first (see ADR 0001: a worktree is
    /// a tab). Drives the worktree bar, which hides while only the main
    /// worktree exists.
    pub worktrees: Vec<WorktreeInfo>,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub commits: Vec<CommitInfo>,
    /// Lane layout for `commits`; recomputed whenever `commits` changes.
    pub graph: Graph,
    /// Ids of commits that exist only on the current branch (reachable from
    /// HEAD but from no other branch tip). Replaced with each first history
    /// page; drives the graph's branch-focus dimming and node glow.
    pub head_only: HashSet<String>,
    /// Branch focus: dim graph commits that other branches also reach, so
    /// the work unique to the current branch stands out. Toggled from the
    /// COMMIT GRAPH header; a no-op while `head_only` is empty.
    pub focus_head: bool,
    /// The last history page has been reached -- hides LOAD MORE.
    pub history_done: bool,
    /// What the center panel is bound to. Diff/conflict replies echo their
    /// key and are dropped unless it still matches this.
    pub selection: Option<DiffKey>,
    /// Flattened rows for the center diff view; `None` while a reply is in
    /// flight.
    pub diff: Option<DiffView>,
    /// Raw diff of the selected commit: feeds the right-panel file list and
    /// per-file diff views. Set when a `DiffKey::Commit` reply arrives.
    pub commit_diff: Option<DiffData>,
    /// File from `commit_diff` open in the center diff view; `None` keeps
    /// the graph visible.
    pub commit_file: Option<String>,
    /// `Some` while the repo is mid-merge; drives the merge banner, the
    /// merge-commit message prefill, and the commit gate.
    pub merging: Option<MergeInfo>,
    /// Parsed conflict for the `DiffKey::Conflict` selection, `None` while
    /// the reply is in flight.
    pub conflict: Option<ConflictView>,
    /// Abort-merge awaiting confirmation, same arm-then-confirm flow as
    /// checkout and discard.
    pub pending_abort_merge: bool,
    pub commit_msg: String,
    /// Branch double-clicked whose Checkout Flow is blocked by a precondition
    /// (uncommitted changes or a merge in progress). A clean checkout fires
    /// immediately and never sets this; while set, the warning modal is shown.
    pub pending_checkout: Option<String>,
    /// Merge started from a branch's context menu; drives the merge modal
    /// (preconditions then direction choice).
    pub pending_merge: Option<PendingMerge>,
    /// Original branch to return to if an into-selected merge is aborted. Set
    /// when that merge is dispatched, cleared at the next flow or on commit,
    /// and consumed on `AbortMerge` to check the original branch back out.
    pub merge_return: Option<String>,
    /// Discard awaiting confirmation: `Some(None)` = all unstaged changes,
    /// `Some(Some(path))` = one file. Same arm-then-confirm flow as checkout.
    pub pending_discard: Option<Option<String>>,
    /// Branch creation awaiting a name; `Some` shows the new-branch dialog.
    pub pending_branch: Option<NewBranch>,
    /// Set when a push/pull found the branch diverged from upstream; drives
    /// the reconcile dialog (merge / rebase / cancel). The job only fires on
    /// the user's choice -- the app never picks a strategy on its own.
    pub pending_sync: Option<PendingSync>,
    /// Set when a push found no `origin` remote; drives the add-origin dialog
    /// (URL prompt, then [`Job::AddOrigin`] on confirm).
    pub pending_remote: Option<PendingRemote>,
    /// Repo-relative path of a submodule the user asked to open as its own tab;
    /// drained after layout (it needs `&mut self`, not the borrowed tab).
    pub pending_open_submodule: Option<String>,
    /// Worktree-add modal state; `Some` shows the Worktree Add Flow modal.
    pub pending_worktree_add: Option<NewWorktree>,
    /// Worktree removal awaiting confirmation; the job only fires on confirm.
    pub pending_worktree_remove: Option<PendingWorktreeRemove>,
    /// Confirmed removal `(path, force)` to dispatch after layout: the
    /// target's tab (if open) must be closed first so the worker drops its
    /// repo handle before the delete, and that needs `&mut self`.
    pub worktree_remove_go: Option<(PathBuf, bool)>,
    /// Destination of a worktree-add in flight; on success it opens as a tab.
    pub opening_worktree: Option<PathBuf>,
    /// Jobs sent but not yet answered; > 0 shows the busy indicators.
    pub in_flight: usize,
    /// Last action outcome for the status bar: (is_error, message).
    pub log: Option<(bool, String)>,
    /// `Open` succeeded; gates refreshes and network actions.
    pub loaded: bool,
    /// A commit job is in flight -- blocks double-commits.
    pub committing: bool,
    /// Long-running network ops in flight (fetch/push/pull/submodules), newest
    /// last, each carrying its live step checklist and console tail. While
    /// non-empty the progress modal floats over the console so a multi-second
    /// remote round trip reads as work underway, not a hang. An entry is pushed
    /// in `GitGudApp::send`, fed by `Update::Progress`, and removed when the
    /// matching network reply lands -- a stack so overlapping ops keep the modal
    /// up until the last, with the newest shown.
    pub net_ops: Vec<OpProgress>,
    /// Set by the worktree watcher thread; the frame loop turns it into a
    /// `Refresh` once the tab is idle.
    fs_dirty: Arc<AtomicBool>,
    /// Set by the watcher when a git ref moved (`.git/HEAD`, `refs/**`,
    /// `packed-refs`) -- an external commit, checkout, fetch, or gc. Unlike
    /// `fs_dirty` this also reloads history, so the graph picks up commits
    /// made outside the app without re-walking on every worktree save.
    refs_dirty: Arc<AtomicBool>,
    /// Keeps the watcher's backend thread alive; dropped with the tab.
    watcher: Option<notify::RecommendedWatcher>,
    last_auto_refresh: Instant,
}

impl RepoTab {
    fn new(id: RepoId, name: String, path: PathBuf) -> Self {
        Self {
            id,
            name,
            path,
            head: String::new(),
            head_detached: false,
            branches: Vec::new(),
            collapsed_folders: HashSet::new(),
            tree_view: true,
            collapsed_file_folders: HashSet::new(),
            submodules: Vec::new(),
            worktrees: Vec::new(),
            staged: Vec::new(),
            unstaged: Vec::new(),
            commits: Vec::new(),
            graph: Graph::default(),
            head_only: HashSet::new(),
            focus_head: true,
            history_done: true,
            selection: None,
            diff: None,
            commit_diff: None,
            commit_file: None,
            merging: None,
            conflict: None,
            pending_abort_merge: false,
            commit_msg: String::new(),
            pending_checkout: None,
            pending_merge: None,
            merge_return: None,
            pending_discard: None,
            pending_branch: None,
            pending_sync: None,
            pending_remote: None,
            pending_open_submodule: None,
            pending_worktree_add: None,
            pending_worktree_remove: None,
            worktree_remove_go: None,
            opening_worktree: None,
            in_flight: 0,
            log: None,
            loaded: false,
            committing: false,
            net_ops: Vec::new(),
            fs_dirty: Arc::new(AtomicBool::new(false)),
            refs_dirty: Arc::new(AtomicBool::new(false)),
            watcher: None,
            last_auto_refresh: Instant::now(),
        }
    }

    /// Gate for the COMMIT button and Ctrl+Enter.
    pub fn can_commit(&self) -> bool {
        // Mid-merge an empty stage is fine: a merge resolved entirely to
        // HEAD's content stages nothing, yet the merge commit must happen.
        (!self.staged.is_empty() || self.merging.is_some())
            && !self.has_conflicts()
            && !self.commit_msg.trim().is_empty()
            && !self.committing
    }

    pub fn has_conflicts(&self) -> bool {
        self.unstaged
            .iter()
            .any(|f| f.kind == ChangeKind::Conflicted)
    }

    /// The uncommitted-changes gate for the checkout and merge flows: staged
    /// work, any non-untracked unstaged work, or a dirty submodule all block a
    /// switch. Untracked files don't -- git carries them across a checkout
    /// untouched, so blocking on them would surprise the user.
    pub fn has_blocking_changes(&self) -> bool {
        !self.staged.is_empty()
            || self
                .unstaged
                .iter()
                .any(|f| f.kind != ChangeKind::Untracked)
            || self.submodules.iter().any(|s| s.dirty)
    }
}

/// Live progress for one in-flight network op, built up from the worker's
/// `Update::Progress` stream and drawn by the progress modal. `op` is the
/// op label (matches what `net_label` pushed, so a reply can find its entry);
/// `steps` is the checklist in the order steps first appeared; `console` is a
/// bounded ring of the most recent subprocess output lines for the tail.
pub struct OpProgress {
    pub op: &'static str,
    pub steps: Vec<ProgressStep>,
    pub console: Vec<String>,
}

/// One checklist row in an [`OpProgress`]: a stable `key` to update it in
/// place, a display `label`, and its current [`StepState`].
pub struct ProgressStep {
    pub key: String,
    pub label: String,
    pub state: StepState,
}

/// How many recent console lines an op keeps. The modal shows only the last
/// couple, but a small backlog is held so the tail doesn't flicker empty
/// between bursts. Bounds memory against a chatty remote (or a verbose hook).
const CONSOLE_RING: usize = 64;

impl OpProgress {
    fn new(op: &'static str) -> Self {
        Self {
            op,
            steps: Vec::new(),
            console: Vec::new(),
        }
    }

    /// Folds one progress event in: a `Step` updates the matching row (or
    /// appends a new one), a `Console` line joins the ring, dropping the oldest
    /// past [`CONSOLE_RING`].
    fn apply(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::Step { key, label, state } => {
                if let Some(step) = self.steps.iter_mut().find(|s| s.key == key) {
                    step.state = state;
                } else {
                    self.steps.push(ProgressStep { key, label, state });
                }
            }
            ProgressEvent::Console(line) => {
                self.console.push(line);
                if self.console.len() > CONSOLE_RING {
                    let drop = self.console.len() - CONSOLE_RING;
                    self.console.drain(0..drop);
                }
            }
        }
    }
}

/// A push that found no `origin` remote, held while the add-origin dialog is
/// up. Carries the URL being typed; the job only fires on confirm -- the app
/// never invents a remote on its own.
pub struct PendingRemote {
    pub url: String,
    /// Set when the dialog opens so the URL field grabs focus once.
    pub needs_focus: bool,
}

/// A diverged push/pull awaiting the user's reconcile choice. Carries which
/// button opened the flow (so a push can follow the integrate) and the
/// ahead/behind counts that fill the dialog copy.
#[derive(Clone, Copy)]
pub struct PendingSync {
    pub kind: SyncKind,
    pub behind: usize,
    pub ahead: usize,
}

/// A merge started from the branch list, held while its modal is up. The modal
/// runs the preconditions (uncommitted changes / merge-in-progress / detached
/// HEAD) then offers the direction choice. `is_remote` hides the into-selected
/// direction (you can't merge into a remote ref); `discard` records that the
/// user waived the uncommitted-changes gate via Discard & continue, so the
/// eventual `Job::Merge` resets first.
#[derive(Clone)]
pub struct PendingMerge {
    pub branch: String,
    pub is_remote: bool,
    pub discard: bool,
}

/// Top-level state: the worker handle plus all open tabs. Every job must go
/// through `send` -- not `worker.send()` -- so the per-tab `in_flight`
/// counter (and with it every busy indicator) stays honest.
pub struct GitGudApp {
    worker: GitWorker,
    tabs: Vec<RepoTab>,
    active: usize,
    next_id: RepoId,
    /// Receives the folder picked on a background thread (rfd blocks).
    picker: Option<Receiver<Option<PathBuf>>>,
    /// Status line for errors that no longer have a tab (e.g. a failed open).
    app_log: Option<(bool, String)>,
    prev_focused: bool,
    /// Lines scrolled per mouse-wheel notch, from the OS where readable.
    wheel_lines: f32,
}

/// How many lines one wheel notch scrolls. winit reports raw notches without
/// applying the OS "lines per notch" setting, so we read it ourselves on
/// Windows (`SPI_GETWHEELSCROLLLINES`); elsewhere there is no per-app setting
/// to read. The 3-line convention felt sluggish in practice against our tight
/// row heights, so the fallback is a notch brisker.
fn wheel_scroll_lines() -> f32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETWHEELSCROLLLINES, SystemParametersInfoW,
        };
        let mut lines: u32 = 0;
        let ok = unsafe {
            SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, (&raw mut lines).cast(), 0)
        };
        // u32::MAX is WHEEL_PAGESCROLL ("scroll one screen"); treat anything
        // outside the settings UI's 1..=100 range as unconfigured.
        if ok != 0 && (1..=100).contains(&lines) {
            return lines as f32;
        }
    }
    5.0
}

impl GitGudApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut app = Self {
            worker: GitWorker::spawn(cc.egui_ctx.clone()),
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            picker: None,
            app_log: None,
            prev_focused: true,
            wheel_lines: wheel_scroll_lines(),
        };
        for path in Self::load_repo_list() {
            app.open_repo(path);
        }
        app.active = 0;
        app
    }

    fn open_repo(&mut self, path: PathBuf) {
        let id = self.next_id;
        self.next_id += 1;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.tabs.push(RepoTab::new(id, name, path.clone()));
        self.active = self.tabs.len() - 1;
        self.send(Job::Open { id, path });
    }

    fn close_tab(&mut self, i: usize) {
        let tab = self.tabs.remove(i);
        self.worker.send(Job::Close { id: tab.id });
        self.clamp_active();
        self.save_repo_list();
    }

    fn clamp_active(&mut self) {
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
    }

    /// Focus the tab already open at `path`, or open one. The worktree bar's
    /// jump action -- a worktree is a tab (ADR 0001).
    fn focus_or_open(&mut self, path: PathBuf) {
        if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
            self.active = i;
        } else {
            self.open_repo(path);
        }
    }

    /// Single send path so in-flight accounting can't be forgotten.
    fn send(&mut self, job: Job) {
        if !matches!(job, Job::Close { .. }) {
            let id = job_id(&job);
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.in_flight += 1;
                if matches!(job, Job::Commit { .. }) {
                    t.committing = true;
                }
                // A long-running network op opens the progress modal; the
                // matching reply pops it in `handle_update`, and
                // `Update::Progress` fills its checklist meanwhile.
                if let Some(label) = net_label(&job) {
                    t.net_ops.push(OpProgress::new(label));
                }
            }
        }
        self.worker.send(job);
    }

    fn spawn_picker(&mut self, ctx: &egui::Context) {
        if self.picker.is_some() {
            return; // dialog already open
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        // rfd blocks its thread, so it must not run on the UI thread.
        std::thread::spawn(move || {
            let _ = tx.send(rfd::FileDialog::new().pick_folder());
            ctx.request_repaint();
        });
        self.picker = Some(rx);
    }

    fn poll_picker(&mut self) {
        let Some(rx) = &self.picker else { return };
        match rx.try_recv() {
            Ok(Some(path)) => {
                self.picker = None;
                self.open_repo(path);
            }
            Ok(None) | Err(TryRecvError::Disconnected) => self.picker = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    fn repos_file() -> Option<PathBuf> {
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else {
            // XDG spec: relative $XDG_CONFIG_HOME must be ignored
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        };
        base.map(|d| d.join("gitgud").join("repos.txt"))
    }

    fn load_repo_list() -> Vec<PathBuf> {
        let Some(file) = Self::repos_file() else {
            return Vec::new();
        };
        std::fs::read_to_string(file)
            .map(|s| {
                s.lines()
                    .map(PathBuf::from)
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn save_repo_list(&self) {
        let Some(file) = Self::repos_file() else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let lines: String = self
            .tabs
            .iter()
            .map(|t| format!("{}\n", t.path.display()))
            .collect();
        let _ = std::fs::write(file, lines);
    }

    /// Watch the worktree (including `.git`, so external commits and branch
    /// switches count too). Events only set a flag and wake egui -- the actual
    /// refresh decision happens on the UI thread, where the debounce state is.
    fn spawn_watcher(tab: &mut RepoTab, ctx: &egui::Context) {
        let dirty = tab.fs_dirty.clone();
        let refs_dirty = tab.refs_dirty.clone();
        let git_dir = tab.path.join(".git");
        let ctx = ctx.clone();
        tab.watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Access events are reads; they can't change git status.
            if let Ok(ev) = res
                && !matches!(ev.kind, notify::EventKind::Access(_))
            {
                dirty.store(true, Ordering::Relaxed);
                if ev.paths.iter().any(|p| is_ref_change(&git_dir, p)) {
                    refs_dirty.store(true, Ordering::Relaxed);
                }
                // Delayed wake batches an event burst into one frame.
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        })
        .and_then(|mut w| {
            w.watch(&tab.path, notify::RecursiveMode::Recursive)?;
            Ok(w)
        })
        // On failure the tab silently falls back to F5/refocus refresh.
        .ok();
    }

    fn handle_update(&mut self, ctx: &egui::Context, upd: Update) {
        let id = update_id(&upd);
        let Some(idx) = self.tabs.iter().position(|t| t.id == id) else {
            // Tab already closed; only a failed open still needs reporting.
            if let Update::Opened { result: Err(e), .. } = upd {
                self.app_log = Some((true, format!("open failed: {e}")));
            }
            return;
        };
        // Progress is a mid-flight stream, not the job's terminal reply: it
        // doesn't answer the in-flight count or close the op. Fold it into the
        // matching op's checklist (newest entry with that label) and bail.
        if let Update::Progress { op, event, .. } = upd {
            if let Some(p) = self.tabs[idx].net_ops.iter_mut().rev().find(|p| p.op == op) {
                p.apply(event);
            }
            ctx.request_repaint();
            return;
        }

        self.tabs[idx].in_flight = self.tabs[idx].in_flight.saturating_sub(1);
        // A finished network op (or a divergence that hands off to the
        // reconcile modal) closes the progress modal; drain the matching stack
        // entry (the newest with that label, in case ops overlap).
        if let Some(op) = net_done_op(&upd)
            && let Some(pos) = self.tabs[idx].net_ops.iter().rposition(|p| p.op == op)
        {
            self.tabs[idx].net_ops.remove(pos);
        }

        match upd {
            Update::Opened {
                result: Ok(meta), ..
            } => {
                let t = &mut self.tabs[idx];
                t.name = meta.name;
                t.path = meta.workdir;
                t.loaded = true;
                Self::spawn_watcher(t, ctx);
                self.save_repo_list();
                self.send(Job::Refresh { id });
                self.send(Job::LoadHistory { id, skip: 0 });
            }
            Update::Opened { result: Err(e), .. } => {
                self.tabs.remove(idx);
                self.clamp_active();
                self.app_log = Some((true, format!("open failed: {e}")));
                self.save_repo_list();
            }
            Update::Refreshed { result, .. } => {
                let t = &mut self.tabs[idx];
                match result {
                    Ok(d) => {
                        t.head = d.head;
                        t.head_detached = d.head_detached;
                        t.branches = d.branches;
                        t.submodules = d.submodules;
                        t.worktrees = d.worktrees;
                        t.staged = d.staged;
                        t.unstaged = d.unstaged;
                        // Entering a merge: seed the commit box with git's
                        // suggested message, but never clobber user text.
                        if t.merging.is_none()
                            && let Some(m) = &d.merging
                            && t.commit_msg.trim().is_empty()
                        {
                            t.commit_msg = m.msg.clone();
                        }
                        // Merge ended (committed or aborted): a stale armed
                        // abort would fire against nothing, and an aborted
                        // merge's prefilled message must not linger for the
                        // next ordinary commit.
                        if let Some(m) = &t.merging
                            && d.merging.is_none()
                        {
                            t.pending_abort_merge = false;
                            if t.commit_msg.trim() == m.msg.trim() {
                                t.commit_msg.clear();
                            }
                        }
                        t.merging = d.merging;
                        // A refresh can remove the file we were diffing
                        // (e.g. it was staged) or resolve the conflict we
                        // were viewing -- drop the stale selection.
                        let stale = match &t.selection {
                            Some(DiffKey::Workdir { path, staged }) => {
                                let list = if *staged { &t.staged } else { &t.unstaged };
                                !list.iter().any(|f| f.path == *path)
                            }
                            Some(DiffKey::Conflict { path }) => !t
                                .unstaged
                                .iter()
                                .any(|f| f.path == *path && f.kind == ChangeKind::Conflicted),
                            _ => false,
                        };
                        if stale {
                            t.selection = None;
                            t.diff = None;
                            t.conflict = None;
                        }
                    }
                    Err(e) => t.log = Some((true, e)),
                }
            }
            Update::History { result, .. } => {
                let t = &mut self.tabs[idx];
                match result {
                    Ok(page) => {
                        if page.skip == 0 {
                            t.commits = page.commits;
                            t.head_only = page.head_only.into_iter().collect();
                        } else {
                            t.commits.extend(page.commits);
                        }
                        t.history_done = page.reached_end;
                        t.graph = crate::git::graph::compute(&t.commits);
                    }
                    Err(e) => t.log = Some((true, e)),
                }
            }
            Update::Diff { key, result, .. } => {
                let t = &mut self.tabs[idx];
                // Only accept the reply for the *current* selection.
                if t.selection.as_ref() == Some(&key) {
                    match result {
                        Ok(data) => match key {
                            // LoadDiff is never sent with a Conflict key.
                            DiffKey::Conflict { .. } => {}
                            DiffKey::Workdir { .. } => t.diff = Some(DiffView::build(data)),
                            DiffKey::Commit { .. } => {
                                t.commit_diff = Some(data);
                                // A file may already be open in the center
                                // (e.g. a re-sent diff): rebuild it from the
                                // fresh data, or drop it if the file is gone.
                                if let Some(path) = t.commit_file.clone() {
                                    t.diff =
                                        DiffView::build_one(t.commit_diff.as_ref().unwrap(), &path);
                                    if t.diff.is_none() {
                                        t.commit_file = None;
                                    }
                                }
                            }
                        },
                        Err(e) => t.log = Some((true, e)),
                    }
                }
            }
            Update::Conflict { path, result, .. } => {
                let t = &mut self.tabs[idx];
                // Only accept the reply for the *current* selection.
                if t.selection == Some(DiffKey::Conflict { path: path.clone() }) {
                    match result {
                        Ok(file) => t.conflict = Some(ConflictView::build(path, file)),
                        Err(e) => t.log = Some((true, e)),
                    }
                }
            }
            Update::Acted { kind, result, .. } => {
                // Scoped block: end the tab borrow before calling self.send.
                let (ok, restore, opened) = {
                    let t = &mut self.tabs[idx];
                    if kind == ActionKind::Commit {
                        t.committing = false;
                    }
                    // Taken win or lose, so a failed add can't leave a stale
                    // destination that a later add would then auto-open.
                    let opened = if kind == ActionKind::WorktreeAdd {
                        t.opening_worktree.take()
                    } else {
                        None
                    };
                    match result {
                        Ok(msg) => {
                            t.log = Some((false, msg));
                            if kind == ActionKind::Commit {
                                t.commit_msg.clear();
                                // Resolve-and-commit concluded an into-selected
                                // merge on the selected branch: no abort can
                                // follow, so the saved original is now inert.
                                t.merge_return = None;
                            }
                            // An into-selected merge that was aborted lands on
                            // the selected branch; restore the original branch
                            // (the working tree is clean after an abort).
                            let restore = if kind == ActionKind::AbortMerge {
                                t.merge_return.take()
                            } else {
                                None
                            };
                            (true, restore, opened)
                        }
                        Err(e) => {
                            t.log = Some((true, e));
                            (false, None, None)
                        }
                    }
                };
                if let Some(branch) = restore {
                    self.send(Job::Checkout {
                        id,
                        branch,
                        discard: false,
                    });
                }
                if ok {
                    self.send(Job::Refresh { id });
                    // Push moves the remote-tracking ref, so history decorations
                    // and ahead/behind counts both need recomputing. A clean
                    // merge creates a commit; an abort moves HEAD's state back.
                    // A worktree add can create a branch, moving refs too.
                    if matches!(
                        kind,
                        ActionKind::Commit
                            | ActionKind::Checkout
                            | ActionKind::CreateBranch
                            | ActionKind::Merge
                            | ActionKind::AbortMerge
                            | ActionKind::Fetch
                            | ActionKind::Push
                            | ActionKind::Pull
                            | ActionKind::WorktreeAdd
                    ) {
                        self.send(Job::LoadHistory { id, skip: 0 });
                    }
                    // A worktree op changed the family every sibling tab lists
                    // in its bar; their watchers can't see it (a linked
                    // worktree's `.git` is a file pointing elsewhere), so
                    // refresh them explicitly.
                    if matches!(
                        kind,
                        ActionKind::WorktreeAdd
                            | ActionKind::WorktreeRemove
                            | ActionKind::WorktreePrune
                    ) {
                        let family: Vec<PathBuf> = self.tabs[idx]
                            .worktrees
                            .iter()
                            .map(|w| w.path.clone())
                            .collect();
                        let siblings: Vec<RepoId> = self
                            .tabs
                            .iter()
                            .filter(|t| t.id != id && t.loaded && family.contains(&t.path))
                            .map(|t| t.id)
                            .collect();
                        for sid in siblings {
                            self.send(Job::Refresh { id: sid });
                        }
                    }
                    // A worktree is a tab (ADR 0001): entering the one just
                    // created means opening-and-focusing it.
                    if let Some(path) = opened {
                        self.open_repo(path);
                    }
                }
            }
            Update::Diverged {
                kind,
                behind,
                ahead,
                ..
            } => {
                // The push/pull probe came back needing a decision: arm the
                // reconcile dialog. The follow-up Integrate fires from there.
                self.tabs[idx].pending_sync = Some(PendingSync {
                    kind,
                    behind,
                    ahead,
                });
            }
            Update::NoOrigin { .. } => {
                // The push probe found nowhere to push: arm the add-origin
                // dialog. The follow-up AddOrigin fires from there.
                self.tabs[idx].pending_remote = Some(PendingRemote {
                    url: String::new(),
                    needs_focus: true,
                });
            }
            Update::WorktreeDirty { path, .. } => {
                // The unforced remove found uncommitted changes: escalate the
                // confirm modal to Discard & remove. The forced job only fires
                // on the user's say-so -- the app never force-removes alone.
                self.tabs[idx].pending_worktree_remove =
                    Some(PendingWorktreeRemove { path, dirty: true });
            }
            // Folded into the matching op above, then returned early.
            Update::Progress { .. } => unreachable!("Progress is handled before this match"),
        }
    }
}

fn job_id(job: &Job) -> RepoId {
    match job {
        Job::Open { id, .. }
        | Job::Close { id }
        | Job::Refresh { id }
        | Job::LoadHistory { id, .. }
        | Job::LoadDiff { id, .. }
        | Job::Stage { id, .. }
        | Job::Unstage { id, .. }
        | Job::Discard { id, .. }
        | Job::Commit { id, .. }
        | Job::Checkout { id, .. }
        | Job::CreateBranch { id, .. }
        | Job::Merge { id, .. }
        | Job::AbortMerge { id }
        | Job::LoadConflict { id, .. }
        | Job::ResolveConflict { id, .. }
        | Job::Fetch { id }
        | Job::Push { id }
        | Job::AddOrigin { id, .. }
        | Job::Pull { id }
        | Job::Integrate { id, .. }
        | Job::UpdateSubmodules { id, .. }
        | Job::WorktreeAdd { id, .. }
        | Job::WorktreeRemove { id, .. }
        | Job::WorktreePrune { id } => *id,
    }
}

/// The progress-modal label for a long-running network job, or `None` for a
/// local job (which is fast enough to need no modal). An integrate carries the
/// label of the button that began the flow so it reads as the same op.
fn net_label(job: &Job) -> Option<&'static str> {
    match job {
        Job::Fetch { .. } => Some("FETCH"),
        Job::Push { .. } | Job::AddOrigin { .. } => Some("PUSH"),
        Job::Pull { .. } => Some("PULL"),
        Job::Integrate { push_after, .. } => Some(if *push_after { "PUSH" } else { "PULL" }),
        Job::UpdateSubmodules { .. } => Some("SUBMODULES"),
        _ => None,
    }
}

/// Whether a watcher event path means a git ref moved: `.git/HEAD` (checkout),
/// anything under `.git/refs/` (commit, branch, fetch), or `.git/packed-refs`
/// (gc). Git updates refs by renaming a `.lock` file, so both the lock and the
/// final path land under these prefixes. Paths outside `.git` -- or inside a
/// gitdir that lives elsewhere (`.git` file) -- never match.
fn is_ref_change(git_dir: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(git_dir) else {
        return false;
    };
    matches!(
        rel.components().next().and_then(|c| c.as_os_str().to_str()),
        Some("HEAD" | "HEAD.lock" | "refs" | "packed-refs" | "packed-refs.lock")
    )
}

/// The op label a terminal update concludes, so its progress-modal entry can
/// be popped, or `None` for replies that aren't a network op's conclusion. A
/// `Diverged` reply ends the op too -- it hands the flow off to the reconcile
/// modal. The labels mirror `net_label` so the right `net_ops` entry is found.
fn net_done_op(upd: &Update) -> Option<&'static str> {
    match upd {
        Update::Acted { kind, .. } => match kind {
            ActionKind::Fetch => Some("FETCH"),
            ActionKind::Push => Some("PUSH"),
            ActionKind::Pull => Some("PULL"),
            ActionKind::UpdateSubmodules => Some("SUBMODULES"),
            _ => None,
        },
        Update::Diverged { kind, .. } => Some(match kind {
            SyncKind::Push => "PUSH",
            SyncKind::Pull => "PULL",
        }),
        // Hands the flow off to the add-origin dialog, ending the op the same
        // way a divergence does.
        Update::NoOrigin { .. } => Some("PUSH"),
        _ => None,
    }
}

fn update_id(upd: &Update) -> RepoId {
    match upd {
        Update::Opened { id, .. }
        | Update::Refreshed { id, .. }
        | Update::History { id, .. }
        | Update::Diff { id, .. }
        | Update::Conflict { id, .. }
        | Update::Acted { id, .. }
        | Update::Diverged { id, .. }
        | Update::NoOrigin { id }
        | Update::WorktreeDirty { id, .. }
        | Update::Progress { id, .. } => *id,
    }
}

impl eframe::App for GitGudApp {
    /// Smooth out trackpad scrolling. winit reports a high-resolution trackpad
    /// as `Line` wheel deltas with fractional counts, and egui then re-smooths
    /// every `Line` event (a ~0.1s exponential ramp) on top of the momentum the
    /// OS already applies. The two inertias fight: a scroll decelerates to a
    /// stop, then the OS momentum stream restarts it. We reclassify the
    /// high-resolution events -- a fractional line count, since a discrete mouse
    /// wheel only ever reports whole notches -- as already-smooth `Point`
    /// deltas, scaled by the same line speed so scroll distance and ctrl-scroll
    /// zoom are unchanged. Genuine wheel notches stay on the `Line` path, where
    /// egui's smoothing of the big discrete steps is wanted -- but scaled by
    /// the OS lines-per-notch setting ([`wheel_scroll_lines`]), which winit
    /// never applies, so wheel scrolling covers the same distance as in other
    /// applications. Zooming (the command modifier) is left unscaled: a notch
    /// is one zoom step regardless of scroll speed.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let line_px = ctx.options(|o| o.input_options.line_scroll_speed);
        for ev in &mut raw_input.events {
            if let egui::Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = ev
                && *unit == egui::MouseWheelUnit::Line
            {
                if delta.x.fract() != 0.0 || delta.y.fract() != 0.0 {
                    *delta *= line_px;
                    *unit = egui::MouseWheelUnit::Point;
                } else if !modifiers.command {
                    *delta *= self.wheel_lines;
                }
            }
        }
    }

    /// One frame: drain worker updates, translate input and panel clicks
    /// into jobs, lay out the HUD, send the collected jobs after layout.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for upd in self.worker.poll() {
            self.handle_update(&ctx, upd);
        }
        self.poll_picker();

        for file in ui.ctx().input(|i| i.raw.dropped_files.clone()) {
            if let Some(path) = file.path {
                self.open_repo(path);
            }
        }

        let (f5, ctrl_enter, ctrl_p, esc, focused) = ui.ctx().input(|i| {
            (
                i.key_pressed(Key::F5),
                i.modifiers.command && i.key_pressed(Key::Enter),
                i.modifiers.command && i.key_pressed(Key::P),
                i.key_pressed(Key::Escape),
                i.viewport().focused.unwrap_or(true),
            )
        });
        let regained = focused && !self.prev_focused;
        self.prev_focused = focused;

        // The gridded void every module floats over; panels are transparent.
        theme::backdrop(ui.painter(), ui.max_rect());

        // Panels borrow the active tab mutably, so clicks are collected here
        // and sent after layout (avoids a second &mut borrow of self).
        let mut jobs: Vec<Job> = Vec::new();
        if let Some(t) = self.tabs.get_mut(self.active) {
            if t.loaded && (f5 || regained) {
                jobs.push(Job::Refresh { id: t.id });
                // F5 always re-walks; a refocus only when refs moved while we
                // were away (external commit/checkout/fetch), so alt-tabbing
                // doesn't reset history paging for nothing.
                if f5 || t.refs_dirty.swap(false, Ordering::Relaxed) {
                    jobs.push(Job::LoadHistory { id: t.id, skip: 0 });
                }
                // A manual refresh covers whatever the watcher saw.
                t.fs_dirty.store(false, Ordering::Relaxed);
                t.refs_dirty.store(false, Ordering::Relaxed);
                t.last_auto_refresh = Instant::now();
            } else if t.loaded && t.in_flight == 0 && t.fs_dirty.load(Ordering::Relaxed) {
                let since = t.last_auto_refresh.elapsed();
                if since >= AUTO_REFRESH_GAP {
                    t.fs_dirty.store(false, Ordering::Relaxed);
                    t.last_auto_refresh = Instant::now();
                    jobs.push(Job::Refresh { id: t.id });
                    if t.refs_dirty.swap(false, Ordering::Relaxed) {
                        jobs.push(Job::LoadHistory { id: t.id, skip: 0 });
                    }
                } else {
                    // Too soon -- schedule a wake for when the gap expires.
                    ui.ctx().request_repaint_after(AUTO_REFRESH_GAP - since);
                }
            }
            if ctrl_enter && t.can_commit() {
                jobs.push(Job::Commit {
                    id: t.id,
                    message: t.commit_msg.clone(),
                });
            }
            // Same gate as the top-bar PUSH button.
            if ctrl_p && t.loaded {
                jobs.push(Job::Push { id: t.id });
            }
            // Escape backs out one level: open file -> graph, then selection
            // -> nothing. Skipped while a confirm modal is up, where Escape
            // already means "close the modal".
            if esc
                && t.pending_checkout.is_none()
                && t.pending_merge.is_none()
                && t.pending_discard.is_none()
                && t.pending_branch.is_none()
                && t.pending_sync.is_none()
                && t.pending_remote.is_none()
                && t.pending_worktree_add.is_none()
                && t.pending_worktree_remove.is_none()
                && t.net_ops.is_empty()
                && !t.pending_abort_merge
            {
                if t.commit_file.is_some() {
                    t.commit_file = None;
                    t.diff = None;
                } else if t.selection.is_some() {
                    t.selection = None;
                    t.diff = None;
                    t.commit_diff = None;
                    t.commit_file = None;
                    t.conflict = None;
                }
            }
        }

        let action = Panel::top("topbar")
            .exact_size(56.0)
            .frame(Frame::new().inner_margin(Margin::symmetric(10, 6)))
            .show_inside(ui, |ui| ui::top_bar(ui, &self.tabs, self.active))
            .inner;
        match action {
            Some(TopBarAction::Select(i)) => self.active = i,
            Some(TopBarAction::Close(i)) => self.close_tab(i),
            Some(TopBarAction::Open) => self.spawn_picker(ui.ctx()),
            Some(TopBarAction::Fetch) => {
                if let Some(t) = self.tabs.get(self.active) {
                    jobs.push(Job::Fetch { id: t.id });
                }
            }
            Some(TopBarAction::Push) => {
                if let Some(t) = self.tabs.get(self.active) {
                    jobs.push(Job::Push { id: t.id });
                }
            }
            Some(TopBarAction::Pull) => {
                if let Some(t) = self.tabs.get(self.active) {
                    jobs.push(Job::Pull { id: t.id });
                }
            }
            None => {}
        }

        // Worktree bar: a subheader strip under the command deck, shown only
        // when the repo has linked worktrees (main alone means no bar -- the
        // common single-worktree repo pays no vertical cost).
        let wt_action = match self.tabs.get(self.active) {
            Some(t) if t.loaded && t.worktrees.len() > 1 => {
                Panel::top("worktreebar")
                    .exact_size(34.0)
                    .frame(Frame::new().inner_margin(Margin::symmetric(10, 3)))
                    .show_inside(ui, |ui| ui::worktrees::bar(ui, t))
                    .inner
            }
            _ => None,
        };
        match wt_action {
            // A worktree is a tab (ADR 0001): jumping opens-or-focuses it.
            Some(ui::worktrees::BarAction::Jump(path)) => self.focus_or_open(path),
            Some(ui::worktrees::BarAction::Add) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.pending_worktree_add = Some(NewWorktree::new(None));
                }
            }
            Some(ui::worktrees::BarAction::Remove(path)) => {
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.pending_worktree_remove = Some(PendingWorktreeRemove { path, dirty: false });
                }
            }
            Some(ui::worktrees::BarAction::Prune) => {
                if let Some(t) = self.tabs.get(self.active) {
                    jobs.push(Job::WorktreePrune { id: t.id });
                }
            }
            None => {}
        }

        Panel::bottom("status")
            .exact_size(26.0)
            .frame(
                Frame::new()
                    .fill(BG1)
                    .inner_margin(Margin::symmetric(10, 0)),
            )
            .show_inside(ui, |ui| {
                ui::status_bar(ui, self.tabs.get(self.active), self.app_log.as_ref());
            });

        if self.tabs.is_empty() {
            let open = CentralPanel::default()
                .show_inside(ui, ui::empty_state)
                .inner;
            if open {
                self.spawn_picker(ui.ctx());
            }
        } else {
            let tab = &mut self.tabs[self.active];
            Panel::left("ops")
                .resizable(true)
                .default_size(300.0)
                .frame(Frame::new().inner_margin(Margin {
                    left: 10,
                    right: 4,
                    top: 2,
                    bottom: 8,
                }))
                .show_inside(ui, |ui| ui::sidebar::show(ui, tab, &mut jobs));
            Panel::right("worktree")
                .resizable(true)
                .default_size(340.0)
                .frame(Frame::new().inner_margin(Margin {
                    left: 4,
                    right: 10,
                    top: 2,
                    bottom: 8,
                }))
                .show_inside(ui, |ui| ui::worktree::show(ui, tab, &mut jobs));
            // Center: the graph by default; the diff viewer while a working-
            // tree file is open, or while a file of the selected commit is.
            enum Center {
                Graph,
                Diff,
                Conflict,
            }
            let center = match &tab.selection {
                Some(DiffKey::Workdir { .. }) => Center::Diff,
                Some(DiffKey::Commit { .. }) if tab.commit_file.is_some() => Center::Diff,
                Some(DiffKey::Conflict { .. }) => Center::Conflict,
                _ => Center::Graph,
            };
            CentralPanel::default()
                .frame(Frame::new().inner_margin(Margin {
                    left: 4,
                    right: 4,
                    top: 2,
                    bottom: 8,
                }))
                .show_inside(ui, |ui| match center {
                    Center::Diff => ui::diff::show(ui, tab),
                    Center::Conflict => ui::conflict::show(ui, tab, &mut jobs),
                    Center::Graph => ui::graph::show(ui, tab, &mut jobs),
                });
        }

        // Reconcile dialog for a diverged push/pull. Lives here, not in a
        // panel, because it belongs to no module -- it floats over the whole
        // console once a probe reports divergence.
        if let Some(t) = self.tabs.get_mut(self.active) {
            ui::sync::show(ui, t, &mut jobs);
            ui::sync::show_no_origin(ui, t, &mut jobs);
            ui::worktrees::modals(ui, t, &mut jobs);
        }

        // Progress modal for an in-flight network op (fetch/push/pull/
        // submodules). Floats over the console while `net_ops` is non-empty so
        // a slow remote round trip reads as work underway. Mutually exclusive
        // with the reconcile dialog: a divergence drains the op as it arms it.
        if let Some(t) = self.tabs.get(self.active) {
            ui::progress::show(ui, t);
        }

        // CRT scanlines over the whole frame, then the outer HUD bezel:
        // corner brackets framing the console, drawn last so they sit on top.
        theme::scanlines(ui.painter(), ui.max_rect());
        theme::corner_brackets(
            ui.painter(),
            ui.max_rect().shrink(2.0),
            16.0,
            Stroke::new(1.5_f32, ACCENT),
        );

        for job in jobs {
            self.send(job);
        }

        // Opening a submodule as a tab needs `&mut self` (it pushes a tab), so
        // it can't happen during layout while a tab is borrowed -- the panel
        // armed it and we act on it here. The path is repo-relative and was
        // confined worker-side before it ever reached the UI.
        let open = self
            .tabs
            .get_mut(self.active)
            .and_then(|t| t.pending_open_submodule.take());
        if let Some(rel) = open {
            let abs = self.tabs[self.active].path.join(rel);
            self.open_repo(abs);
        }

        // A confirmed worktree removal, armed by the modal. Runs here because
        // the target's tab (if open) must be closed first -- queued on the same
        // channel, the Close precedes the remove, so the worker drops its repo
        // handle before git deletes the directory (open handles would block
        // the delete on Windows).
        let go = self
            .tabs
            .get_mut(self.active)
            .and_then(|t| t.worktree_remove_go.take());
        if let Some((path, force)) = go {
            let id = self.tabs[self.active].id;
            if let Some(i) = self.tabs.iter().position(|t| t.path == path) {
                let closing = self.tabs.remove(i);
                self.worker.send(Job::Close { id: closing.id });
                if i < self.active {
                    self.active -= 1;
                } else {
                    self.clamp_active();
                }
                self.save_repo_list();
            }
            self.send(Job::WorktreeRemove { id, path, force });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_paths_are_ref_changes() {
        let git = PathBuf::from("/repo/.git");
        for p in [
            "/repo/.git/HEAD",
            "/repo/.git/HEAD.lock",
            "/repo/.git/packed-refs",
            "/repo/.git/packed-refs.lock",
            "/repo/.git/refs/heads/main",
            "/repo/.git/refs/heads/main.lock",
            "/repo/.git/refs/remotes/origin/main",
        ] {
            assert!(is_ref_change(&git, Path::new(p)), "{p}");
        }
    }

    #[test]
    fn non_ref_paths_are_not_ref_changes() {
        let git = PathBuf::from("/repo/.git");
        for p in [
            "/repo/src/main.rs",
            "/repo/HEAD",
            "/repo/.git/index",
            "/repo/.git/objects/ab/cdef",
            "/repo/.git/logs/HEAD",
            "/repo/.git/COMMIT_EDITMSG",
            "/other/.git/HEAD",
        ] {
            assert!(!is_ref_change(&git, Path::new(p)), "{p}");
        }
    }
}

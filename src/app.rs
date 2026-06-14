//! Application state: repo tabs, worker wiring, persistence, and the
//! per-frame loop that turns worker updates and UI clicks into jobs.
//!
//! The UI thread never touches `git2` -- repo work is a `Job` sent to the
//! worker and an `Update` handled when it comes back. Clicks made during
//! panel layout can't send directly (the active tab is borrowed mutably),
//! so they collect into a frame-local `Vec<Job>` flushed after layout.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use eframe::egui::{self, CentralPanel, Frame, Key, Margin, Panel, Stroke};
use notify::Watcher;

use crate::git::worker::GitWorker;
use crate::git::{
    ActionKind, BranchInfo, ChangeKind, CommitInfo, DiffData, DiffKey, FileChange, Job, MergeInfo,
    RepoId, SyncKind, Update, graph::Graph,
};
use crate::theme::{self, ACCENT, BG1};
use crate::ui::{self, TopBarAction, conflict::ConflictView, diff::DiffView};

/// Minimum gap between watcher-triggered refreshes. Bursty writers (saves,
/// builds) collapse into one status scan per window instead of one per event.
const AUTO_REFRESH_GAP: Duration = Duration::from_millis(800);

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
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub commits: Vec<CommitInfo>,
    /// Lane layout for `commits`; recomputed whenever `commits` changes.
    pub graph: Graph,
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
    /// Branch the user clicked but hasn't confirmed switching to yet. While
    /// set, the confirm dialog is shown; the checkout only fires on confirm.
    pub pending_checkout: Option<String>,
    /// Discard awaiting confirmation: `Some(None)` = all unstaged changes,
    /// `Some(Some(path))` = one file. Same arm-then-confirm flow as checkout.
    pub pending_discard: Option<Option<String>>,
    /// Set when a push/pull found the branch diverged from upstream; drives
    /// the reconcile dialog (merge / rebase / cancel). The job only fires on
    /// the user's choice -- the app never picks a strategy on its own.
    pub pending_sync: Option<PendingSync>,
    /// Jobs sent but not yet answered; > 0 shows the busy indicators.
    pub in_flight: usize,
    /// Last action outcome for the status bar: (is_error, message).
    pub log: Option<(bool, String)>,
    /// `Open` succeeded; gates refreshes and network actions.
    pub loaded: bool,
    /// A commit job is in flight -- blocks double-commits.
    pub committing: bool,
    /// Set by the worktree watcher thread; the frame loop turns it into a
    /// `Refresh` once the tab is idle.
    fs_dirty: Arc<AtomicBool>,
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
            staged: Vec::new(),
            unstaged: Vec::new(),
            commits: Vec::new(),
            graph: Graph::default(),
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
            pending_discard: None,
            pending_sync: None,
            in_flight: 0,
            log: None,
            loaded: false,
            committing: false,
            fs_dirty: Arc::new(AtomicBool::new(false)),
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

    /// Single send path so in-flight accounting can't be forgotten.
    fn send(&mut self, job: Job) {
        if !matches!(job, Job::Close { .. }) {
            let id = job_id(&job);
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.in_flight += 1;
                if matches!(job, Job::Commit { .. }) {
                    t.committing = true;
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
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("gitgud").join("repos.txt"))
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
        let ctx = ctx.clone();
        tab.watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // Access events are reads; they can't change git status.
            if let Ok(ev) = res
                && !matches!(ev.kind, notify::EventKind::Access(_))
            {
                dirty.store(true, Ordering::Relaxed);
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
        self.tabs[idx].in_flight = self.tabs[idx].in_flight.saturating_sub(1);

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
                let ok = {
                    let t = &mut self.tabs[idx];
                    if kind == ActionKind::Commit {
                        t.committing = false;
                    }
                    match result {
                        Ok(msg) => {
                            t.log = Some((false, msg));
                            if kind == ActionKind::Commit {
                                t.commit_msg.clear();
                            }
                            true
                        }
                        Err(e) => {
                            t.log = Some((true, e));
                            false
                        }
                    }
                };
                if ok {
                    self.send(Job::Refresh { id });
                    // Push moves the remote-tracking ref, so history decorations
                    // and ahead/behind counts both need recomputing. A clean
                    // merge creates a commit; an abort moves HEAD's state back.
                    if matches!(
                        kind,
                        ActionKind::Commit
                            | ActionKind::Checkout
                            | ActionKind::Merge
                            | ActionKind::AbortMerge
                            | ActionKind::Fetch
                            | ActionKind::Push
                            | ActionKind::Pull
                    ) {
                        self.send(Job::LoadHistory { id, skip: 0 });
                    }
                }
            }
            Update::Diverged {
                kind, behind, ahead, ..
            } => {
                // The push/pull probe came back needing a decision: arm the
                // reconcile dialog. The follow-up Integrate fires from there.
                self.tabs[idx].pending_sync = Some(PendingSync { kind, behind, ahead });
            }
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
        | Job::Merge { id, .. }
        | Job::AbortMerge { id }
        | Job::LoadConflict { id, .. }
        | Job::ResolveConflict { id, .. }
        | Job::Fetch { id }
        | Job::Push { id }
        | Job::Pull { id }
        | Job::Integrate { id, .. } => *id,
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
        | Update::Diverged { id, .. } => *id,
    }
}

impl eframe::App for GitGudApp {
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
                if f5 {
                    jobs.push(Job::LoadHistory { id: t.id, skip: 0 });
                }
                // A manual refresh covers whatever the watcher saw.
                t.fs_dirty.store(false, Ordering::Relaxed);
                t.last_auto_refresh = Instant::now();
            } else if t.loaded && t.in_flight == 0 && t.fs_dirty.load(Ordering::Relaxed) {
                let since = t.last_auto_refresh.elapsed();
                if since >= AUTO_REFRESH_GAP {
                    t.fs_dirty.store(false, Ordering::Relaxed);
                    t.last_auto_refresh = Instant::now();
                    jobs.push(Job::Refresh { id: t.id });
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
                && t.pending_discard.is_none()
                && t.pending_sync.is_none()
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
                .default_size(240.0)
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
        }

        // CRT scanlines over the whole frame, then the outer HUD bezel:
        // corner brackets framing the console, drawn last so they sit on top.
        theme::scanlines(ui.painter(), ui.max_rect());
        theme::corner_brackets(
            ui.painter(),
            ui.max_rect().shrink(2.0),
            16.0,
            Stroke::new(1.5, ACCENT),
        );

        for job in jobs {
            self.send(job);
        }
    }
}

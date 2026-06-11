//! Application state: repo tabs, worker wiring, persistence, and the
//! per-frame loop that turns worker updates and UI clicks into jobs.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use eframe::egui::{self, CentralPanel, Frame, Key, Margin, Panel, Stroke};

use crate::git::worker::GitWorker;
use crate::git::{ActionKind, BranchInfo, CommitInfo, DiffKey, FileChange, Job, RepoId, Update};
use crate::theme::{self, ACCENT, BG1};
use crate::ui::{self, TopBarAction, diff::DiffView};

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
    pub history_done: bool,
    pub selection: Option<DiffKey>,
    pub diff: Option<DiffView>,
    pub commit_msg: String,
    /// Branch the user clicked but hasn't confirmed switching to yet. While
    /// set, the confirm dialog is shown; the checkout only fires on confirm.
    pub pending_checkout: Option<String>,
    /// Discard awaiting confirmation: `Some(None)` = all unstaged changes,
    /// `Some(Some(path))` = one file. Same arm-then-confirm flow as checkout.
    pub pending_discard: Option<Option<String>>,
    /// Jobs sent but not yet answered; > 0 shows the busy indicators.
    pub in_flight: usize,
    /// Last action outcome for the status bar: (is_error, message).
    pub log: Option<(bool, String)>,
    pub loaded: bool,
    /// A commit job is in flight -- blocks double-commits.
    pub committing: bool,
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
            history_done: true,
            selection: None,
            diff: None,
            commit_msg: String::new(),
            pending_checkout: None,
            pending_discard: None,
            in_flight: 0,
            log: None,
            loaded: false,
            committing: false,
        }
    }

    pub fn can_commit(&self) -> bool {
        !self.staged.is_empty() && !self.commit_msg.trim().is_empty() && !self.committing
    }
}

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

    fn handle_update(&mut self, upd: Update) {
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
                        // A refresh can remove the file we were diffing
                        // (e.g. it was staged) -- drop the stale selection.
                        let stale = match &t.selection {
                            Some(DiffKey::Workdir { path, staged }) => {
                                let list = if *staged { &t.staged } else { &t.unstaged };
                                !list.iter().any(|f| f.path == *path)
                            }
                            _ => false,
                        };
                        if stale {
                            t.selection = None;
                            t.diff = None;
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
                    }
                    Err(e) => t.log = Some((true, e)),
                }
            }
            Update::Diff { key, result, .. } => {
                let t = &mut self.tabs[idx];
                // Only accept the reply for the *current* selection.
                if t.selection.as_ref() == Some(&key) {
                    match result {
                        Ok(data) => t.diff = Some(DiffView::build(data)),
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
                    // and ahead/behind counts both need recomputing.
                    if matches!(
                        kind,
                        ActionKind::Commit
                            | ActionKind::Checkout
                            | ActionKind::Fetch
                            | ActionKind::Push
                    ) {
                        self.send(Job::LoadHistory { id, skip: 0 });
                    }
                }
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
        | Job::Fetch { id }
        | Job::Push { id } => *id,
    }
}

fn update_id(upd: &Update) -> RepoId {
    match upd {
        Update::Opened { id, .. }
        | Update::Refreshed { id, .. }
        | Update::History { id, .. }
        | Update::Diff { id, .. }
        | Update::Acted { id, .. } => *id,
    }
}

impl eframe::App for GitGudApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        for upd in self.worker.poll() {
            self.handle_update(upd);
        }
        self.poll_picker();

        for file in ui.ctx().input(|i| i.raw.dropped_files.clone()) {
            if let Some(path) = file.path {
                self.open_repo(path);
            }
        }

        let (f5, ctrl_enter, ctrl_p, focused) = ui.ctx().input(|i| {
            (
                i.key_pressed(Key::F5),
                i.modifiers.command && i.key_pressed(Key::Enter),
                i.modifiers.command && i.key_pressed(Key::P),
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
        if let Some(t) = self.tabs.get(self.active) {
            if t.loaded && (f5 || regained) {
                jobs.push(Job::Refresh { id: t.id });
                if f5 {
                    jobs.push(Job::LoadHistory { id: t.id, skip: 0 });
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
                .show_inside(ui, |ui| ui::empty_state(ui))
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
            Panel::left("history")
                .resizable(true)
                .default_size(430.0)
                .frame(Frame::new().inner_margin(Margin {
                    left: 4,
                    right: 4,
                    top: 2,
                    bottom: 8,
                }))
                .show_inside(ui, |ui| ui::history::show(ui, tab, &mut jobs));
            CentralPanel::default()
                .frame(Frame::new().inner_margin(Margin {
                    left: 4,
                    right: 10,
                    top: 2,
                    bottom: 8,
                }))
                .show_inside(ui, |ui| ui::diff::show(ui, tab));
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

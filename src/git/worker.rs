//! The background thread that owns all `git2::Repository` handles.
//!
//! `Repository` is `Send` but not `Sync`: Rust lets us *move* it to one
//! thread but forbids sharing it across threads. So the worker owns every
//! repo in a HashMap and the UI only ever sees the plain-data snapshots
//! defined in `mod.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use git2::Repository;

use super::{ActionKind, Job, RepoId, SyncKind, Update, ops};

/// The UI-side handle to the worker: a job sender and an update receiver.
/// Dropping it closes the job channel, which is the worker's shutdown signal.
pub struct GitWorker {
    jobs: Sender<Job>,
    updates: Receiver<Update>,
}

impl GitWorker {
    /// Spawns the worker thread. It holds a clone of the egui `Context` so it
    /// can wake the UI (`request_repaint`) whenever a result is ready --
    /// otherwise egui only repaints on user input.
    pub fn spawn(egui_ctx: eframe::egui::Context) -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (upd_tx, upd_rx) = channel::<Update>();

        std::thread::spawn(move || {
            let mut repos: HashMap<RepoId, (Repository, PathBuf)> = HashMap::new();
            // `recv` blocks until a job arrives; errors only when the UI side
            // is dropped, which is our shutdown signal.
            while let Ok(job) = job_rx.recv() {
                let update = run_job(job, &mut repos, &upd_tx, &egui_ctx);
                if let Some(update) = update {
                    if upd_tx.send(update).is_err() {
                        break;
                    }
                    egui_ctx.request_repaint();
                }
            }
        });

        Self {
            jobs: job_tx,
            updates: upd_rx,
        }
    }

    /// Queues a job; fire-and-forget, the reply arrives via [`poll`]. App
    /// code goes through `GitGudApp::send` instead, which wraps this to
    /// count in-flight jobs for the busy indicator.
    ///
    /// [`poll`]: GitWorker::poll
    pub fn send(&self, job: Job) {
        // Ignoring the error is fine: it only fails if the worker died,
        // and then there is nobody left to report to.
        let _ = self.jobs.send(job);
    }

    /// Non-blocking drain of every pending update. Called once per frame.
    pub fn poll(&self) -> Vec<Update> {
        self.updates.try_iter().collect()
    }
}

/// Executes one job. `None` means no reply from here: `Close` never replies,
/// network jobs (fetch/push/pull) reply later from their own thread via
/// `upd_tx`, and a job for an unknown id (tab already closed) is dropped.
fn run_job(
    job: Job,
    repos: &mut HashMap<RepoId, (Repository, PathBuf)>,
    upd_tx: &Sender<Update>,
    egui_ctx: &eframe::egui::Context,
) -> Option<Update> {
    match job {
        Job::Open { id, path } => Some(match ops::open(&path) {
            Ok((repo, meta)) => {
                repos.insert(id, (repo, meta.workdir.clone()));
                Update::Opened {
                    id,
                    result: Ok(meta),
                }
            }
            Err(e) => Update::Opened { id, result: Err(e) },
        }),
        Job::Close { id } => {
            repos.remove(&id);
            None
        }
        Job::Refresh { id } => {
            let (repo, _) = repos.get(&id)?;
            Some(Update::Refreshed {
                id,
                result: ops::refresh(repo),
            })
        }
        Job::LoadHistory { id, skip } => {
            let (repo, _) = repos.get(&id)?;
            Some(Update::History {
                id,
                result: ops::history(repo, skip, super::HISTORY_PAGE),
            })
        }
        Job::LoadDiff { id, key } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::diff(repo, &key);
            Some(Update::Diff { id, key, result })
        }
        Job::Stage { id, path } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::stage(repo, path.as_deref());
            Some(Update::Acted {
                id,
                kind: ActionKind::Stage,
                result,
            })
        }
        Job::Unstage { id, path } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::unstage(repo, path.as_deref());
            Some(Update::Acted {
                id,
                kind: ActionKind::Unstage,
                result,
            })
        }
        Job::Discard { id, path } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::discard(repo, path.as_deref());
            Some(Update::Acted {
                id,
                kind: ActionKind::Discard,
                result,
            })
        }
        Job::Commit { id, message } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::commit(repo, &message);
            Some(Update::Acted {
                id,
                kind: ActionKind::Commit,
                result,
            })
        }
        Job::Checkout { id, branch } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::checkout(repo, &branch);
            Some(Update::Acted {
                id,
                kind: ActionKind::Checkout,
                result,
            })
        }
        Job::Merge { id, branch } => {
            // Local but shells out to the CLI so the user's merge config,
            // strategies, and hooks all apply.
            let (_, workdir) = repos.get(&id)?;
            Some(Update::Acted {
                id,
                kind: ActionKind::Merge,
                result: ops::merge(workdir, &branch),
            })
        }
        Job::AbortMerge { id } => {
            let (_, workdir) = repos.get(&id)?;
            Some(Update::Acted {
                id,
                kind: ActionKind::AbortMerge,
                result: ops::abort_merge(workdir),
            })
        }
        Job::LoadConflict { id, path } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::load_conflict(repo, &path);
            Some(Update::Conflict { id, path, result })
        }
        Job::ResolveConflict {
            id,
            path,
            resolution,
        } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::resolve(repo, &path, &resolution);
            Some(Update::Acted {
                id,
                kind: ActionKind::Resolve,
                result,
            })
        }
        Job::Fetch { id } => {
            // Network can take seconds; run it on its own thread so local
            // jobs (stage, refresh) are not stuck behind it in the queue.
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let result = ops::fetch(&workdir);
                let _ = tx.send(Update::Acted {
                    id,
                    kind: ActionKind::Fetch,
                    result,
                });
                ctx.request_repaint();
            });
            None
        }
        Job::Push { id } => {
            // Same off-thread treatment as fetch: a network push must not
            // block local jobs queued behind it. The probe either completes
            // the push or reports divergence for the UI to resolve.
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(sync_update(id, SyncKind::Push, ops::push(&workdir)));
                ctx.request_repaint();
            });
            None
        }
        Job::Pull { id } => {
            // Network, same off-thread treatment as fetch and push.
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(sync_update(id, SyncKind::Pull, ops::pull(&workdir)));
                ctx.request_repaint();
            });
            None
        }
        Job::Integrate {
            id,
            strategy,
            push_after,
        } => {
            // Network, same off-thread treatment. The reply is keyed as the
            // op the flow began as so the UI re-loads history either way.
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let result = ops::integrate(&workdir, strategy, push_after);
                let kind = if push_after {
                    ActionKind::Push
                } else {
                    ActionKind::Pull
                };
                let _ = tx.send(Update::Acted { id, kind, result });
                ctx.request_repaint();
            });
            None
        }
    }
}

/// Maps a push/pull probe's [`SyncOutcome`] onto the right `Update`: a
/// finished op becomes `Acted`, a divergence becomes the `Diverged` prompt.
fn sync_update(id: RepoId, kind: SyncKind, outcome: ops::SyncOutcome) -> Update {
    match outcome {
        ops::SyncOutcome::Completed(result) => Update::Acted {
            id,
            kind: match kind {
                SyncKind::Push => ActionKind::Push,
                SyncKind::Pull => ActionKind::Pull,
            },
            result,
        },
        ops::SyncOutcome::Diverged { behind, ahead } => Update::Diverged {
            id,
            kind,
            behind,
            ahead,
        },
    }
}

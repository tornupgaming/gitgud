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

use super::{ActionKind, Job, RepoId, Update, ops};

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
            // block local jobs queued behind it.
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let result = ops::push(&workdir);
                let _ = tx.send(Update::Acted {
                    id,
                    kind: ActionKind::Push,
                    result,
                });
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
                let result = ops::pull(&workdir);
                let _ = tx.send(Update::Acted {
                    id,
                    kind: ActionKind::Pull,
                    result,
                });
                ctx.request_repaint();
            });
            None
        }
    }
}

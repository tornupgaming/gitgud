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
        Job::Checkout {
            id,
            branch,
            discard,
        } => {
            let (repo, workdir) = repos.get(&id)?;
            // Discard & continue: reset first so the switch lands atomically.
            // A failed reset never checks out -- one job, one Update.
            let result = if discard {
                ops::reset_hard(workdir).and_then(|()| ops::checkout(repo, &branch))
            } else {
                ops::checkout(repo, &branch)
            };
            Some(Update::Acted {
                id,
                kind: ActionKind::Checkout,
                result,
            })
        }
        Job::CreateBranch {
            id,
            name,
            start_point,
            checkout,
        } => {
            let (repo, _) = repos.get(&id)?;
            let result = ops::create_branch(repo, &name, &start_point, checkout);
            Some(Update::Acted {
                id,
                kind: ActionKind::CreateBranch,
                result,
            })
        }
        Job::Merge {
            id,
            branch,
            direction,
            discard,
        } => {
            // Local but shells out to the CLI so the user's merge config,
            // strategies, and hooks all apply. Discard & continue resets first,
            // same atomic treatment as checkout.
            let (_, workdir) = repos.get(&id)?;
            let result = if discard {
                ops::reset_hard(workdir).and_then(|()| ops::merge(workdir, &branch, direction))
            } else {
                ops::merge(workdir, &branch, direction)
            };
            Some(Update::Acted {
                id,
                kind: ActionKind::Merge,
                result,
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
            let (repo, workdir) = repos.get(&id)?;
            // Enumerate submodules here, in-process via libgit2 while we still
            // hold the repo handle -- fast, and it lets `ops::fetch` show every
            // submodule row at once instead of stalling on `git submodule status`.
            let subs = ops::initialized_submodule_paths(repo);
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let mut report = reporter(&tx, &ctx, id, "FETCH");
                let result = ops::fetch(&workdir, &subs, &mut report);
                drop(report);
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
                let mut report = reporter(&tx, &ctx, id, "PUSH");
                let outcome = ops::push(&workdir, &mut report);
                drop(report);
                let _ = tx.send(sync_update(id, SyncKind::Push, outcome));
                ctx.request_repaint();
            });
            None
        }
        Job::AddOrigin { id, url } => {
            // The confirm action of the no-origin dialog: it ends in a push,
            // so it gets the same off-thread treatment (and PUSH label).
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let mut report = reporter(&tx, &ctx, id, "PUSH");
                let result = ops::add_origin_and_push(&workdir, &url, &mut report);
                drop(report);
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
                let mut report = reporter(&tx, &ctx, id, "PULL");
                let outcome = ops::pull(&workdir, &mut report);
                drop(report);
                let _ = tx.send(sync_update(id, SyncKind::Pull, outcome));
                ctx.request_repaint();
            });
            None
        }
        Job::UpdateSubmodules { id, path, init } => {
            // May fetch over the network (clone/checkout of nested submodules),
            // so it gets the same throwaway-thread treatment as fetch/pull.
            let (_, workdir) = repos.get(&id)?;
            let workdir = workdir.clone();
            let tx = upd_tx.clone();
            let ctx = egui_ctx.clone();
            std::thread::spawn(move || {
                let mut report = reporter(&tx, &ctx, id, "SUBMODULES");
                let result = ops::update_submodules(&workdir, path.as_deref(), init, &mut report);
                drop(report);
                let _ = tx.send(Update::Acted {
                    id,
                    kind: ActionKind::UpdateSubmodules,
                    result,
                });
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
                // Report under the label of the op that began the flow so the
                // progress modal reads as the same PUSH/PULL throughout.
                let op = if push_after { "PUSH" } else { "PULL" };
                let mut report = reporter(&tx, &ctx, id, op);
                let result = ops::integrate(&workdir, strategy, push_after, &mut report);
                drop(report);
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

/// Builds the progress sink handed to a network op: each event it emits is
/// wrapped in an [`Update::Progress`] tagged with the op label and pushed to
/// the UI, then the UI is woken so the modal repaints mid-flight. Borrows the
/// thread's own `tx`/`ctx`, so it must be dropped before the final reply is
/// sent on the same `tx`.
fn reporter<'a>(
    tx: &'a Sender<Update>,
    ctx: &'a eframe::egui::Context,
    id: RepoId,
    op: &'static str,
) -> impl FnMut(super::ProgressEvent) + 'a {
    move |event| {
        let _ = tx.send(Update::Progress { id, op, event });
        ctx.request_repaint();
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
        // Only the push probe reports this; pull without an upstream errors
        // before any remote lookup.
        ops::SyncOutcome::NoOrigin => Update::NoOrigin { id },
    }
}

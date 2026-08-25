//! The worktree bar and its flow modals.
//!
//! A worktree is a tab (ADR 0001): the bar is pure navigation. It lists every
//! worktree of the active tab's repository -- main first, the one you're
//! standing in highlighted -- and jumping opens-or-focuses that worktree's
//! tab. It only appears once a linked worktree exists; the first one is born
//! from the sidebar's "Open in new worktree…" branch action.
//!
//! The Worktree Add Flow: a modal with a branch (fixed by the sidebar entry,
//! or typed from the bar's ADD button -- an existing local branch checks out,
//! a new name creates a branch off HEAD) and an editable destination path,
//! pre-filled with the sibling-directory convention `../<repo>-<branch>`.
//!
//! The Worktree Remove Flow: arm-then-confirm; a dirty worktree escalates the
//! modal to Discard & remove (riding on a forced removal), the same shape as
//! the Checkout Flow's warning. The branch is never deleted.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{TryRecvError, channel};

use eframe::egui::{Align, Id, Key, Layout, RichText, Sense, TextEdit, Ui, vec2};

use super::HudButton;
use crate::app::{NewWorktree, RepoTab};
use crate::git::Job;
use crate::theme::{ACCENT, AMBER, DIM, RED};

/// What the user asked of the bar. `Jump` opens-or-focuses a worktree's tab;
/// `Prune` clears every stale record (git's prune is repo-wide).
pub enum BarAction {
    Jump(PathBuf),
    Add,
    Remove(PathBuf),
    Prune,
}

/// The worktree bar: one plate per worktree, branch-first (short commit id in
/// amber when detached), MAIN marker on the original checkout, the current one
/// highlighted, stale entries dimmed with prune-on-click. The caller only
/// shows the bar when the repository has linked worktrees.
pub fn bar(ui: &mut Ui, tab: &RepoTab) -> Option<BarAction> {
    let mut action = None;
    ui.horizontal_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(92.0, ui.available_height()), Sense::hover());
        super::sub_label(ui, r, "WORKTREES");
        for wt in &tab.worktrees {
            let current = wt.path == tab.path;
            if wt.is_main {
                ui.label(RichText::new("MAIN").small().color(DIM));
            }
            if wt.stale {
                // The directory is gone; nothing to jump into. The plate
                // itself becomes the prune action, so no extra button
                // clutters the bar.
                let resp = ui
                    .add(HudButton::new(&wt.branch).small(true).text_color(DIM))
                    .on_hover_text(format!(
                        "{}\ndirectory missing — click to prune stale worktrees",
                        wt.path.display()
                    ));
                if resp.clicked() {
                    action = Some(BarAction::Prune);
                }
                continue;
            }
            let mut button = HudButton::new(&wt.branch).small(true).accent(current);
            if wt.detached {
                button = button.text_color(AMBER);
            }
            let mut hover = wt.path.display().to_string();
            if wt.detached {
                hover.push_str("\ndetached HEAD");
            }
            if wt.locked {
                hover.push_str("\nlocked");
            }
            let resp = ui.add(button).on_hover_text(hover);
            if resp.clicked() && !current {
                action = Some(BarAction::Jump(wt.path.clone()));
            }
            // Main is never removable; the current worktree can't remove
            // itself (git refuses the working tree it runs in); a locked one
            // is under some other tool's protection.
            if !wt.is_main && !current && !wt.locked {
                super::hud_menu(&resp, |ui| {
                    if ui.button("Remove…").clicked() {
                        action = Some(BarAction::Remove(wt.path.clone()));
                        ui.close();
                    }
                });
            }
        }
        if ui
            .add(HudButton::new("+ ADD").small(true))
            .on_hover_text("check a branch out into a new worktree")
            .clicked()
        {
            action = Some(BarAction::Add);
        }
    });
    action
}

/// Both worktree flow modals; called once per frame alongside the other
/// floating dialogs.
pub fn modals(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    add_modal(ui, tab, jobs);
    remove_modal(ui, tab);
}

/// The Worktree Add Flow modal. The job (and the auto-open of the new tab)
/// only fires on CREATE.
fn add_modal(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let t = super::factor(
        ui.ctx(),
        Id::new("worktree_add"),
        tab.pending_worktree_add.is_some(),
    );
    if tab.pending_worktree_add.is_none() {
        return;
    }

    // Resolve the BROWSE picker and re-derive the path suggestion before the
    // body borrows the fields for its text edits.
    {
        let nw = tab.pending_worktree_add.as_mut().unwrap();
        if let Some(rx) = &nw.picker {
            match rx.try_recv() {
                Ok(Some(dir)) => {
                    // The picker chooses the parent directory; the leaf name
                    // (the worktree's own folder) is kept.
                    let leaf = Path::new(&nw.path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "worktree".to_string());
                    nw.path = dir.join(leaf).display().to_string();
                    nw.path_edited = true;
                    nw.picker = None;
                }
                Ok(None) | Err(TryRecvError::Disconnected) => nw.picker = None,
                Err(TryRecvError::Empty) => {}
            }
        }
    }

    // An existing local branch checks out; a new name creates a branch off
    // HEAD; a branch already checked out somewhere is refused (git allows one
    // checkout per branch, and the current worktree's branch is in the list).
    let branch = {
        let nw = tab.pending_worktree_add.as_ref().unwrap();
        nw.fixed_branch
            .clone()
            .unwrap_or_else(|| nw.branch.trim().to_string())
    };
    let exists = tab
        .branches
        .iter()
        .any(|b| !b.is_remote && b.name == branch);
    let occupied = tab
        .worktrees
        .iter()
        .any(|w| !w.detached && w.branch == branch);
    let create = !branch.is_empty() && !exists;
    if !tab.pending_worktree_add.as_ref().unwrap().path_edited {
        let suggestion = suggested_path(tab, &branch);
        tab.pending_worktree_add.as_mut().unwrap().path = suggestion;
    }
    let valid = !branch.is_empty()
        && !occupied
        && !tab
            .pending_worktree_add
            .as_ref()
            .unwrap()
            .path
            .trim()
            .is_empty();

    let ((confirm, cancel), closed) =
        super::hud_modal(ui, "worktree_add", "NEW WORKTREE", ACCENT, 380.0, t, |ui| {
            // `unwrap` is safe: the early return above guards `is_none`.
            let nw = tab.pending_worktree_add.as_mut().unwrap();
            let mut entered = false;
            if let Some(fixed) = &nw.fixed_branch {
                ui.label(format!("Open \"{fixed}\" in a new worktree:"));
            } else {
                ui.label("Branch:");
                ui.add_space(2.0);
                let field = ui.add(
                    TextEdit::singleline(&mut nw.branch)
                        .hint_text("existing branch to check out, or a new name")
                        .desired_width(f32::INFINITY),
                );
                if nw.needs_focus {
                    field.request_focus();
                    nw.needs_focus = false;
                }
                entered = valid && field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            }
            ui.add_space(4.0);
            let hint = if branch.is_empty() {
                RichText::new("A worktree is a second checkout in its own folder.")
                    .small()
                    .color(DIM)
            } else if occupied {
                RichText::new(format!(
                    "⚠ \"{branch}\" is already checked out in a worktree."
                ))
                .small()
                .color(AMBER)
            } else if exists {
                RichText::new(format!("Checks out the existing branch \"{branch}\"."))
                    .small()
                    .color(DIM)
            } else {
                RichText::new(format!("Creates a new branch \"{branch}\" off HEAD."))
                    .small()
                    .color(DIM)
            };
            ui.label(hint);
            ui.add_space(8.0);
            ui.label("Location:");
            ui.add_space(2.0);
            let browse = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let browse = ui.add(HudButton::new("BROWSE").small(true)).clicked();
                    let field = ui.add(
                        TextEdit::singleline(&mut nw.path).desired_width(ui.available_width()),
                    );
                    if field.changed() {
                        nw.path_edited = true;
                    }
                    browse
                })
                .inner;
            if browse && nw.picker.is_none() {
                let (tx, rx) = channel();
                let ctx = ui.ctx().clone();
                // rfd blocks its thread, so it must not run on the UI thread.
                std::thread::spawn(move || {
                    let _ = tx.send(rfd::FileDialog::new().pick_folder());
                    ctx.request_repaint();
                });
                nw.picker = Some(rx);
            }

            ui.add_space(14.0);
            let (confirm, cancel) = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let confirm = ui
                        .add_enabled(valid, HudButton::new("CREATE").accent(true))
                        .clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (confirm, cancel)
                })
                .inner;
            (confirm || entered, cancel)
        });

    if confirm {
        let nw = tab.pending_worktree_add.take().unwrap();
        let path = PathBuf::from(nw.path.trim());
        // The reply to this job auto-opens the new worktree as a tab.
        tab.opening_worktree = Some(path.clone());
        jobs.push(Job::WorktreeAdd {
            id: tab.id,
            path,
            branch,
            create_branch: create,
        });
    } else if cancel || closed {
        tab.pending_worktree_add = None;
    }
}

/// The Worktree Remove Flow modal: confirm when clean, Discard & remove when
/// git reported the worktree dirty. The confirmed removal is parked on
/// `worktree_remove_go` -- the app must close the target's tab before the job
/// is sent, which needs `&mut self`, not the borrowed tab.
fn remove_modal(ui: &mut Ui, tab: &mut RepoTab) {
    let t = super::factor(
        ui.ctx(),
        Id::new("worktree_remove"),
        tab.pending_worktree_remove.is_some(),
    );
    let Some(pr) = tab.pending_worktree_remove.clone() else {
        return;
    };

    let ((remove, cancel), closed) = super::hud_modal(
        ui,
        "worktree_remove",
        "REMOVE WORKTREE",
        ACCENT,
        340.0,
        t,
        |ui| {
            if pr.dirty {
                ui.label(format!(
                    "The worktree at \"{}\" has uncommitted changes.",
                    pr.path.display()
                ));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Discard them and remove it? Its files are deleted from \
                         disk; the branch is kept.",
                    )
                    .small()
                    .color(AMBER),
                );
            } else {
                ui.label(format!("Remove the worktree at \"{}\"?", pr.path.display()));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Its files are deleted from disk; the branch is kept.")
                        .small()
                        .color(DIM),
                );
            }
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if pr.dirty {
                    "DISCARD & REMOVE"
                } else {
                    "REMOVE"
                };
                let remove = ui.add(HudButton::new(label).text_color(RED)).clicked();
                let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                (remove, cancel)
            })
            .inner
        },
    );

    if remove {
        // Dirty means git already refused unforced; the retry rides on force.
        tab.worktree_remove_go = Some((pr.path, pr.dirty));
        tab.pending_worktree_remove = None;
    } else if cancel || closed {
        tab.pending_worktree_remove = None;
    }
}

/// Arms the Worktree Add Flow for the sidebar's "Open in new worktree…".
pub fn arm_add(tab: &mut RepoTab, branch: String) {
    tab.pending_worktree_add = Some(NewWorktree::new(Some(branch)));
}

/// A branch is openable in a new worktree when no worktree has it checked out
/// (git allows one checkout per branch; the current worktree's branch is in
/// the list, so HEAD is covered too).
pub fn branch_is_free(tab: &RepoTab, branch: &str) -> bool {
    !tab.worktrees
        .iter()
        .any(|w| !w.detached && w.branch == branch)
}

/// The sibling-directory suggestion for a new worktree: next to the *main*
/// worktree (so linked tabs suggest the same place), named
/// `<repo>-<sanitized-branch>`.
fn suggested_path(tab: &RepoTab, branch: &str) -> String {
    let main = tab
        .worktrees
        .first()
        .map(|w| w.path.clone())
        .unwrap_or_else(|| tab.path.clone());
    let repo = main
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let parent = main.parent().map(Path::to_path_buf).unwrap_or(main);
    parent
        .join(suggested_leaf(&repo, branch))
        .display()
        .to_string()
}

/// `<repo>-<branch>` with the branch name made filesystem-safe. Pure so it can
/// be tested without a `RepoTab`.
fn suggested_leaf(repo: &str, branch: &str) -> String {
    if branch.is_empty() {
        return format!("{repo}-worktree");
    }
    let safe: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{repo}-{safe}")
}

#[cfg(test)]
mod tests {
    use super::suggested_leaf;

    #[test]
    fn leaf_sanitizes_branch_separators_and_specials() {
        assert_eq!(suggested_leaf("gitgud", "feature/x"), "gitgud-feature-x");
        assert_eq!(suggested_leaf("gitgud", "wip: thing"), "gitgud-wip--thing");
        assert_eq!(suggested_leaf("gitgud", "v1.2_rc-3"), "gitgud-v1.2_rc-3");
    }

    #[test]
    fn leaf_for_empty_branch_falls_back() {
        assert_eq!(suggested_leaf("gitgud", ""), "gitgud-worktree");
    }
}

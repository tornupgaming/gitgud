//! The dialogs that resolve push/pull friction: the reconcile dialog shown
//! when a push or pull finds the branch diverged from its upstream, and the
//! add-origin dialog shown when a push finds no `origin` remote at all. The
//! worker never picks a strategy (or invents a remote) on its own -- it
//! reports the situation and these modals hand the choice to the user.

use eframe::egui::{Align, Id, Key, Layout, RichText, TextEdit, Ui};

use super::HudButton;
use crate::app::RepoTab;
use crate::git::{Job, SyncKind, SyncStrategy};
use crate::theme::{ACCENT, AMBER, DIM};

/// Renders the merge/rebase/cancel dialog while `tab.pending_sync` is set, and
/// turns the click into a [`Job::Integrate`] (or clears the arming on cancel).
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let t = super::factor(
        ui.ctx(),
        Id::new("reconcile_sync"),
        tab.pending_sync.is_some(),
    );
    let Some(sync) = tab.pending_sync else {
        return;
    };
    // Push must end with the commits on origin; pull just reconciles locally.
    let push_after = sync.kind == SyncKind::Push;

    let ((merge, rebase, cancel), closed) = super::hud_modal(
        ui,
        "reconcile_sync",
        "UPSTREAM DIVERGED",
        ACCENT,
        340.0,
        t,
        |ui| {
            // The friction itself: origin moved under you.
            ui.label(
                RichText::new(format!(
                    "⚠ origin has {} commit(s) on \"{}\" you don't have.",
                    sync.behind, tab.head
                ))
                .color(AMBER),
            );
            ui.add_space(2.0);
            let tail = if push_after {
                "before your push can land"
            } else {
                "to combine them"
            };
            ui.label(format!(
                "You're ahead by {}. Choose how to reconcile {tail}:",
                sync.ahead
            ));

            ui.add_space(10.0);
            // Spell out what each strategy does -- the whole point is to keep
            // the user in control rather than guessing for them.
            ui.label(
                RichText::new("MERGE — keep both histories, joined by a merge commit.")
                    .small()
                    .color(DIM),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new("REBASE — replay your commits on top, for a linear history.")
                    .small()
                    .color(DIM),
            );

            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let merge = ui.add(HudButton::new("MERGE").accent(true)).clicked();
                let rebase = ui.add(HudButton::new("REBASE")).clicked();
                let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                (merge, rebase, cancel)
            })
            .inner
        },
    );

    let strategy = if merge {
        Some(SyncStrategy::Merge)
    } else if rebase {
        Some(SyncStrategy::Rebase)
    } else {
        None
    };
    if let Some(strategy) = strategy {
        jobs.push(Job::Integrate {
            id: tab.id,
            strategy,
            push_after,
        });
        tab.pending_sync = None;
    } else if cancel || closed {
        tab.pending_sync = None;
    }
}

/// Renders the add-origin dialog while `tab.pending_remote` is set: a push
/// found no `origin` remote, so the user supplies a URL (or cancels) and the
/// confirm becomes a [`Job::AddOrigin`] -- add the remote, then push.
pub fn show_no_origin(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let t = super::factor(
        ui.ctx(),
        Id::new("add_origin"),
        tab.pending_remote.is_some(),
    );
    if tab.pending_remote.is_none() {
        return;
    }

    let ((confirm, cancel), closed) =
        super::hud_modal(ui, "add_origin", "NO REMOTE", ACCENT, 340.0, t, |ui| {
            // The friction itself: there is nowhere to push.
            ui.label(
                RichText::new(format!(
                    "⚠ no \"origin\" remote — \"{}\" has nowhere to push.",
                    tab.head
                ))
                .color(AMBER),
            );
            ui.add_space(2.0);
            ui.label("Enter a remote URL to add as origin and push to:");
            ui.add_space(6.0);
            // `unwrap` is safe: the early return above guards `is_none`.
            let pr = tab.pending_remote.as_mut().unwrap();
            let field = ui.add(
                TextEdit::singleline(&mut pr.url)
                    .hint_text("https://… or git@…")
                    .desired_width(f32::INFINITY),
            );
            // Grab focus the first frame the dialog is up so the user can
            // paste immediately.
            if pr.needs_focus {
                field.request_focus();
                pr.needs_focus = false;
            }
            // A leading '-' would read as an option to `git remote add`; the
            // worker rejects it too, this just keeps the button honest.
            let url = pr.url.trim();
            let valid = !url.is_empty() && !url.starts_with('-');
            let entered = valid && field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

            ui.add_space(14.0);
            let (confirm, cancel) = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let confirm = ui
                        .add_enabled(valid, HudButton::new("ADD + PUSH").accent(true))
                        .clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (confirm, cancel)
                })
                .inner;
            // Enter submits the primary (and only) action.
            (confirm || entered, cancel)
        });

    if confirm {
        let pr = tab.pending_remote.take().unwrap();
        jobs.push(Job::AddOrigin {
            id: tab.id,
            url: pr.url.trim().to_string(),
        });
    } else if cancel || closed {
        tab.pending_remote = None;
    }
}

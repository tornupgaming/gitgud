//! The reconcile dialog shown when a push or pull finds the branch diverged
//! from its upstream. The worker never picks a strategy on its own -- it
//! reports the divergence and this modal hands the choice to the user.

use eframe::egui::{Align, Id, Layout, RichText, Ui};

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

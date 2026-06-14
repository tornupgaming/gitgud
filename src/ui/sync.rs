//! The reconcile dialog shown when a push or pull finds the branch diverged
//! from its upstream. The worker never picks a strategy on its own -- it
//! reports the divergence and this modal hands the choice to the user.

use eframe::egui::{
    Align, Align2, Frame, Id, Layout, Margin, Modal, RichText, Sense, Stroke, Ui, pos2, vec2,
};

use super::HudButton;
use crate::app::RepoTab;
use crate::git::{Job, SyncKind, SyncStrategy};
use crate::theme::{self, ACCENT, AMBER, BG0, BG1, DIM};

/// Renders the merge/rebase/cancel dialog while `tab.pending_sync` is set, and
/// turns the click into a [`Job::Integrate`] (or clears the arming on cancel).
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let Some(sync) = tab.pending_sync else {
        return;
    };
    // Push must end with the commits on origin; pull just reconciles locally.
    let push_after = sync.kind == SyncKind::Push;

    let frame = Frame::new()
        .fill(BG1)
        .stroke(Stroke::new(1.0, ACCENT))
        .inner_margin(Margin::same(14));
    let modal = Modal::new(Id::new("reconcile_sync"))
        .frame(frame)
        .backdrop_color(theme::alpha(BG0, 215))
        .show(ui.ctx(), |ui| {
            ui.set_width(340.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            theme::tracked_text(
                ui.painter(),
                pos2(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                "UPSTREAM DIVERGED",
                theme::display_font(13.0),
                ACCENT,
                2.5,
            );
            let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
            theme::fade_rule(
                ui.painter(),
                pos2(rule.left(), rule.center().y),
                rule.width(),
                ACCENT,
            );
            ui.add_space(6.0);

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
            let buttons = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let merge = ui.add(HudButton::new("MERGE").accent(true)).clicked();
                    let rebase = ui.add(HudButton::new("REBASE")).clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (merge, rebase, cancel)
                })
                .inner;
            theme::corner_brackets(
                ui.painter(),
                ui.min_rect().expand(10.0),
                12.0,
                Stroke::new(1.5, ACCENT),
            );
            buttons
        });

    let (merge, rebase, cancel) = modal.inner;
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
    } else if cancel || modal.should_close() {
        tab.pending_sync = None;
    }
}

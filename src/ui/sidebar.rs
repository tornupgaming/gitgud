//! Left "ops" column: the BRANCHES module and its checkout confirm flow.

use eframe::egui::{
    Align, Align2, CornerRadius, Frame, Id, Layout, Margin, Modal, RichText, ScrollArea, Sense,
    Stroke, TextStyle, Ui, pos2, vec2,
};

use super::{HudButton, module};
use crate::app::RepoTab;
use crate::git::Job;
use crate::theme::{self, ACCENT, AMBER, BG0, BG1, BG3, GREEN, PURPLE, RED, TEXT};

/// The BRANCHES module plus its confirm modal. No branch click checks out
/// directly -- everything routes through the arm-then-confirm flow below.
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let readout = format!("{:02}", tab.branches.len());
    let clicked = module(ui, None, "BRANCHES", &readout, |ui| {
        ScrollArea::vertical()
            .id_salt("branches")
            .auto_shrink([false, false])
            .show(ui, |ui| branches(ui, tab))
            .inner
    });
    // A click only *arms* a switch; the checkout waits for confirmation.
    if let Some(name) = clicked {
        tab.pending_checkout = Some(name);
    }
    confirm_checkout(ui, tab, jobs);
}

/// Modal shown while a branch action is pending: switch to the armed branch
/// or merge it into HEAD. The job only fires on confirm; clicking away,
/// Escape, or Cancel aborts it. Both actions are invalid mid-merge.
fn confirm_checkout(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let Some(branch) = tab.pending_checkout.clone() else {
        return;
    };
    let dirty = !tab.staged.is_empty() || !tab.unstaged.is_empty();
    let mid_merge = tab.merging.is_some();

    // Angular HUD dialog: accent frame, dimmed backdrop, a tracked title and
    // corner brackets traced round the content.
    let frame = Frame::new()
        .fill(BG1)
        .stroke(Stroke::new(1.0, ACCENT))
        .inner_margin(Margin::same(14));
    let modal = Modal::new(Id::new("confirm_checkout"))
        .frame(frame)
        .backdrop_color(theme::alpha(BG0, 215))
        .show(ui.ctx(), |ui| {
            ui.set_width(300.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            theme::tracked_text(
                ui.painter(),
                pos2(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                "CONFIRM CHECKOUT",
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
            ui.label(format!(
                "Switch to \"{branch}\", or merge it into \"{}\"?",
                tab.head
            ));
            if mid_merge {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("⚠ Merge in progress — commit or abort it first.")
                        .small()
                        .color(AMBER),
                );
            } else if dirty {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("⚠ You have uncommitted changes that will move with you.")
                        .small()
                        .color(AMBER),
                );
            }
            ui.add_space(14.0);
            let buttons = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let switch = ui
                        .add_enabled(!mid_merge, HudButton::new("SWITCH").accent(true))
                        .clicked();
                    let merge = ui
                        .add_enabled(!mid_merge, HudButton::new("MERGE"))
                        .clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (switch, merge, cancel)
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

    let (switch, merge, cancel) = modal.inner;
    if switch {
        jobs.push(Job::Checkout { id: tab.id, branch });
        tab.pending_checkout = None;
    } else if merge {
        jobs.push(Job::Merge { id: tab.id, branch });
        tab.pending_checkout = None;
    } else if cancel || modal.should_close() {
        tab.pending_checkout = None;
    }
}

/// Draws the branch list and returns the name of a local branch the user
/// clicked, if any. The caller decides what to do with it (here: confirm).
fn branches(ui: &mut Ui, tab: &RepoTab) -> Option<String> {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;

    let mut clicked = None;
    for b in &tab.branches {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let p = ui.painter().with_clip_rect(rect);
        if b.is_head {
            // The checked-out branch reads as the "armed" row.
            p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(AMBER, 16));
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::ZERO, BG3);
        }
        let y = rect.center().y;
        if b.is_remote {
            // Hollow node marker, painted: Rajdhani has no ◦ glyph.
            p.circle_stroke(pos2(rect.left() + 9.0, y), 3.0, Stroke::new(1.0, PURPLE));
            p.text(
                pos2(rect.left() + 20.0, y),
                Align2::LEFT_CENTER,
                &b.name,
                font.clone(),
                PURPLE,
            );
        } else {
            if b.is_head {
                p.text(
                    pos2(rect.left() + 4.0, y),
                    Align2::LEFT_CENTER,
                    "▶",
                    font.clone(),
                    AMBER,
                );
            }
            p.text(
                pos2(rect.left() + 20.0, y),
                Align2::LEFT_CENTER,
                &b.name,
                font.clone(),
                if b.is_head { AMBER } else { TEXT },
            );
            let mut right = rect.right() - 4.0;
            if b.behind > 0 {
                let r = p.text(
                    pos2(right, y),
                    Align2::RIGHT_CENTER,
                    format!("↓{}", b.behind),
                    font.clone(),
                    RED,
                );
                right = r.left() - 6.0;
            }
            if b.ahead > 0 {
                p.text(
                    pos2(right, y),
                    Align2::RIGHT_CENTER,
                    format!("↑{}", b.ahead),
                    font.clone(),
                    GREEN,
                );
            }
        }
        if resp.clicked() && !b.is_head {
            clicked = Some(b.name.clone());
        }
    }
    clicked
}

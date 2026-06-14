//! Left "ops" column: the BRANCHES module and its checkout confirm flow.

use eframe::egui::{
    Align, Align2, CornerRadius, Frame, Id, Key, Layout, Margin, Modal, RichText, ScrollArea,
    Sense, Stroke, TextEdit, TextStyle, Ui, pos2, vec2,
};

use super::{HudButton, module};
use crate::app::{NewBranch, RepoTab};
use crate::git::Job;
use crate::theme::{self, ACCENT, AMBER, BG0, BG1, BG3, GREEN, PURPLE, RED, TEXT};

/// What the user asked of a branch row: a left-click arms a checkout; the
/// right-click menu arms a new branch from that row's tip.
enum BranchAction {
    Checkout(String),
    CreateFrom { start_point: String, label: String },
}

/// The BRANCHES module plus its confirm and new-branch modals. No branch
/// click checks out directly -- everything routes through the arm-then-confirm
/// flow below.
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let readout = format!("{:02}", tab.branches.len());
    let action = module(ui, None, "BRANCHES", &readout, |ui| {
        ScrollArea::vertical()
            .id_salt("branches")
            .auto_shrink([false, false])
            .show(ui, |ui| branches(ui, tab))
            .inner
    });
    // A click only *arms* an action; the job waits for confirmation.
    match action {
        Some(BranchAction::Checkout(name)) => tab.pending_checkout = Some(name),
        Some(BranchAction::CreateFrom { start_point, label }) => {
            tab.pending_branch = Some(NewBranch::new(start_point, label));
        }
        None => {}
    }
    confirm_checkout(ui, tab, jobs);
    confirm_new_branch(ui, tab, jobs);
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

/// Modal for "create branch from here": a name field plus CREATE (stay put)
/// and CREATE + SWITCH (also check it out). The job fires on confirm or Enter;
/// clicking away, Escape, or Cancel aborts. An empty name disables both.
fn confirm_new_branch(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    if tab.pending_branch.is_none() {
        return;
    }

    // Same angular HUD dialog as the checkout confirm.
    let frame = Frame::new()
        .fill(BG1)
        .stroke(Stroke::new(1.0, ACCENT))
        .inner_margin(Margin::same(14));
    let modal = Modal::new(Id::new("new_branch"))
        .frame(frame)
        .backdrop_color(theme::alpha(BG0, 215))
        .show(ui.ctx(), |ui| {
            ui.set_width(300.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            theme::tracked_text(
                ui.painter(),
                pos2(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                "NEW BRANCH",
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

            // `unwrap` is safe: the early return above guards `is_none`.
            let nb = tab.pending_branch.as_mut().unwrap();
            ui.label(format!("Branch from \"{}\":", nb.label));
            ui.add_space(6.0);
            let field = ui.add(
                TextEdit::singleline(&mut nb.name)
                    .hint_text("branch name")
                    .desired_width(f32::INFINITY),
            );
            // Grab focus the first frame the dialog is up so the user can type
            // immediately.
            if nb.needs_focus {
                field.request_focus();
                nb.needs_focus = false;
            }
            let valid = !nb.name.trim().is_empty();
            let entered = valid && field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));

            ui.add_space(14.0);
            let (create_switch, create, cancel) = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let switch = ui
                        .add_enabled(valid, HudButton::new("CREATE + SWITCH").accent(true))
                        .clicked();
                    let create = ui.add_enabled(valid, HudButton::new("CREATE")).clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (switch, create, cancel)
                })
                .inner;
            theme::corner_brackets(
                ui.painter(),
                ui.min_rect().expand(10.0),
                12.0,
                Stroke::new(1.5, ACCENT),
            );
            // Enter submits the primary action (create + switch).
            (create_switch || entered, create, cancel)
        });

    let (create_switch, create, cancel) = modal.inner;
    if create_switch || create {
        let nb = tab.pending_branch.take().unwrap();
        jobs.push(Job::CreateBranch {
            id: tab.id,
            name: nb.name.trim().to_string(),
            start_point: nb.start_point,
            checkout: create_switch,
        });
    } else if cancel || modal.should_close() {
        tab.pending_branch = None;
    }
}

/// Draws the branch list and returns the action the user asked for, if any:
/// a left-click on a non-HEAD branch arms a checkout, the right-click menu
/// arms a new branch from any row. The caller confirms before acting.
fn branches(ui: &mut Ui, tab: &RepoTab) -> Option<BranchAction> {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;

    let mut action = None;
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
            action = Some(BranchAction::Checkout(b.name.clone()));
        }
        // A branch's tip is a valid start point -- branch by name, which
        // revparse resolves for both local and remote branches.
        resp.context_menu(|ui| {
            if ui.button("Create branch from here").clicked() {
                action = Some(BranchAction::CreateFrom {
                    start_point: b.name.clone(),
                    label: b.name.clone(),
                });
                ui.close();
            }
        });
    }
    action
}

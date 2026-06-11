//! Left "ops" column: BRANCHES, WORKING TREE, and COMMIT modules.

use eframe::egui::{
    Align, Align2, CentralPanel, CornerRadius, Frame, Id, Layout, Margin, Modal, Panel, Rect,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, TextStyle, Ui, pos2, vec2,
};

use super::{HudButton, module, sub_label, truncate_left};
use crate::app::RepoTab;
use crate::git::{ChangeKind, DiffKey, Job};
use crate::theme::{self, ACCENT, AMBER, BG0, BG1, BG3, DIM, GREEN, PURPLE, RED, TEXT};

pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    // Commit module pinned to the bottom, the other two stacked above it.
    Panel::bottom("commit_box")
        .exact_size(186.0)
        .frame(Frame::new().inner_margin(Margin {
            left: 0,
            right: 0,
            top: 8,
            bottom: 0,
        }))
        .show_inside(ui, |ui| {
            let readout = format!("{:02} STAGED", tab.staged.len());
            module(ui, None, "COMMIT", &readout, |ui| commit_box(ui, tab, jobs));
        });
    CentralPanel::default()
        .frame(Frame::new())
        .show_inside(ui, |ui| {
            let branches_h = ui.available_height() * 0.40;
            let readout = format!("{:02}", tab.branches.len());
            let clicked = module(ui, Some(branches_h), "BRANCHES", &readout, |ui| {
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
            ui.add_space(8.0);
            let delta = tab.staged.len() + tab.unstaged.len();
            let readout = format!("{:02} Δ", delta);
            module(ui, None, "WORKING TREE", &readout, |ui| {
                changes(ui, tab, jobs)
            });
        });
    confirm_checkout(ui, tab, jobs);
    confirm_discard(ui, tab, jobs);
}

/// Modal for a pending discard. Destructive (untracked files are deleted, no
/// undo), so it gets the red treatment instead of the checkout dialog's accent.
fn confirm_discard(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let Some(target) = tab.pending_discard.clone() else {
        return;
    };
    let what = match &target {
        Some(path) => format!("Discard changes to \"{path}\"?"),
        None => format!("Discard all {} unstaged changes?", tab.unstaged.len()),
    };

    let frame = Frame::new()
        .fill(BG1)
        .stroke(Stroke::new(1.0, RED))
        .inner_margin(Margin::same(14));
    let modal = Modal::new(Id::new("confirm_discard"))
        .frame(frame)
        .backdrop_color(theme::alpha(BG0, 215))
        .show(ui.ctx(), |ui| {
            ui.set_width(300.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            theme::tracked_text(
                ui.painter(),
                pos2(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                "CONFIRM DISCARD",
                theme::display_font(13.0),
                RED,
                2.5,
            );
            let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
            theme::fade_rule(
                ui.painter(),
                pos2(rule.left(), rule.center().y),
                rule.width(),
                RED,
            );
            ui.add_space(6.0);
            ui.label(what);
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠ This cannot be undone. Untracked files will be deleted.")
                    .small()
                    .color(RED),
            );
            ui.add_space(14.0);
            let buttons = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let discard = ui.add(HudButton::new("DISCARD").text_color(RED)).clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (discard, cancel)
                })
                .inner;
            theme::corner_brackets(
                ui.painter(),
                ui.min_rect().expand(10.0),
                12.0,
                Stroke::new(1.5, RED),
            );
            buttons
        });

    let (discard, cancel) = modal.inner;
    if discard {
        jobs.push(Job::Discard {
            id: tab.id,
            path: target,
        });
        tab.pending_discard = None;
    } else if cancel || modal.should_close() {
        tab.pending_discard = None;
    }
}

/// Modal shown while a branch switch is pending. Returns the checkout job only
/// once the user confirms; clicking away, Escape, or Cancel aborts it.
fn confirm_checkout(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let Some(branch) = tab.pending_checkout.clone() else {
        return;
    };
    let dirty = !tab.staged.is_empty() || !tab.unstaged.is_empty();

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
            ui.label(format!("Check out \"{branch}\"?"));
            if dirty {
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
                    let switch = ui.add(HudButton::new("SWITCH").accent(true)).clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (switch, cancel)
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

    let (switch, cancel) = modal.inner;
    if switch {
        jobs.push(Job::Checkout { id: tab.id, branch });
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

fn changes(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    ScrollArea::vertical()
        .id_salt("changes")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let staged_label = format!("STAGED · {:02}", tab.staged.len());
            let any_staged = !tab.staged.is_empty();
            if list_header(ui, &staged_label, &[("UNSTAGE ALL", any_staged)]) == Some(0) {
                jobs.push(Job::Unstage {
                    id: tab.id,
                    path: None,
                });
            }
            file_rows(ui, tab, true, jobs);
            ui.add_space(10.0);
            let unstaged_label = format!("UNSTAGED · {:02}", tab.unstaged.len());
            let any_unstaged = !tab.unstaged.is_empty();
            let actions = [("STAGE ALL", any_unstaged), ("DISCARD ALL", any_unstaged)];
            match list_header(ui, &unstaged_label, &actions) {
                Some(0) => jobs.push(Job::Stage {
                    id: tab.id,
                    path: None,
                }),
                // Arms the confirm modal; the job fires there.
                Some(1) => tab.pending_discard = Some(None),
                _ => {}
            }
            file_rows(ui, tab, false, jobs);
        });
}

/// Section header with right-aligned action buttons (first = rightmost).
/// Returns the index of the clicked action, if any.
fn list_header(ui: &mut Ui, label: &str, actions: &[(&str, bool)]) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        let reserve = 110.0 * actions.len() as f32;
        let (rect, _) = ui.allocate_exact_size(
            vec2((ui.available_width() - reserve).max(40.0), 20.0),
            Sense::hover(),
        );
        sub_label(ui, rect, label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for (i, (action, enabled)) in actions.iter().enumerate() {
                let btn = HudButton::new(*action).small(true);
                if ui.add_enabled(*enabled, btn).clicked() {
                    clicked = Some(i);
                }
            }
        });
    });
    clicked
}

fn file_rows(ui: &mut Ui, tab: &mut RepoTab, staged: bool, jobs: &mut Vec<Job>) {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
    let btn_w = 16.0;

    // Record clicks during the walk and mutate `tab` after the loop -- we
    // can't set `tab.selection` while still borrowing `tab.staged`.
    let mut select: Option<DiffKey> = None;
    let mut toggle: Option<String> = None;
    let mut discard: Option<String> = None;

    let files = if staged { &tab.staged } else { &tab.unstaged };
    for f in files {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let key = DiffKey::Workdir {
            path: f.path.clone(),
            staged,
        };
        let selected = tab.selection.as_ref() == Some(&key);
        let p = ui.painter().with_clip_rect(rect);
        if selected {
            p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(ACCENT, 26));
            p.rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, theme::alpha(ACCENT, 150)),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::ZERO, BG3);
        }

        let y = rect.center().y;
        let glyph_color = match f.kind {
            ChangeKind::Added | ChangeKind::Untracked => GREEN,
            ChangeKind::Deleted | ChangeKind::Conflicted => RED,
            ChangeKind::Modified => AMBER,
            _ => DIM,
        };
        p.text(
            pos2(rect.left() + 4.0, y),
            Align2::LEFT_CENTER,
            f.kind.glyph(),
            font.clone(),
            glyph_color,
        );

        // Unstaged rows carry two buttons (discard + stage), staged rows one.
        let btn_count = if staged { 1.0 } else { 2.0 };
        let text_w = rect.width() - 20.0 - (btn_w + 4.0) * btn_count - 8.0;
        let path = truncate_left(ui, &f.path, &font, text_w);
        p.text(
            pos2(rect.left() + 20.0, y),
            Align2::LEFT_CENTER,
            path,
            font.clone(),
            TEXT,
        );

        let btn_rect = Rect::from_center_size(
            pos2(rect.right() - 4.0 - btn_w / 2.0, y),
            vec2(btn_w, btn_w),
        );
        let btn = ui.interact(btn_rect, resp.id.with("toggle"), Sense::click());
        if btn.hovered() {
            p.rect_filled(btn_rect, CornerRadius::ZERO, BG3);
            p.rect_stroke(
                btn_rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, ACCENT),
                StrokeKind::Inside,
            );
        }
        let (label, label_color) = if staged { ("−", RED) } else { ("+", GREEN) };
        p.text(
            btn_rect.center(),
            Align2::CENTER_CENTER,
            label,
            font.clone(),
            label_color,
        );

        let mut discard_clicked = false;
        if !staged {
            let x_rect = Rect::from_center_size(
                pos2(btn_rect.left() - 4.0 - btn_w / 2.0, y),
                vec2(btn_w, btn_w),
            );
            let x_btn = ui.interact(x_rect, resp.id.with("discard"), Sense::click());
            if x_btn.hovered() {
                p.rect_filled(x_rect, CornerRadius::ZERO, BG3);
                p.rect_stroke(
                    x_rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, RED),
                    StrokeKind::Inside,
                );
            }
            p.text(
                x_rect.center(),
                Align2::CENTER_CENTER,
                "✕",
                font.clone(),
                RED,
            );
            discard_clicked = x_btn.clicked();
        }

        if discard_clicked {
            discard = Some(f.path.clone());
        } else if btn.clicked() {
            toggle = Some(f.path.clone());
        } else if resp.clicked() {
            select = Some(key);
        }
    }

    if let Some(path) = discard {
        // Arms the confirm modal; the job fires there.
        tab.pending_discard = Some(Some(path));
    }

    if let Some(path) = toggle {
        jobs.push(if staged {
            Job::Unstage {
                id: tab.id,
                path: Some(path),
            }
        } else {
            Job::Stage {
                id: tab.id,
                path: Some(path),
            }
        });
    }
    if let Some(key) = select {
        tab.selection = Some(key.clone());
        tab.diff = None;
        jobs.push(Job::LoadDiff { id: tab.id, key });
    }
}

fn commit_box(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    ui.add(
        TextEdit::multiline(&mut tab.commit_msg)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("commit message…"),
    );
    // Char-count readout between the editor and the trigger.
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
    theme::tracked_text(
        ui.painter(),
        pos2(r.right() - 2.0, r.center().y),
        Align2::RIGHT_CENTER,
        &format!("{:03} CHARS", tab.commit_msg.chars().count()),
        theme::display_font(10.0),
        DIM,
        1.0,
    );
    let enabled = tab.can_commit();
    let button = HudButton::new("▶ COMMIT")
        .accent(true)
        .hint("CTRL+ENTER")
        .min_size(vec2(ui.available_width(), 30.0));
    if ui.add_enabled(enabled, button).clicked() {
        jobs.push(Job::Commit {
            id: tab.id,
            message: tab.commit_msg.clone(),
        });
    }
}

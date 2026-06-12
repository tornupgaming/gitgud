//! Right "worktree" column. Two modes: the working-tree view (staged/unstaged
//! changes plus the commit box) by default, or the selected commit's detail
//! (metadata + touched-file list) while a graph commit is selected.

use eframe::egui::{
    Align, Align2, CentralPanel, CornerRadius, Frame, Id, Layout, Margin, Modal, Panel, Rect,
    RichText, ScrollArea, Sense, Spinner, Stroke, StrokeKind, TextEdit, TextStyle, Ui, pos2, vec2,
};

use super::diff::DiffView;
use super::{HudButton, module, rel_time, sub_label, truncate_left};
use crate::app::RepoTab;
use crate::git::{ChangeKind, DiffKey, Job};
use crate::theme::{self, ACCENT, AMBER, BG0, BG1, BG3, BRIGHT, DIM, GREEN, RED, TEXT};

/// Picks the column's mode off the current selection, then runs the discard
/// and abort-merge confirm modals (they render whenever their tab state is
/// armed, regardless of mode).
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    match tab.selection.clone() {
        Some(DiffKey::Commit { id }) => commit_detail(ui, tab, &id),
        _ => wip(ui, tab, jobs),
    }
    confirm_discard(ui, tab, jobs);
    confirm_abort_merge(ui, tab, jobs);
}

/// Working-tree mode: commit module pinned to the bottom, changes above it.
fn wip(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
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
            let delta = tab.staged.len() + tab.unstaged.len();
            let readout = format!("{:02} Δ", delta);
            module(ui, None, "WORKING TREE", &readout, |ui| {
                if tab.merging.is_some() {
                    merge_banner(ui, tab);
                    ui.add_space(8.0);
                }
                changes(ui, tab, jobs)
            });
        });
}

/// Amber strip above the changes lists while a merge is in progress. Abort is
/// arm-then-confirm like discard: the button only sets `pending_abort_merge`.
fn merge_banner(ui: &mut Ui, tab: &mut RepoTab) {
    let Some(m) = &tab.merging else { return };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::hover());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(AMBER, 14));
    p.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, theme::alpha(AMBER, 140)),
        StrokeKind::Inside,
    );
    theme::tracked_text(
        &p,
        pos2(rect.left() + 8.0, rect.top() + 13.0),
        Align2::LEFT_CENTER,
        "MERGING",
        theme::display_font(12.0),
        AMBER,
        2.0,
    );
    theme::tracked_text(
        &p,
        pos2(rect.right() - 8.0, rect.top() + 13.0),
        Align2::RIGHT_CENTER,
        &m.head_short,
        theme::display_font(11.0),
        DIM,
        1.5,
    );
    let (status, color) = if tab.has_conflicts() {
        ("resolve conflicts, then commit", DIM)
    } else {
        ("ready to commit merge", GREEN)
    };
    p.text(
        pos2(rect.left() + 8.0, rect.bottom() - 13.0),
        Align2::LEFT_CENTER,
        status,
        TextStyle::Small.resolve(ui.style()),
        color,
    );
    let btn_rect = Rect::from_min_size(
        pos2(rect.right() - 88.0, rect.bottom() - 26.0),
        vec2(80.0, 20.0),
    );
    let abort = HudButton::new("× ABORT").small(true).text_color(RED);
    if ui.put(btn_rect, abort).clicked() {
        tab.pending_abort_merge = true;
    }
}

/// Modal for a pending merge abort. Destroys all resolution progress, so it
/// gets the same red treatment as discard.
fn confirm_abort_merge(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    if !tab.pending_abort_merge {
        return;
    }

    let frame = Frame::new()
        .fill(BG1)
        .stroke(Stroke::new(1.0, RED))
        .inner_margin(Margin::same(14));
    let modal = Modal::new(Id::new("confirm_abort_merge"))
        .frame(frame)
        .backdrop_color(theme::alpha(BG0, 215))
        .show(ui.ctx(), |ui| {
            ui.set_width(300.0);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            theme::tracked_text(
                ui.painter(),
                pos2(r.left(), r.center().y),
                Align2::LEFT_CENTER,
                "CONFIRM ABORT",
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
            ui.label("Abort the merge in progress?");
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠ Abandons the merge and all conflict resolutions.")
                    .small()
                    .color(RED),
            );
            ui.add_space(14.0);
            let buttons = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let abort = ui
                        .add(HudButton::new("ABORT MERGE").text_color(RED))
                        .clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (abort, cancel)
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

    let (abort, cancel) = modal.inner;
    if abort {
        jobs.push(Job::AbortMerge { id: tab.id });
        tab.pending_abort_merge = false;
    } else if cancel || modal.should_close() {
        tab.pending_abort_merge = false;
    }
}

/// Commit-detail mode: metadata for the selected commit and its touched
/// files; clicking a file opens that file's diff in the center panel.
fn commit_detail(ui: &mut Ui, tab: &mut RepoTab, id: &str) {
    let short = tab
        .commits
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.short.clone())
        .unwrap_or_else(|| id[..id.len().min(7)].to_owned());
    module(ui, None, "COMMIT", &short, |ui| {
        if ui
            .add(HudButton::new("◀ WORKING TREE").small(true))
            .clicked()
        {
            tab.selection = None;
            tab.commit_diff = None;
            tab.commit_file = None;
            tab.diff = None;
            return;
        }
        ui.add_space(8.0);

        if let Some(c) = tab.commits.iter().find(|c| c.id == id) {
            ui.label(RichText::new(c.summary.as_str()).color(BRIGHT));
            ui.label(
                RichText::new(format!("{} · {}", c.author, rel_time(c.time)))
                    .small()
                    .color(DIM),
            );
        }
        ui.add_space(6.0);

        let Some(data) = &tab.commit_diff else {
            // Selection made but the worker hasn't answered yet.
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.add(Spinner::new().size(24.0).color(ACCENT)));
            return;
        };

        ui.horizontal(|ui| {
            let label = format!("{:02} FILES", data.files.len());
            let (rect, _) = ui.allocate_exact_size(vec2(100.0, 20.0), Sense::hover());
            sub_label(ui, rect, &label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("−{}", data.dels)).color(RED));
                ui.label(RichText::new(format!("+{}", data.adds)).color(GREEN));
            });
        });
        ui.add_space(4.0);

        // Record the click during the walk; `tab.commit_diff` is borrowed
        // for the whole list, so the mutation happens after it.
        let mut open: Option<String> = None;
        let font = TextStyle::Body.resolve(ui.style());
        let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
        ScrollArea::vertical()
            .id_salt("commit_files")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for f in &data.files {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let selected = tab.commit_file.as_deref() == Some(f.path.as_str());
                    let p = ui.painter().with_clip_rect(rect);
                    if selected {
                        p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(ACCENT, 24));
                    } else if resp.hovered() {
                        p.rect_filled(rect, CornerRadius::ZERO, BG3);
                    }
                    let y = rect.center().y;
                    let r = p.text(
                        pos2(rect.right() - 4.0, y),
                        Align2::RIGHT_CENTER,
                        format!("−{}", f.dels),
                        font.clone(),
                        RED,
                    );
                    let r = p.text(
                        pos2(r.left() - 6.0, y),
                        Align2::RIGHT_CENTER,
                        format!("+{}", f.adds),
                        font.clone(),
                        GREEN,
                    );
                    let path = truncate_left(
                        ui,
                        &f.path,
                        &font,
                        (r.left() - 8.0 - rect.left() - 4.0).max(0.0),
                    );
                    p.text(
                        pos2(rect.left() + 4.0, y),
                        Align2::LEFT_CENTER,
                        path,
                        font.clone(),
                        TEXT,
                    );
                    if selected {
                        theme::glow_outline(
                            &p,
                            theme::chamfer(rect.shrink(1.0), 6.0),
                            theme::alpha(ACCENT, 200),
                        );
                    }
                    if resp.clicked() {
                        open = Some(f.path.clone());
                    }
                }
            });

        if let Some(path) = open {
            // Built synchronously from the already-loaded commit diff -- no job.
            tab.diff = DiffView::build_one(tab.commit_diff.as_ref().unwrap(), &path);
            tab.commit_file = Some(path);
        }
    });
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
        let conflicted = f.kind == ChangeKind::Conflicted;
        let key = if conflicted {
            DiffKey::Conflict {
                path: f.path.clone(),
            }
        } else {
            DiffKey::Workdir {
                path: f.path.clone(),
                staged,
            }
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
        // Conflicted rows carry none: staging raw marker text or discarding
        // mid-merge is a footgun; resolution happens in the center view.
        let btn_count = if conflicted {
            0.0
        } else if staged {
            1.0
        } else {
            2.0
        };
        let text_w = rect.width() - 20.0 - (btn_w + 4.0) * btn_count - 8.0;
        let path = truncate_left(ui, &f.path, &font, text_w);
        p.text(
            pos2(rect.left() + 20.0, y),
            Align2::LEFT_CENTER,
            path,
            font.clone(),
            if conflicted { RED } else { TEXT },
        );

        let mut toggle_clicked = false;
        let mut discard_clicked = false;
        if !conflicted {
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
            toggle_clicked = btn.clicked();

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
                    "×",
                    font.clone(),
                    RED,
                );
                discard_clicked = x_btn.clicked();
            }
        }

        if discard_clicked {
            discard = Some(f.path.clone());
        } else if toggle_clicked {
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
        if let DiffKey::Conflict { path } = key {
            tab.conflict = None;
            jobs.push(Job::LoadConflict { id: tab.id, path });
        } else {
            jobs.push(Job::LoadDiff { id: tab.id, key });
        }
    }
}

fn commit_box(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    ui.add(
        TextEdit::multiline(&mut tab.commit_msg)
            .desired_rows(3)
            .desired_width(f32::INFINITY)
            .hint_text("commit message…"),
    );
    // Char-count readout between the editor and the trigger; mid-merge the
    // left side shows why the trigger is disabled.
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
    let conflicts = tab
        .unstaged
        .iter()
        .filter(|f| f.kind == ChangeKind::Conflicted)
        .count();
    if conflicts > 0 {
        theme::tracked_text(
            ui.painter(),
            pos2(r.left() + 2.0, r.center().y),
            Align2::LEFT_CENTER,
            &format!("{:02} CONFLICTS", conflicts),
            theme::display_font(10.0),
            RED,
            1.0,
        );
    }
    let enabled = tab.can_commit();
    let label = if tab.merging.is_some() {
        "▶ COMMIT MERGE"
    } else {
        "▶ COMMIT"
    };
    let button = HudButton::new(label)
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

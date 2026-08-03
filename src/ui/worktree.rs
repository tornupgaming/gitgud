//! Right "worktree" column. Two modes: the working-tree view (staged/unstaged
//! changes plus the commit box) by default, or the selected commit's detail
//! (metadata + touched-file list) while a graph commit is selected.

use eframe::egui::{
    Align, Align2, CentralPanel, CornerRadius, FontId, Frame, Id, Layout, Margin, Panel, Rect,
    Response, RichText, Sense, Spinner, Stroke, StrokeKind, TextEdit, TextStyle, Ui, pos2, vec2,
};

use super::diff::DiffView;
use super::{HudButton, module, rel_time, sub_label, tree, truncate_left, vscroll};
use crate::app::RepoTab;
use crate::git::{ChangeKind, DiffKey, FileChange, FileDiff, Job};
use crate::theme::{self, ACCENT, AMBER, BG3, BRIGHT, DIM, GREEN, PURPLE, RED, TEXT};

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
        Stroke::new(1.0_f32, theme::alpha(AMBER, 140)),
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
    let t = super::factor(
        ui.ctx(),
        Id::new("confirm_abort_merge"),
        tab.pending_abort_merge,
    );
    if !tab.pending_abort_merge {
        return;
    }

    let ((abort, cancel), closed) = super::hud_modal(
        ui,
        "confirm_abort_merge",
        "CONFIRM ABORT",
        RED,
        300.0,
        t,
        |ui| {
            ui.label("Abort the merge in progress?");
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠ Abandons the merge and all conflict resolutions.")
                    .small()
                    .color(RED),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let abort = ui
                    .add(HudButton::new("ABORT MERGE").text_color(RED))
                    .clicked();
                let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                (abort, cancel)
            })
            .inner
        },
    );

    if abort {
        jobs.push(Job::AbortMerge { id: tab.id });
        tab.pending_abort_merge = false;
    } else if cancel || closed {
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

        let mut flip_view = false;
        ui.horizontal(|ui| {
            let label = format!("{:02} FILES", data.files.len());
            let (rect, _) = ui.allocate_exact_size(vec2(100.0, 20.0), Sense::hover());
            sub_label(ui, rect, &label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("−{}", data.dels)).color(RED));
                ui.label(RichText::new(format!("+{}", data.adds)).color(GREEN));
                let view = if tab.tree_view { "LIST" } else { "TREE" };
                flip_view = ui.add(HudButton::new(view).small(true)).clicked();
            });
        });
        ui.add_space(4.0);

        // Record clicks during the walk; `tab.commit_diff` is borrowed for
        // the whole list, so the mutations happen after it.
        let mut open: Option<String> = None;
        let mut folder: Option<String> = None;
        let font = TextStyle::Body.resolve(ui.style());
        let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
        vscroll("commit_files").show(ui, |ui| {
            if tab.tree_view {
                let nodes = tree::build(data.files.iter().map(|f| f.path.as_str()), "commit");
                for row in tree::rows(&nodes, &tab.collapsed_file_folders) {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    match row.kind {
                        tree::RowKind::Folder {
                            name,
                            path,
                            collapsed,
                        } => {
                            let p = ui.painter().with_clip_rect(rect);
                            tree::connectors(&p, rect, row.depth, row.lanes, row.is_last);
                            tree::folder_row(
                                &p,
                                rect,
                                resp.hovered(),
                                row.depth as f32 * tree::INDENT,
                                name,
                                collapsed,
                                font.clone(),
                            );
                            if resp.clicked() {
                                folder = Some(path.to_string());
                            }
                        }
                        tree::RowKind::Leaf(i) => {
                            let f = &data.files[i];
                            let leaf = f.path.rsplit('/').next().unwrap_or(&f.path);
                            let selected = tab.commit_file.as_deref() == Some(f.path.as_str());
                            if commit_file_row(
                                ui,
                                rect,
                                &resp,
                                f,
                                leaf,
                                selected,
                                Some(&row),
                                &font,
                            ) {
                                open = Some(f.path.clone());
                            }
                        }
                    }
                }
            } else {
                for f in &data.files {
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    let selected = tab.commit_file.as_deref() == Some(f.path.as_str());
                    if commit_file_row(ui, rect, &resp, f, &f.path, selected, None, &font) {
                        open = Some(f.path.clone());
                    }
                }
            }
        });

        if flip_view {
            tab.tree_view = !tab.tree_view;
        }
        if let Some(path) = folder {
            // Absent = expanded, so toggling is remove-or-insert.
            if !tab.collapsed_file_folders.remove(&path) {
                tab.collapsed_file_folders.insert(path);
            }
        }
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
    let t = super::factor(
        ui.ctx(),
        Id::new("confirm_discard"),
        tab.pending_discard.is_some(),
    );
    let Some(target) = tab.pending_discard.clone() else {
        return;
    };
    let what = match &target {
        Some(path) => format!("Discard changes to \"{path}\"?"),
        None => format!("Discard all {} unstaged changes?", tab.unstaged.len()),
    };

    let ((discard, cancel), closed) = super::hud_modal(
        ui,
        "confirm_discard",
        "CONFIRM DISCARD",
        RED,
        300.0,
        t,
        |ui| {
            ui.label(what);
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠ This cannot be undone. Untracked files will be deleted.")
                    .small()
                    .color(RED),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let discard = ui.add(HudButton::new("DISCARD").text_color(RED)).clicked();
                let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                (discard, cancel)
            })
            .inner
        },
    );

    if discard {
        jobs.push(Job::Discard {
            id: tab.id,
            path: target,
        });
        tab.pending_discard = None;
    } else if cancel || closed {
        tab.pending_discard = None;
    }
}

/// What the user asked of a file row or its tree: open it in the diff view,
/// stage/unstage it, arm a discard, or collapse/expand a folder. Recorded
/// during the row walk (which borrows the file list) and applied after it by
/// `apply_file_action`.
enum FileAction {
    Select(DiffKey),
    Toggle(String),
    Discard(String),
    ToggleFolder(String),
}

fn changes(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    vscroll("changes").show(ui, |ui| {
        let staged_label = format!("STAGED · {:02}", tab.staged.len());
        let any_staged = !tab.staged.is_empty();
        // The view toggle rides on the STAGED header but flips both lists (and
        // the commit-detail view).
        let view = if tab.tree_view { "LIST" } else { "TREE" };
        let actions = [("UNSTAGE ALL", any_staged), (view, true)];
        match list_header(ui, &staged_label, &actions) {
            Some(0) => jobs.push(Job::Unstage {
                id: tab.id,
                path: None,
            }),
            Some(1) => tab.tree_view = !tab.tree_view,
            _ => {}
        }
        let action = file_rows(ui, tab, true);
        apply_file_action(tab, true, action, jobs);
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
        let action = file_rows(ui, tab, false);
        apply_file_action(tab, false, action, jobs);
    });
}

/// Applies a recorded file-row click. `staged` picks a Toggle's direction.
fn apply_file_action(
    tab: &mut RepoTab,
    staged: bool,
    action: Option<FileAction>,
    jobs: &mut Vec<Job>,
) {
    match action {
        Some(FileAction::Discard(path)) => {
            // Arms the confirm modal; the job fires there.
            tab.pending_discard = Some(Some(path));
        }
        Some(FileAction::Toggle(path)) => jobs.push(if staged {
            Job::Unstage {
                id: tab.id,
                path: Some(path),
            }
        } else {
            Job::Stage {
                id: tab.id,
                path: Some(path),
            }
        }),
        Some(FileAction::Select(key)) => {
            tab.selection = Some(key.clone());
            tab.diff = None;
            if let DiffKey::Conflict { path } = key {
                tab.conflict = None;
                jobs.push(Job::LoadConflict { id: tab.id, path });
            } else {
                jobs.push(Job::LoadDiff { id: tab.id, key });
            }
        }
        Some(FileAction::ToggleFolder(path)) => {
            // Absent = expanded, so toggling is remove-or-insert.
            if tab.collapsed_file_folders.contains(&path) {
                tab.collapsed_file_folders.remove(&path);
            } else {
                tab.collapsed_file_folders.insert(path);
            }
        }
        None => {}
    }
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

/// Walks one section's file list -- flat or as a path tree per `tab.tree_view`
/// -- and returns the action the user asked for, if any.
fn file_rows(ui: &mut Ui, tab: &RepoTab, staged: bool) -> Option<FileAction> {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;
    let files = if staged { &tab.staged } else { &tab.unstaged };
    let mut action = None;

    if !tab.tree_view {
        for f in files {
            if let Some(a) = file_row(ui, tab, f, staged, None, &f.path, &font, row_h) {
                action = Some(a);
            }
        }
        return action;
    }

    let prefix = if staged { "staged" } else { "unstaged" };
    let nodes = tree::build(files.iter().map(|f| f.path.as_str()), prefix);
    for row in tree::rows(&nodes, &tab.collapsed_file_folders) {
        match row.kind {
            tree::RowKind::Folder {
                name,
                path,
                collapsed,
            } => {
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let p = ui.painter().with_clip_rect(rect);
                tree::connectors(&p, rect, row.depth, row.lanes, row.is_last);
                tree::folder_row(
                    &p,
                    rect,
                    resp.hovered(),
                    row.depth as f32 * tree::INDENT,
                    name,
                    collapsed,
                    font.clone(),
                );
                if resp.clicked() {
                    action = Some(FileAction::ToggleFolder(path.to_string()));
                }
            }
            tree::RowKind::Leaf(i) => {
                let f = &files[i];
                let leaf = f.path.rsplit('/').next().unwrap_or(&f.path);
                if let Some(a) = file_row(ui, tab, f, staged, Some(&row), leaf, &font, row_h) {
                    action = Some(a);
                }
            }
        }
    }
    action
}

/// One working-tree file row: kind glyph, name, and the stage/unstage/discard
/// buttons. `row` carries the tree position in tree mode (`None` = flat list);
/// `label` is what's drawn -- the full path flat, the final segment in a tree.
/// The returned action always carries the full path.
#[allow(clippy::too_many_arguments)]
fn file_row(
    ui: &mut Ui,
    tab: &RepoTab,
    f: &FileChange,
    staged: bool,
    row: Option<&tree::Row>,
    label: &str,
    font: &FontId,
    row_h: f32,
) -> Option<FileAction> {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let btn_w = 16.0;
    let indent = row.map_or(0.0, |r| r.depth as f32 * tree::INDENT);
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
            Stroke::new(1.0_f32, theme::alpha(ACCENT, 150)),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, BG3);
    }
    if let Some(r) = row {
        tree::connectors(&p, rect, r.depth, r.lanes, r.is_last);
    }

    let y = rect.center().y;
    let glyph_color = match f.kind {
        ChangeKind::Added | ChangeKind::Untracked => GREEN,
        ChangeKind::Deleted | ChangeKind::Conflicted => RED,
        ChangeKind::Modified => AMBER,
        ChangeKind::Submodule => PURPLE,
        _ => DIM,
    };
    p.text(
        pos2(rect.left() + 4.0 + indent, y),
        Align2::LEFT_CENTER,
        f.kind.glyph(),
        font.clone(),
        glyph_color,
    );

    // Unstaged rows carry two buttons (discard + stage), staged rows one.
    // Conflicted rows carry none: staging raw marker text or discarding
    // mid-merge is a footgun; resolution happens in the center view.
    // Submodule rows carry only stage/unstage: discarding a gitlink without
    // moving the submodule checkout would just re-stage on the next refresh.
    let submodule = f.kind == ChangeKind::Submodule;
    let btn_count = if conflicted {
        0.0
    } else if staged || submodule {
        1.0
    } else {
        2.0
    };
    let text_w = rect.width() - 20.0 - indent - (btn_w + 4.0) * btn_count - 8.0;
    let label = truncate_left(ui, label, font, text_w.max(0.0));
    p.text(
        pos2(rect.left() + 20.0 + indent, y),
        Align2::LEFT_CENTER,
        label,
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
                Stroke::new(1.0_f32, ACCENT),
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

        if !staged && !submodule {
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
                    Stroke::new(1.0_f32, RED),
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
        Some(FileAction::Discard(f.path.clone()))
    } else if toggle_clicked {
        Some(FileAction::Toggle(f.path.clone()))
    } else if resp.clicked() {
        Some(FileAction::Select(key))
    } else {
        None
    }
}

/// One commit-detail file row: +/− counts right-aligned, then the name. `row`
/// carries the tree position in tree mode (`None` = flat list); `label` is the
/// full path flat, the final segment in a tree. Returns true when clicked.
#[allow(clippy::too_many_arguments)]
fn commit_file_row(
    ui: &Ui,
    rect: Rect,
    resp: &Response,
    f: &FileDiff,
    label: &str,
    selected: bool,
    row: Option<&tree::Row>,
    font: &FontId,
) -> bool {
    let p = ui.painter().with_clip_rect(rect);
    if selected {
        p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(ACCENT, 24));
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, BG3);
    }
    if let Some(r) = row {
        tree::connectors(&p, rect, r.depth, r.lanes, r.is_last);
    }
    let text_x = row.map_or(4.0, |r| 20.0 + r.depth as f32 * tree::INDENT);
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
    let label = truncate_left(
        ui,
        label,
        font,
        (r.left() - 8.0 - rect.left() - text_x).max(0.0),
    );
    p.text(
        pos2(rect.left() + text_x, y),
        Align2::LEFT_CENTER,
        label,
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
    resp.clicked()
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

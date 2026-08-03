//! Left "ops" column: the BRANCHES module and its checkout confirm flow.

use eframe::egui::{
    Align, Align2, CentralPanel, CornerRadius, Frame, Id, Key, Layout, Margin, Panel, RichText,
    Sense, Stroke, TextEdit, TextStyle, Ui, pos2, vec2,
};

use super::{HudButton, module, submodules, tree, vscroll};
use crate::app::{NewBranch, PendingMerge, RepoTab};
use crate::git::{Job, MergeDirection};
use crate::theme::{self, ACCENT, AMBER, BG3, DIM, GREEN, PURPLE, RED, TEXT};

/// What the user asked of a branch row: a double-click runs the Checkout Flow,
/// the right-click menu can merge that row or create a new branch from its tip,
/// and a click on a folder row toggles its collapsed state.
enum BranchAction {
    Checkout(String),
    Merge { branch: String, is_remote: bool },
    CreateFrom { start_point: String, label: String },
    ToggleFolder(String),
}

/// The BRANCHES module plus its flow modals. A double-click runs the Checkout
/// Flow; the context menu opens the Merge Branch Flow or the new-branch dialog.
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    // With submodules, SUBMODULES takes a resizable strip at the bottom and
    // BRANCHES fills the rest; without, BRANCHES owns the whole column.
    if !tab.submodules.is_empty() {
        Panel::bottom("submodules_panel")
            .resizable(true)
            .default_size(180.0)
            .frame(Frame::new().inner_margin(Margin {
                left: 0,
                right: 0,
                top: 8,
                bottom: 0,
            }))
            .show_inside(ui, |ui| submodules::show(ui, tab, jobs));
        CentralPanel::default()
            .frame(Frame::new())
            .show_inside(ui, |ui| branches_module(ui, tab, jobs));
    } else {
        branches_module(ui, tab, jobs);
    }
    checkout_flow_modal(ui, tab, jobs);
    merge_flow_modal(ui, tab, jobs);
    confirm_new_branch(ui, tab, jobs);
}

/// The BRANCHES module proper. A clean checkout fires immediately; anything
/// that needs a decision (a blocked checkout, a merge) arms a modal run by
/// `show`. Either way, starting a flow clears any stale into-selected restore.
fn branches_module(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let readout = format!("{:02}", tab.branches.len());
    let action = module(ui, None, "BRANCHES", &readout, |ui| {
        vscroll("branches").show(ui, |ui| branches(ui, tab)).inner
    });
    match action {
        Some(BranchAction::Checkout(name)) => {
            tab.merge_return = None;
            // Checkout Flow: a clean tree switches at once; a precondition
            // failure (uncommitted changes or a merge in progress) arms the
            // warning modal instead.
            if tab.has_blocking_changes() || tab.merging.is_some() {
                tab.pending_checkout = Some(name);
            } else {
                jobs.push(Job::Checkout {
                    id: tab.id,
                    branch: name,
                    discard: false,
                });
            }
        }
        Some(BranchAction::Merge { branch, is_remote }) => {
            tab.merge_return = None;
            tab.pending_merge = Some(PendingMerge {
                branch,
                is_remote,
                discard: false,
            });
        }
        Some(BranchAction::CreateFrom { start_point, label }) => {
            tab.pending_branch = Some(NewBranch::new(start_point, label));
        }
        Some(BranchAction::ToggleFolder(path)) => {
            // Absent = expanded, so toggling is remove-or-insert.
            if tab.collapsed_folders.contains(&path) {
                tab.collapsed_folders.remove(&path);
            } else {
                tab.collapsed_folders.insert(path);
            }
        }
        None => {}
    }
}

/// The Checkout Flow's warning modal, shown only when a precondition blocks the
/// switch. Uncommitted changes (checked first) offer Discard & continue; a
/// merge in progress is exit-only. A clean switch never reaches here.
fn checkout_flow_modal(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let t = super::factor(
        ui.ctx(),
        Id::new("checkout_flow"),
        tab.pending_checkout.is_some(),
    );
    let Some(branch) = tab.pending_checkout.clone() else {
        return;
    };
    let dirty = tab.has_blocking_changes();
    let mid_merge = tab.merging.is_some();
    // The blocking condition cleared under us (e.g. a background refresh):
    // nothing left to warn about, so just switch.
    if !dirty && !mid_merge {
        jobs.push(Job::Checkout {
            id: tab.id,
            branch,
            discard: false,
        });
        tab.pending_checkout = None;
        return;
    }

    let ((discard, cancel), closed) = super::hud_modal(
        ui,
        "checkout_flow",
        "CHECKOUT BLOCKED",
        ACCENT,
        300.0,
        t,
        |ui| {
            if dirty {
                ui.label(format!(
                    "Switch to \"{branch}\" — but you have uncommitted changes."
                ));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Discard them (a hard reset of tracked changes and submodules) \
                         and continue? Untracked files are kept.",
                    )
                    .small()
                    .color(AMBER),
                );
                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let discard = ui
                        .add(HudButton::new("DISCARD & CONTINUE").text_color(RED))
                        .clicked();
                    let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                    (discard, cancel)
                })
                .inner
            } else {
                ui.label(format!(
                    "Can't switch to \"{branch}\" — a merge is in progress."
                ));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("⚠ Finish or abort the current merge first.")
                        .small()
                        .color(AMBER),
                );
                ui.add_space(14.0);
                let cancel = ui
                    .with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(HudButton::new("OK")).clicked()
                    })
                    .inner;
                (false, cancel)
            }
        },
    );

    if discard {
        jobs.push(Job::Checkout {
            id: tab.id,
            branch,
            discard: true,
        });
        tab.pending_checkout = None;
    } else if cancel || closed {
        tab.pending_checkout = None;
    }
}

/// The Merge Branch Flow modal: runs the preconditions (uncommitted changes,
/// then merge-in-progress, then detached HEAD) and finally offers the direction
/// choice. The job fires only on a direction click. Discard & continue waives
/// the uncommitted-changes gate and rides on the eventual job's `discard` flag.
fn merge_flow_modal(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let t = super::factor(ui.ctx(), Id::new("merge_flow"), tab.pending_merge.is_some());
    let Some(pm) = tab.pending_merge.clone() else {
        return;
    };
    let dirty = tab.has_blocking_changes();

    enum Outcome {
        None,
        Discard,
        IntoCurrent,
        IntoSelected,
        Cancel,
    }

    let (outcome, closed) = super::hud_modal(ui, "merge_flow", "MERGE", ACCENT, 330.0, t, |ui| {
        if dirty && !pm.discard {
            ui.label(format!(
                "Merge \"{}\" — but you have uncommitted changes.",
                pm.branch
            ));
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Discard them (a hard reset of tracked changes and submodules) \
                     and continue? Untracked files are kept.",
                )
                .small()
                .color(AMBER),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(HudButton::new("DISCARD & CONTINUE").text_color(RED))
                    .clicked()
                {
                    Outcome::Discard
                } else if ui.add(HudButton::new("CANCEL")).clicked() {
                    Outcome::Cancel
                } else {
                    Outcome::None
                }
            })
            .inner
        } else if tab.merging.is_some() {
            ui.label(format!(
                "Can't merge \"{}\" — a merge is already in progress.",
                pm.branch
            ));
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠ Finish or abort the current merge first.")
                    .small()
                    .color(AMBER),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(HudButton::new("OK")).clicked() {
                    Outcome::Cancel
                } else {
                    Outcome::None
                }
            })
            .inner
        } else if tab.head_detached {
            ui.label("Can't merge — HEAD is detached.");
            ui.add_space(4.0);
            ui.label(
                RichText::new("⚠ Check out a branch to merge into first.")
                    .small()
                    .color(AMBER),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(HudButton::new("OK")).clicked() {
                    Outcome::Cancel
                } else {
                    Outcome::None
                }
            })
            .inner
        } else {
            ui.label(format!("Merge \"{}\" — choose a direction:", pm.branch));
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "INTO CURRENT — merge \"{}\" into \"{}\"; HEAD stays put.",
                    pm.branch, tab.head
                ))
                .small()
                .color(DIM),
            );
            if !pm.is_remote {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!(
                        "INTO SELECTED — switch to \"{}\" and merge \"{}\" in.",
                        pm.branch, tab.head
                    ))
                    .small()
                    .color(DIM),
                );
            }
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let into_current = ui
                    .add(HudButton::new("INTO CURRENT").accent(true))
                    .clicked();
                // Into-selected is meaningless for a remote ref (can't merge
                // into one), so it's hidden there.
                let into_selected =
                    !pm.is_remote && ui.add(HudButton::new("INTO SELECTED")).clicked();
                let cancel = ui.add(HudButton::new("CANCEL")).clicked();
                if into_current {
                    Outcome::IntoCurrent
                } else if into_selected {
                    Outcome::IntoSelected
                } else if cancel {
                    Outcome::Cancel
                } else {
                    Outcome::None
                }
            })
            .inner
        }
    });

    match outcome {
        // Waive the uncommitted gate; the chosen direction's job resets first.
        Outcome::Discard => {
            if let Some(pm) = tab.pending_merge.as_mut() {
                pm.discard = true;
            }
        }
        Outcome::IntoCurrent => {
            jobs.push(Job::Merge {
                id: tab.id,
                branch: pm.branch,
                direction: MergeDirection::IntoCurrent,
                discard: pm.discard,
            });
            tab.pending_merge = None;
        }
        Outcome::IntoSelected => {
            // Remember the branch we're leaving so an abort can return to it.
            tab.merge_return = Some(tab.head.clone());
            jobs.push(Job::Merge {
                id: tab.id,
                branch: pm.branch,
                direction: MergeDirection::IntoSelected,
                discard: pm.discard,
            });
            tab.pending_merge = None;
        }
        Outcome::Cancel => tab.pending_merge = None,
        Outcome::None => {
            if closed {
                tab.pending_merge = None;
            }
        }
    }
}

/// Modal for "create branch from here": a name field plus CREATE (stay put)
/// and CREATE + SWITCH (also check it out). The job fires on confirm or Enter;
/// clicking away, Escape, or Cancel aborts. An empty name disables both.
fn confirm_new_branch(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let t = super::factor(
        ui.ctx(),
        Id::new("new_branch"),
        tab.pending_branch.is_some(),
    );
    if tab.pending_branch.is_none() {
        return;
    }

    let ((create_switch, create, cancel), closed) =
        super::hud_modal(ui, "new_branch", "NEW BRANCH", ACCENT, 300.0, t, |ui| {
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
            // Enter submits the primary action (create + switch).
            (create_switch || entered, create, cancel)
        });

    if create_switch || create {
        let nb = tab.pending_branch.take().unwrap();
        jobs.push(Job::CreateBranch {
            id: tab.id,
            name: nb.name.trim().to_string(),
            start_point: nb.start_point,
            checkout: create_switch,
        });
    } else if cancel || closed {
        tab.pending_branch = None;
    }
}

/// Draws the branch tree and returns the action the user asked for, if any: a
/// double-click on a non-HEAD branch arms a checkout, the right-click menu arms a
/// new branch from that row, and a click on a folder toggles it. The caller
/// confirms branch actions before acting.
fn branches(ui: &mut Ui, tab: &RepoTab) -> Option<BranchAction> {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;

    let nodes = tree::build(tab.branches.iter().map(|b| b.name.as_str()), "");
    let rows = tree::rows(&nodes, &tab.collapsed_folders);

    let mut action = None;
    for row in &rows {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let p = ui.painter().with_clip_rect(rect);
        let y = rect.center().y;
        let indent = row.depth as f32 * tree::INDENT;
        tree::connectors(&p, rect, row.depth, row.lanes, row.is_last);
        match row.kind {
            tree::RowKind::Folder {
                name,
                path,
                collapsed,
            } => {
                tree::folder_row(
                    &p,
                    rect,
                    resp.hovered(),
                    indent,
                    name,
                    collapsed,
                    font.clone(),
                );
                if resp.clicked() {
                    action = Some(BranchAction::ToggleFolder(path.to_string()));
                }
            }
            tree::RowKind::Leaf(i) => {
                let b = &tab.branches[i];
                if b.is_head {
                    // The checked-out branch reads as the "armed" row.
                    p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(AMBER, 16));
                } else if resp.hovered() {
                    p.rect_filled(rect, CornerRadius::ZERO, BG3);
                }
                // The folder chain carries the prefix, so a leaf shows only its
                // final segment.
                let leaf = b.name.rsplit('/').next().unwrap_or(&b.name);
                if b.is_remote {
                    // Hollow node marker, painted: Rajdhani has no ◦ glyph.
                    p.circle_stroke(
                        pos2(rect.left() + 9.0 + indent, y),
                        3.0,
                        Stroke::new(1.0_f32, PURPLE),
                    );
                    p.text(
                        pos2(rect.left() + 20.0 + indent, y),
                        Align2::LEFT_CENTER,
                        leaf,
                        font.clone(),
                        PURPLE,
                    );
                } else {
                    if b.is_head {
                        p.text(
                            pos2(rect.left() + 4.0 + indent, y),
                            Align2::LEFT_CENTER,
                            "▶",
                            font.clone(),
                            AMBER,
                        );
                    }
                    p.text(
                        pos2(rect.left() + 20.0 + indent, y),
                        Align2::LEFT_CENTER,
                        leaf,
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
                if resp.double_clicked() && !b.is_head {
                    action = Some(BranchAction::Checkout(b.name.clone()));
                }
                // A branch's tip is a valid start point -- branch by name, which
                // revparse resolves for both local and remote branches.
                super::hud_menu(&resp, |ui| {
                    if ui.button("Create branch from here").clicked() {
                        action = Some(BranchAction::CreateFrom {
                            start_point: b.name.clone(),
                            label: b.name.clone(),
                        });
                        ui.close();
                    }
                    // Merge targets a branch other than the current HEAD; the
                    // ellipsis marks that it opens the direction modal.
                    if !b.is_head && ui.button("Merge…").clicked() {
                        action = Some(BranchAction::Merge {
                            branch: b.name.clone(),
                            is_remote: b.is_remote,
                        });
                        ui.close();
                    }
                });
            }
        }
    }
    action
}

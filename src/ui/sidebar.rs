//! Left "ops" column: the BRANCHES module and its checkout confirm flow.

use std::collections::HashSet;

use eframe::egui::{
    Align, Align2, CentralPanel, CornerRadius, Frame, Id, Key, Layout, Margin, Modal, Painter,
    Panel, Pos2, Rect, RichText, ScrollArea, Sense, Shape, Stroke, TextEdit, TextStyle, Ui, pos2,
    vec2,
};

use super::{HudButton, module, submodules};
use crate::app::{NewBranch, RepoTab};
use crate::git::{BranchInfo, Job};
use crate::theme::{self, ACCENT, AMBER, BG0, BG1, BG3, DIM, GREEN, PURPLE, RED, TEXT};

/// Horizontal step per tree depth level.
const INDENT: f32 = 14.0;

/// What the user asked of a branch row: a left-click arms a checkout, the
/// right-click menu arms a new branch from that row's tip, and a click on a
/// folder row toggles its collapsed state.
enum BranchAction {
    Checkout(String),
    CreateFrom { start_point: String, label: String },
    ToggleFolder(String),
}

/// The BRANCHES module plus its confirm and new-branch modals. No branch
/// click checks out directly -- everything routes through the arm-then-confirm
/// flow below.
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
            .show_inside(ui, |ui| branches_module(ui, tab));
    } else {
        branches_module(ui, tab);
    }
    confirm_checkout(ui, tab, jobs);
    confirm_new_branch(ui, tab, jobs);
}

/// The BRANCHES module proper. A click only *arms* an action; the job waits
/// for confirmation in the modals run by `show`.
fn branches_module(ui: &mut Ui, tab: &mut RepoTab) {
    let readout = format!("{:02}", tab.branches.len());
    let action = module(ui, None, "BRANCHES", &readout, |ui| {
        ScrollArea::vertical()
            .id_salt("branches")
            .auto_shrink([false, false])
            .show(ui, |ui| branches(ui, tab))
            .inner
    });
    match action {
        Some(BranchAction::Checkout(name)) => tab.pending_checkout = Some(name),
        Some(BranchAction::CreateFrom { start_point, label }) => {
            tab.pending_branch = Some(NewBranch::new(start_point, label));
        }
        Some(BranchAction::ToggleFolder(path)) => {
            // Absent = expanded, so toggling is remove-or-insert.
            if !tab.collapsed_folders.remove(&path) {
                tab.collapsed_folders.insert(path);
            }
        }
        None => {}
    }
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

/// A node in the branch tree. A leaf points back into `tab.branches` by index;
/// a folder owns an ordered child list (which may mix folders and leaves).
enum Node {
    Leaf(usize),
    Folder {
        name: String,
        /// Full `/`-joined path, the collapse-state key (`origin/feature`).
        path: String,
        children: Vec<Node>,
    },
}

/// Inserts one branch -- pre-split into `/` segments -- into the tree, creating
/// folders along the way. The flat list is name-sorted, so all branches under a
/// folder are contiguous and the live folder is whichever matching one sits
/// nearest the end; scanning from the back finds it.
fn insert(level: &mut Vec<Node>, segs: &[&str], prefix: &str, idx: usize) {
    if segs.len() == 1 {
        level.push(Node::Leaf(idx));
        return;
    }
    let folder = segs[0];
    let path = if prefix.is_empty() {
        folder.to_string()
    } else {
        format!("{prefix}/{folder}")
    };
    let pos = level
        .iter()
        .rposition(|n| matches!(n, Node::Folder { name, .. } if name == folder));
    match pos {
        Some(p) => {
            if let Node::Folder { children, .. } = &mut level[p] {
                insert(children, &segs[1..], &path, idx);
            }
        }
        None => {
            let mut children = Vec::new();
            insert(&mut children, &segs[1..], &path, idx);
            level.push(Node::Folder {
                name: folder.to_string(),
                path,
                children,
            });
        }
    }
}

/// Groups the already-ordered flat branch list into a tree by `/` segments.
/// Child order is the branch order, so folders and leaves interleave by name.
fn build_tree(branches: &[BranchInfo]) -> Vec<Node> {
    let mut roots = Vec::new();
    for (i, b) in branches.iter().enumerate() {
        let segs: Vec<&str> = b.name.split('/').collect();
        insert(&mut roots, &segs, "", i);
    }
    roots
}

/// One drawn line of the tree, after collapsed folders have hidden their
/// subtrees. `depth` drives indentation; `lanes`/`is_last` drive the guide
/// lines (see `connectors`).
#[derive(Clone, Copy)]
struct Row<'a> {
    depth: usize,
    /// Bit `c` set = an ancestor's subtree continues past this row, so a
    /// vertical pass-through line is drawn in indent column `c`.
    lanes: u64,
    /// This node is the last child of its parent, so its elbow is an `L`
    /// rather than a `T`.
    is_last: bool,
    kind: RowKind<'a>,
}

#[derive(Clone, Copy)]
enum RowKind<'a> {
    Folder {
        name: &'a str,
        path: &'a str,
        collapsed: bool,
    },
    Branch(&'a BranchInfo),
}

/// Walks the tree into the visible row list, skipping the children of any
/// collapsed folder. `lanes` accumulates which indent columns still have a
/// vertical guide line at this depth.
fn flatten<'a>(
    nodes: &'a [Node],
    branches: &'a [BranchInfo],
    collapsed: &HashSet<String>,
    depth: usize,
    lanes: u64,
    out: &mut Vec<Row<'a>>,
) {
    let last = nodes.len().saturating_sub(1);
    for (i, n) in nodes.iter().enumerate() {
        let is_last = i == last;
        match n {
            Node::Leaf(j) => out.push(Row {
                depth,
                lanes,
                is_last,
                kind: RowKind::Branch(&branches[*j]),
            }),
            Node::Folder {
                name,
                path,
                children,
            } => {
                let is_collapsed = collapsed.contains(path.as_str());
                out.push(Row {
                    depth,
                    lanes,
                    is_last,
                    kind: RowKind::Folder {
                        name,
                        path,
                        collapsed: is_collapsed,
                    },
                });
                if !is_collapsed {
                    // A non-last folder leaves a pass-through lane in its own
                    // elbow column for everything below it.
                    let child_lanes = if depth >= 1 && !is_last {
                        lanes | 1 << (depth - 1)
                    } else {
                        lanes
                    };
                    flatten(children, branches, collapsed, depth + 1, child_lanes, out);
                }
            }
        }
    }
}

/// Draws the tree guide lines for one row: vertical pass-throughs for ancestor
/// lanes, plus the row's own elbow (`├` when more siblings follow, `└` when
/// last). Columns align under the parent chevron one level up.
fn connectors(p: &Painter, rect: Rect, depth: usize, lanes: u64, is_last: bool) {
    if depth == 0 {
        return;
    }
    let stroke = Stroke::new(1.0, theme::alpha(DIM, 150));
    let col_x = |c: usize| rect.left() + 8.0 + c as f32 * INDENT;
    let mid = rect.center().y;
    // Ancestor pass-throughs occupy columns below the elbow.
    for c in 0..depth - 1 {
        if lanes & (1 << c) != 0 {
            p.line_segment(
                [pos2(col_x(c), rect.top()), pos2(col_x(c), rect.bottom())],
                stroke,
            );
        }
    }
    // Elbow: vertical down to the row centre (or full height if siblings
    // follow), then a horizontal stub reaching toward this row's marker.
    let ex = col_x(depth - 1);
    let v_bottom = if is_last { mid } else { rect.bottom() };
    p.line_segment([pos2(ex, rect.top()), pos2(ex, v_bottom)], stroke);
    p.line_segment([pos2(ex, mid), pos2(col_x(depth) - 5.0, mid)], stroke);
}

/// A folder's collapse marker: a small filled triangle, right-pointing when
/// collapsed and down-pointing when open. Painted rather than glyph'd -- the
/// remote node marker is painted for the same reason (Rajdhani lacks the glyph).
fn chevron(p: &Painter, c: Pos2, collapsed: bool) {
    let s = 3.5;
    let pts = if collapsed {
        vec![
            pos2(c.x - s * 0.6, c.y - s),
            pos2(c.x - s * 0.6, c.y + s),
            pos2(c.x + s * 0.8, c.y),
        ]
    } else {
        vec![
            pos2(c.x - s, c.y - s * 0.6),
            pos2(c.x + s, c.y - s * 0.6),
            pos2(c.x, c.y + s * 0.8),
        ]
    };
    p.add(Shape::convex_polygon(pts, DIM, Stroke::NONE));
}

/// Draws the branch tree and returns the action the user asked for, if any: a
/// left-click on a non-HEAD branch arms a checkout, the right-click menu arms a
/// new branch from that row, and a click on a folder toggles it. The caller
/// confirms branch actions before acting.
fn branches(ui: &mut Ui, tab: &RepoTab) -> Option<BranchAction> {
    let font = TextStyle::Body.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Body) + 6.0;

    let tree = build_tree(&tab.branches);
    let mut rows = Vec::new();
    flatten(
        &tree,
        &tab.branches,
        &tab.collapsed_folders,
        0,
        0,
        &mut rows,
    );

    let mut action = None;
    for row in &rows {
        let (rect, resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let p = ui.painter().with_clip_rect(rect);
        let y = rect.center().y;
        let depth = row.depth;
        let indent = depth as f32 * INDENT;
        connectors(&p, rect, depth, row.lanes, row.is_last);
        match row.kind {
            RowKind::Folder {
                name,
                path,
                collapsed,
            } => {
                if resp.hovered() {
                    p.rect_filled(rect, CornerRadius::ZERO, BG3);
                }
                let cx = rect.left() + 8.0 + indent;
                chevron(&p, pos2(cx, y), collapsed);
                // Descender from the chevron down to the first child's elbow.
                if !collapsed {
                    p.line_segment(
                        [pos2(cx, y), pos2(cx, rect.bottom())],
                        Stroke::new(1.0, theme::alpha(DIM, 150)),
                    );
                }
                p.text(
                    pos2(rect.left() + 20.0 + indent, y),
                    Align2::LEFT_CENTER,
                    name,
                    font.clone(),
                    DIM,
                );
                if resp.clicked() {
                    action = Some(BranchAction::ToggleFolder(path.to_string()));
                }
            }
            RowKind::Branch(b) => {
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
                        Stroke::new(1.0, PURPLE),
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
        }
    }
    action
}

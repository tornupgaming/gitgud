//! The central COMMIT GRAPH module: virtualized multi-lane commit graph
//! with a WIP row, ref chips, and load-more paging.

use eframe::egui::{
    Align2, Color32, CornerRadius, Painter, Rect, ScrollArea, Sense, Shape, Stroke, StrokeKind,
    TextStyle, Ui, epaint::CubicBezierShape, pos2, vec2,
};

use super::{HudButton, module, rel_time, text_width, truncate_right};
use crate::app::{NewBranch, RepoTab};
use crate::git::graph::Edge;
use crate::git::{DiffKey, Job};
use crate::theme::{self, ACCENT, AMBER, BG0, BG3, DIM, LANES, PURPLE, TEXT};

/// Horizontal pitch between graph lanes.
const LANE_W: f32 = 14.0;
/// Lanes beyond this are clamped onto the column's right edge so monster
/// repos don't push the text off-screen.
const MAX_VIS_LANES: u16 = 8;

/// Row geometry shared by every row painter.
struct Geom {
    body_h: f32,
    small_h: f32,
    row_h: f32,
    /// Vertical distance between consecutive row centerlines (row height plus
    /// the ui's item spacing) -- edges must span this, not just `row_h`.
    pitch: f32,
    graph_w: f32,
    max_lane: u16,
}

enum Action {
    Select(String),
    /// Clear selection + diffs; also fired by the WIP row to rebind the right
    /// panel to the working tree.
    Deselect,
    /// Arm the new-branch dialog at a commit: (full id, short id for display).
    CreateBranch(String, String),
    LoadMore,
}

/// The COMMIT GRAPH module: optional WIP row, virtualized commit rows, and
/// the LOAD MORE trigger. Row clicks are recorded as an `Action` and applied
/// after the scroll area ends, because the rows borrow `tab`.
pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    let body_h = ui.text_style_height(&TextStyle::Body);
    let small_h = ui.text_style_height(&TextStyle::Small);
    let row_h = body_h + small_h + 8.0;
    let vis_lanes = tab.graph.lanes.clamp(1, MAX_VIS_LANES);
    let g = Geom {
        body_h,
        small_h,
        row_h,
        pitch: row_h + ui.spacing().item_spacing.y,
        graph_w: f32::from(vis_lanes) * LANE_W + 4.0,
        max_lane: vis_lanes - 1,
    };

    let wip = !tab.staged.is_empty() || !tab.unstaged.is_empty();
    let wip_rows = usize::from(wip);
    let total = wip_rows + tab.commits.len() + usize::from(!tab.history_done);
    let mut action: Option<Action> = None;

    let readout = format!(
        "{}{} ENTRIES",
        tab.commits.len(),
        if tab.history_done { "" } else { "+" }
    );
    module(ui, None, "COMMIT GRAPH", &readout, |ui| {
        ScrollArea::vertical()
            .id_salt("graph")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, total, |ui, range| {
                let first = range.start;
                for i in range {
                    if wip && i == 0 {
                        wip_row(ui, tab, &g, &mut action);
                    } else if i - wip_rows == tab.commits.len() {
                        let btn = HudButton::new("▼ LOAD MORE")
                            .text_color(DIM)
                            .min_size(vec2(ui.available_width(), row_h));
                        if ui.add(btn).clicked() {
                            action = Some(Action::LoadMore);
                        }
                    } else {
                        let ci = i - wip_rows;
                        // The first visible row also draws the previous row's
                        // edges so segments entering from above don't pop in.
                        commit_row(ui, tab, ci, &g, i == first && ci > 0, &mut action);
                    }
                }
            });
    });

    match action {
        Some(Action::LoadMore) => jobs.push(Job::LoadHistory {
            id: tab.id,
            skip: tab.commits.len(),
        }),
        Some(Action::Select(id)) => {
            let key = DiffKey::Commit { id };
            tab.selection = Some(key.clone());
            tab.diff = None;
            tab.commit_diff = None;
            tab.commit_file = None;
            jobs.push(Job::LoadDiff { id: tab.id, key });
        }
        Some(Action::Deselect) => {
            tab.selection = None;
            tab.diff = None;
            tab.commit_diff = None;
            tab.commit_file = None;
        }
        Some(Action::CreateBranch(start_point, label)) => {
            tab.pending_branch = Some(NewBranch::new(start_point, label));
        }
        None => {}
    }
}

fn lane_x(left: f32, lane: u16) -> f32 {
    left + LANE_W / 2.0 + f32::from(lane) * LANE_W
}

/// Draw one row's edge segments from its centerline (`y_top`) to the next
/// row's. Stubs (last loaded row, parents not paged in yet) run half length
/// and faded so the graph visibly continues off the bottom.
fn draw_edges(p: &Painter, left: f32, y_top: f32, g: &Geom, edges: &[Edge], stub: bool) {
    let (y_bot, a) = if stub {
        (y_top + g.pitch * 0.5, 120)
    } else {
        (y_top + g.pitch, 255)
    };
    for e in edges {
        let color = theme::alpha(LANES[e.color as usize % LANES.len()], a);
        let x0 = lane_x(left, e.from.min(g.max_lane));
        let x1 = lane_x(left, e.to.min(g.max_lane));
        let stroke = Stroke::new(1.5, color);
        if x0 == x1 {
            p.line_segment([pos2(x0, y_top), pos2(x0, y_bot)], stroke);
        } else {
            // Both control points at the vertical midpoint give the
            // GitKraken-style S-curve between lanes.
            let ym = (y_top + y_bot) / 2.0;
            p.add(CubicBezierShape::from_points_stroke(
                [pos2(x0, y_top), pos2(x0, ym), pos2(x1, ym), pos2(x1, y_bot)],
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        }
    }
}

fn commit_row(
    ui: &mut Ui,
    tab: &RepoTab,
    ci: usize,
    g: &Geom,
    draw_prev: bool,
    action: &mut Option<Action>,
) {
    let c = &tab.commits[ci];
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), g.row_h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let selected = matches!(&tab.selection, Some(DiffKey::Commit { id }) if *id == c.id);
    let p = ui.painter().with_clip_rect(rect);
    if selected {
        p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(ACCENT, 24));
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, BG3);
    }

    let y1 = rect.top() + 4.0 + g.body_h / 2.0;
    let y2 = rect.bottom() - 4.0 - g.small_h / 2.0;

    // Edges use the unclipped painter so they bridge the row spacing; the
    // graph layout may briefly lag the commit list, so missing rows fall
    // back to no edges / lane 0.
    let up = ui.painter();
    if draw_prev && let Some(prev) = tab.graph.rows.get(ci - 1) {
        draw_edges(up, rect.left(), y1 - g.pitch, g, &prev.edges, false);
    }
    let row = tab.graph.rows.get(ci);
    if let Some(row) = row {
        draw_edges(
            up,
            rect.left(),
            y1,
            g,
            &row.edges,
            ci + 1 == tab.commits.len(),
        );
    }

    // Node on top of its edges: merges a filled diamond, regular commits a
    // ringed dot.
    let lane = row.map_or(0, |r| r.lane);
    let node = pos2(lane_x(rect.left(), lane.min(g.max_lane)), y1);
    let color = LANES[lane as usize % LANES.len()];
    if c.is_merge {
        p.add(Shape::convex_polygon(
            vec![
                pos2(node.x, node.y - 4.0),
                pos2(node.x + 4.0, node.y),
                pos2(node.x, node.y + 4.0),
                pos2(node.x - 4.0, node.y),
            ],
            color,
            Stroke::NONE,
        ));
    } else {
        p.circle_stroke(node, 3.5, Stroke::new(1.0, color));
        p.circle_filled(node, 1.5, color);
    }

    let body_font = TextStyle::Body.resolve(ui.style());
    let small_font = TextStyle::Small.resolve(ui.style());
    let text_x = rect.left() + g.graph_w + 6.0;

    // Leave room for the ref chips (their text plus 8px of box padding and a
    // 6px gap) so the summary truncates, not the chips.
    let chips_w: f32 = c
        .refs
        .iter()
        .map(|r| text_width(ui, r, &small_font) + 14.0)
        .sum();
    let avail = (rect.right() - 6.0 - text_x - chips_w).max(text_width(ui, "…", &body_font));
    let summary = truncate_right(ui, &c.summary, &body_font, avail);
    let sum_rect = p.text(
        pos2(text_x, y1),
        Align2::LEFT_CENTER,
        summary,
        body_font,
        TEXT,
    );

    let mut cx = sum_rect.right() + 8.0;
    for r in &c.refs {
        let color = if r.contains('/') { PURPLE } else { AMBER };
        let w = text_width(ui, r, &small_font) + 8.0;
        let chip = Rect::from_min_size(
            pos2(cx, y1 - g.small_h / 2.0 - 2.0),
            vec2(w, g.small_h + 4.0),
        );
        p.rect_filled(chip, CornerRadius::ZERO, theme::alpha(color, 26));
        p.rect_stroke(
            chip,
            CornerRadius::ZERO,
            Stroke::new(1.0, color),
            StrokeKind::Inside,
        );
        p.text(
            chip.center(),
            Align2::CENTER_CENTER,
            r,
            small_font.clone(),
            color,
        );
        cx += w + 6.0;
    }

    let meta = format!("{} · {} · {}", c.short, c.author, rel_time(c.time));
    let meta = truncate_right(ui, &meta, &small_font, rect.right() - 6.0 - text_x);
    p.text(pos2(text_x, y2), Align2::LEFT_CENTER, meta, small_font, DIM);

    // Selected rows get the glowing chamfered frame, the HUD "target lock".
    if selected {
        theme::glow_outline(
            &p,
            theme::chamfer(rect.shrink(1.0), 6.0),
            theme::alpha(ACCENT, 200),
        );
    }

    if resp.clicked() {
        // Clicking the selected commit toggles it off.
        *action = Some(if selected {
            Action::Deselect
        } else {
            Action::Select(c.id.clone())
        });
    }
    super::hud_menu(&resp, |ui| {
        if ui.button("Create branch from here").clicked() {
            *action = Some(Action::CreateBranch(c.id.clone(), c.short.clone()));
            ui.close();
        }
    });
    resp.on_hover_text(format!("{}\n{} <{}>", c.id, c.author, c.email));
}

/// Uncommitted-changes row pinned above the history: a pending node on the
/// HEAD commit's lane with a dashed drop-line into it.
fn wip_row(ui: &mut Ui, tab: &RepoTab, g: &Geom, action: &mut Option<Action>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), g.row_h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let p = ui.painter().with_clip_rect(rect);
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, BG3);
    } else if tab.selection.is_none() {
        // Subtle bind indicator: the right panel is showing the working tree.
        p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(ACCENT, 12));
    }

    let lane = tab
        .commits
        .iter()
        .position(|c| c.refs.iter().any(|r| r == &tab.head))
        .and_then(|i| tab.graph.rows.get(i))
        .map_or(0, |r| r.lane);
    let x = lane_x(rect.left(), lane.min(g.max_lane));
    let y1 = rect.top() + 4.0 + g.body_h / 2.0;

    // Dashed connector down toward the HEAD commit's node, unclipped so it
    // bridges the row gap.
    ui.painter().extend(Shape::dashed_line(
        &[pos2(x, y1 + 6.0), pos2(x, y1 + g.pitch)],
        Stroke::new(1.0, DIM),
        3.0,
        3.0,
    ));
    // Hollow ring with a void-filled center reads as "pending".
    p.circle_stroke(
        pos2(x, y1),
        4.0,
        Stroke::new(1.5, theme::alpha(ACCENT, 200)),
    );
    p.circle_filled(pos2(x, y1), 2.0, BG0);

    let body_font = TextStyle::Body.resolve(ui.style());
    let small_font = TextStyle::Small.resolve(ui.style());
    let text_x = rect.left() + g.graph_w + 6.0;
    let wip_rect = p.text(
        pos2(text_x, y1),
        Align2::LEFT_CENTER,
        "// WIP",
        body_font,
        ACCENT,
    );

    let label = format!("{} CHANGES", tab.staged.len() + tab.unstaged.len());
    let w = text_width(ui, &label, &small_font) + 8.0;
    let chip = Rect::from_min_size(
        pos2(wip_rect.right() + 8.0, y1 - g.small_h / 2.0 - 2.0),
        vec2(w, g.small_h + 4.0),
    );
    p.rect_filled(chip, CornerRadius::ZERO, theme::alpha(ACCENT, 26));
    p.rect_stroke(
        chip,
        CornerRadius::ZERO,
        Stroke::new(1.0, ACCENT),
        StrokeKind::Inside,
    );
    p.text(
        chip.center(),
        Align2::CENTER_CENTER,
        label,
        small_font.clone(),
        ACCENT,
    );

    let y2 = rect.bottom() - 4.0 - g.small_h / 2.0;
    p.text(
        pos2(text_x, y2),
        Align2::LEFT_CENTER,
        "uncommitted changes",
        small_font,
        DIM,
    );

    if resp.clicked() {
        *action = Some(Action::Deselect);
    }
}

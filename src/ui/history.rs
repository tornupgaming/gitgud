//! Virtualized commit history list ("history" side panel).

use eframe::egui::{
    Align2, CornerRadius, Rect, ScrollArea, Sense, Stroke, StrokeKind, TextStyle, Ui, pos2, vec2,
};

use super::{HudButton, section_header, text_width, truncate_right};
use crate::app::RepoTab;
use crate::git::{DiffKey, Job};
use crate::theme::{self, ACCENT, AMBER, BG3, BORDER, DIM, PURPLE, TEXT};

/// Width of the commit-graph node column.
const NODE_COL: f32 = 18.0;

pub fn show(ui: &mut Ui, tab: &mut RepoTab, jobs: &mut Vec<Job>) {
    section_header(ui, &format!("◤ HISTORY — {}", tab.head));

    let body_h = ui.text_style_height(&TextStyle::Body);
    let small_h = ui.text_style_height(&TextStyle::Small);
    let row_h = body_h + small_h + 8.0;

    // One extra virtual row at the end hosts the "load more" button.
    let total = tab.commits.len() + usize::from(!tab.history_done);
    let mut select: Option<String> = None;
    let mut load_more = false;

    ScrollArea::vertical()
        .id_salt("history")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, total, |ui, range| {
            for i in range {
                if i == tab.commits.len() {
                    let btn = HudButton::new("▼ LOAD MORE")
                        .text_color(DIM)
                        .min_size(vec2(ui.available_width(), row_h));
                    if ui.add(btn).clicked() {
                        load_more = true;
                    }
                } else {
                    commit_row(ui, tab, i, (body_h, small_h), &mut select);
                }
            }
        });

    if load_more {
        jobs.push(Job::LoadHistory {
            id: tab.id,
            skip: tab.commits.len(),
        });
    }
    if let Some(id) = select {
        let key = DiffKey::Commit { id };
        tab.selection = Some(key.clone());
        tab.diff = None;
        jobs.push(Job::LoadDiff { id: tab.id, key });
    }
}

fn commit_row(
    ui: &mut Ui,
    tab: &RepoTab,
    i: usize,
    heights: (f32, f32),
    select: &mut Option<String>,
) {
    let c = &tab.commits[i];
    let (body_h, small_h) = heights;
    let row_h = body_h + small_h + 8.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }

    let selected = matches!(&tab.selection, Some(DiffKey::Commit { id }) if *id == c.id);
    let p = ui.painter().with_clip_rect(rect);
    if selected {
        p.rect_filled(rect, CornerRadius::ZERO, theme::alpha(ACCENT, 30));
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::ZERO, BG3);
    }

    // Graph rail: drawn unclipped so it bridges the spacing between rows.
    let node_x = rect.left() + NODE_COL / 2.0;
    let gap = ui.spacing().item_spacing.y / 2.0;
    ui.painter().line_segment(
        [
            pos2(node_x, rect.top() - gap),
            pos2(node_x, rect.bottom() + gap),
        ],
        Stroke::new(1.0, BORDER),
    );

    let body_font = TextStyle::Body.resolve(ui.style());
    let small_font = TextStyle::Small.resolve(ui.style());
    let y1 = rect.top() + 4.0 + body_h / 2.0;
    let y2 = rect.bottom() - 4.0 - small_h / 2.0;

    let glyph = if c.is_merge { "◆" } else { "●" };
    p.text(
        pos2(node_x, y1),
        Align2::CENTER_CENTER,
        glyph,
        body_font.clone(),
        DIM,
    );

    let text_x = rect.left() + NODE_COL + 6.0;

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
        let chip = Rect::from_min_size(pos2(cx, y1 - small_h / 2.0 - 2.0), vec2(w, small_h + 4.0));
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

    if resp.clicked() {
        *select = Some(c.id.clone());
    }
    resp.on_hover_text(format!("{}\n{} <{}>", c.id, c.author, c.email));
}

fn rel_time(commit_time: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let s = (now - commit_time).max(0);
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    if s < MIN {
        "now".to_owned()
    } else if s < HOUR {
        format!("{}m ago", s / MIN)
    } else if s < DAY {
        format!("{}h ago", s / HOUR)
    } else if s < MONTH {
        format!("{}d ago", s / DAY)
    } else if s < YEAR {
        format!("{}mo ago", s / MONTH)
    } else {
        format!("{}y ago", s / YEAR)
    }
}

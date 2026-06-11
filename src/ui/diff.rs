//! Central diff viewer (the DIFF ANALYSIS module). The diff arrives as nested
//! `DiffData`; we flatten it once into `DiffView` rows so the per-frame render
//! is a plain index walk that `show_rows` can virtualize.

use eframe::egui::{
    Align2, CornerRadius, Rect, ScrollArea, Sense, Spinner, Stroke, StrokeKind, TextStyle, Ui,
    pos2, vec2,
};

use super::{char_w, module, text_width, truncate_right};
use crate::app::RepoTab;
use crate::git::{DiffData, DiffKey, DiffLine, LineKind};
use crate::theme::{self, ACCENT, AMBER, BG2, BORDER, BRIGHT, DIM, GREEN, PURPLE, RED, TEXT};

/// Right-aligned digits reserved per line-number gutter column.
const GUTTER_CHARS: f32 = 4.0;
/// Segmented add/delete ratio bar: cell count and geometry.
const RATIO_CELLS: usize = 24;
const RATIO_CELL_W: f32 = 4.0;
const RATIO_GAP: f32 = 2.0;
const RATIO_BAR_W: f32 = RATIO_CELLS as f32 * (RATIO_CELL_W + RATIO_GAP) - RATIO_GAP;

pub struct DiffView {
    pub adds: usize,
    pub dels: usize,
    pub rows: Vec<DiffRow>,
}

pub enum DiffRow {
    File {
        path: String,
        adds: usize,
        dels: usize,
    },
    Binary,
    Line(DiffLine),
}

impl DiffView {
    pub fn build(data: DiffData) -> Self {
        let mut rows = Vec::new();
        for f in data.files {
            let binary = f.binary;
            rows.push(DiffRow::File {
                path: f.path,
                adds: f.adds,
                dels: f.dels,
            });
            if binary {
                rows.push(DiffRow::Binary);
            } else {
                rows.extend(f.lines.into_iter().map(DiffRow::Line));
            }
        }
        Self {
            adds: data.adds,
            dels: data.dels,
            rows,
        }
    }
}

pub fn show(ui: &mut Ui, tab: &RepoTab) {
    let readout = match (&tab.selection, &tab.diff) {
        (None, _) => "STANDBY".to_owned(),
        (Some(_), None) => "SCANNING".to_owned(),
        (Some(_), Some(v)) => format!("+{} −{}", v.adds, v.dels),
    };
    module(ui, None, "DIFF ANALYSIS", &readout, |ui| {
        let Some(key) = &tab.selection else {
            standby(ui);
            return;
        };
        let Some(view) = &tab.diff else {
            // Selection made but the worker hasn't answered yet.
            ui.add_space(ui.available_height() * 0.45);
            ui.vertical_centered(|ui| ui.add(Spinner::new().size(24.0).color(ACCENT)));
            return;
        };
        header(ui, tab, key, view);
        ui.add_space(4.0);
        body(ui, view);
    });
}

/// Idle screen: a bracketed "no target" readout in the module's center.
fn standby(ui: &mut Ui) {
    ui.add_space(ui.available_height() * 0.45);
    let cx = ui.max_rect().center().x;
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let text = theme::tracked_text(
        ui.painter(),
        pos2(cx, r.center().y),
        Align2::CENTER_CENTER,
        "AWAITING TARGET · SELECT A FILE OR COMMIT",
        theme::display_font(12.0),
        DIM,
        2.0,
    );
    theme::corner_brackets(
        ui.painter(),
        text.expand2(vec2(14.0, 8.0)),
        8.0,
        Stroke::new(1.0, theme::alpha(ACCENT, 120)),
    );
}

fn header(ui: &mut Ui, tab: &RepoTab, key: &DiffKey, view: &DiffView) {
    let (tag, tag_color, title) = match key {
        DiffKey::Workdir { path, staged } => {
            if *staged {
                ("STAGED", GREEN, path.clone())
            } else {
                ("UNSTAGED", AMBER, path.clone())
            }
        }
        DiffKey::Commit { id } => {
            let title = match tab.commits.iter().find(|c| c.id == *id) {
                Some(c) => format!("{} · {}", c.short, c.summary),
                None => id[..id.len().min(7)].to_owned(),
            };
            ("COMMIT", PURPLE, title)
        }
    };

    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let p = ui.painter();
    let y = rect.center().y;
    let small = TextStyle::Small.resolve(ui.style());
    let body = TextStyle::Body.resolve(ui.style());

    let tag_w = text_width(ui, tag, &small) + 12.0;
    let chip = Rect::from_min_size(pos2(rect.left(), y - 9.0), vec2(tag_w, 18.0));
    p.rect_filled(chip, CornerRadius::ZERO, theme::alpha(tag_color, 26));
    p.rect_stroke(
        chip,
        CornerRadius::ZERO,
        Stroke::new(1.0, tag_color),
        StrokeKind::Inside,
    );
    p.text(chip.center(), Align2::CENTER_CENTER, tag, small, tag_color);

    let bar = Rect::from_min_size(
        pos2(rect.right() - RATIO_BAR_W, y - 4.0),
        vec2(RATIO_BAR_W, 8.0),
    );
    ratio_bar(p, bar, view.adds, view.dels);
    let r = p.text(
        pos2(bar.left() - 12.0, y),
        Align2::RIGHT_CENTER,
        format!("−{}", view.dels),
        body.clone(),
        RED,
    );
    let r = p.text(
        pos2(r.left() - 8.0, y),
        Align2::RIGHT_CENTER,
        format!("+{}", view.adds),
        body.clone(),
        GREEN,
    );

    let text_x = chip.right() + 10.0;
    let title = truncate_right(ui, &title, &body, r.left() - 12.0 - text_x);
    p.text(pos2(text_x, y), Align2::LEFT_CENTER, title, body, BRIGHT);

    theme::fade_rule(
        p,
        pos2(rect.left(), rect.bottom() + 1.0),
        rect.width() * 0.7,
        ACCENT,
    );
}

/// Add/delete ratio as a row of discrete cells -- an instrument readout, not
/// a progress bar.
fn ratio_bar(p: &eframe::egui::Painter, rect: Rect, adds: usize, dels: usize) {
    let total = adds + dels;
    let green_cells = if total == 0 {
        0
    } else {
        // Round so a tiny-but-nonzero side still shows at least one cell.
        ((adds as f32 / total as f32 * RATIO_CELLS as f32).round() as usize)
            .clamp(usize::from(adds > 0), RATIO_CELLS - usize::from(dels > 0))
    };
    for i in 0..RATIO_CELLS {
        let x = rect.left() + i as f32 * (RATIO_CELL_W + RATIO_GAP);
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(RATIO_CELL_W, rect.height()));
        let color = if total == 0 {
            BORDER
        } else if i < green_cells {
            GREEN
        } else {
            RED
        };
        p.rect_filled(cell, CornerRadius::ZERO, color);
    }
}

fn body(ui: &mut Ui, view: &DiffView) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.text_style_height(&TextStyle::Monospace) + 2.0;
    let cw = char_w(ui, &font);
    let gutter_w = cw * 2.0 * GUTTER_CHARS + 16.0;

    ScrollArea::vertical()
        .id_salt("diff")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, view.rows.len(), |ui, range| {
            for i in range {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let p = ui.painter().with_clip_rect(rect);
                let y = rect.center().y;
                match &view.rows[i] {
                    DiffRow::File { path, adds, dels } => {
                        p.rect_filled(rect, CornerRadius::ZERO, BG2);
                        p.line_segment(
                            [rect.left_bottom(), rect.right_bottom()],
                            Stroke::new(1.0, theme::alpha(ACCENT, 90)),
                        );
                        let r = p.text(
                            pos2(rect.right() - 6.0, y),
                            Align2::RIGHT_CENTER,
                            format!("−{dels}"),
                            font.clone(),
                            RED,
                        );
                        p.text(
                            pos2(r.left() - 8.0, y),
                            Align2::RIGHT_CENTER,
                            format!("+{adds}"),
                            font.clone(),
                            GREEN,
                        );
                        p.text(
                            pos2(rect.left() + 8.0, y),
                            Align2::LEFT_CENTER,
                            format!("▶ {path}"),
                            font.clone(),
                            BRIGHT,
                        );
                    }
                    DiffRow::Binary => {
                        p.text(
                            pos2(rect.left() + gutter_w, y),
                            Align2::LEFT_CENTER,
                            "binary file",
                            font.clone(),
                            DIM,
                        );
                    }
                    DiffRow::Line(l) => {
                        let (color, bg) = match l.kind {
                            LineKind::Add => (GREEN, Some(theme::alpha(GREEN, 14))),
                            LineKind::Del => (RED, Some(theme::alpha(RED, 14))),
                            LineKind::Hunk => {
                                (theme::alpha(ACCENT, 170), Some(theme::alpha(ACCENT, 10)))
                            }
                            LineKind::Ctx => (TEXT, None),
                        };
                        if let Some(bg) = bg {
                            p.rect_filled(rect, CornerRadius::ZERO, bg);
                        }
                        if l.kind != LineKind::Hunk {
                            if l.old_no > 0 {
                                p.text(
                                    pos2(rect.left() + cw * GUTTER_CHARS + 4.0, y),
                                    Align2::RIGHT_CENTER,
                                    l.old_no,
                                    font.clone(),
                                    DIM,
                                );
                            }
                            if l.new_no > 0 {
                                p.text(
                                    pos2(rect.left() + cw * 2.0 * GUTTER_CHARS + 8.0, y),
                                    Align2::RIGHT_CENTER,
                                    l.new_no,
                                    font.clone(),
                                    DIM,
                                );
                            }
                            // Gutter rule separating line numbers from code.
                            let gx = rect.left() + gutter_w - 6.0;
                            p.line_segment(
                                [pos2(gx, rect.top()), pos2(gx, rect.bottom())],
                                Stroke::new(1.0, theme::alpha(BORDER, 160)),
                            );
                        }
                        p.text(
                            pos2(rect.left() + gutter_w, y),
                            Align2::LEFT_CENTER,
                            &l.text,
                            font.clone(),
                            color,
                        );
                    }
                }
            }
        });
}

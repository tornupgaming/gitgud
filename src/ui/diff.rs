//! Central diff viewer. The diff arrives as nested `DiffData`; we flatten it
//! once into `DiffView` rows so the per-frame render is a plain index walk
//! that `show_rows` can virtualize.

use eframe::egui::{
    Align, Align2, CornerRadius, Frame, Label, Layout, Margin, Rect, RichText, ScrollArea, Sense,
    Spinner, Stroke, TextStyle, Ui, pos2, vec2,
};

use super::char_w;
use crate::app::RepoTab;
use crate::git::{DiffData, DiffKey, DiffLine, LineKind};
use crate::theme::{self, ACCENT, BG2, BORDER, DIM, GREEN, RED, TEXT};

/// Right-aligned digits reserved per line-number gutter column.
const GUTTER_CHARS: f32 = 4.0;
const RATIO_BAR_W: f32 = 120.0;

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
    let Some(key) = &tab.selection else {
        theme::backdrop(ui.painter(), ui.max_rect());
        ui.add_space(ui.available_height() * 0.45);
        ui.vertical_centered(|ui| ui.label(RichText::new("◧ select a file or commit").color(DIM)));
        return;
    };
    let Some(view) = &tab.diff else {
        // Selection made but the worker hasn't answered yet.
        theme::backdrop(ui.painter(), ui.max_rect());
        ui.add_space(ui.available_height() * 0.45);
        ui.vertical_centered(|ui| ui.add(Spinner::new().size(24.0).color(ACCENT)));
        return;
    };

    // Header sits on its own band: a darker plate with a left accent tick and
    // an accent rule fading off below it.
    let band = Frame::new()
        .fill(BG2)
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| header(ui, tab, key, view))
        .response
        .rect;
    let p = ui.painter();
    p.rect_filled(
        Rect::from_min_size(band.min, vec2(3.0, band.height())),
        CornerRadius::ZERO,
        ACCENT,
    );
    p.line_segment(
        [
            pos2(band.left(), band.bottom()),
            pos2(band.right(), band.bottom()),
        ],
        Stroke::new(1.0, theme::alpha(ACCENT, 60)),
    );
    theme::fade_rule(
        p,
        pos2(band.left(), band.bottom() + 2.0),
        band.width() * 0.6,
        ACCENT,
    );
    ui.add_space(3.0);
    body(ui, view);
}

fn header(ui: &mut Ui, tab: &RepoTab, key: &DiffKey, view: &DiffView) {
    let title = match key {
        DiffKey::Workdir { path, staged } => {
            format!("{path} — {}", if *staged { "staged" } else { "unstaged" })
        }
        DiffKey::Commit { id } => match tab.commits.iter().find(|c| c.id == *id) {
            Some(c) => format!("commit {} — {}", c.short, c.summary),
            None => format!("commit {}", &id[..id.len().min(7)]),
        },
    };
    // The right-to-left ui must live inside `horizontal`: a bare `with_layout`
    // grabs ALL remaining panel height and would center this row in it,
    // leaving no room for the diff body below.
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ratio_bar(ui, view.adds, view.dels);
            ui.label(RichText::new(format!("−{}", view.dels)).color(RED));
            ui.label(RichText::new(format!("+{}", view.adds)).color(GREEN));
            ui.add(Label::new(RichText::new(title).color(TEXT)).truncate());
        });
    });
}

fn ratio_bar(ui: &mut Ui, adds: usize, dels: usize) {
    let (rect, _) = ui.allocate_exact_size(vec2(RATIO_BAR_W, 8.0), Sense::hover());
    let p = ui.painter();
    let total = adds + dels;
    if total == 0 {
        p.rect_filled(rect, CornerRadius::ZERO, BORDER);
        return;
    }
    let split = rect.left() + rect.width() * adds as f32 / total as f32;
    p.rect_filled(
        Rect::from_min_max(rect.min, pos2(split, rect.bottom())),
        CornerRadius::ZERO,
        GREEN,
    );
    p.rect_filled(
        Rect::from_min_max(pos2(split, rect.top()), rect.max),
        CornerRadius::ZERO,
        RED,
    );
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
                        p.rect_filled(
                            Rect::from_min_size(rect.min, vec2(3.0, rect.height())),
                            CornerRadius::ZERO,
                            ACCENT,
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
                            pos2(rect.left() + 10.0, y),
                            Align2::LEFT_CENTER,
                            path,
                            font.clone(),
                            TEXT,
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
                            LineKind::Hunk => (DIM, Some(ui.visuals().faint_bg_color)),
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

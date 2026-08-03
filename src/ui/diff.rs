//! Central diff viewer (the DIFF ANALYSIS module). The diff arrives as nested
//! `DiffData`; we flatten it once into `DiffView` rows -- computing syntax
//! highlight spans in the same pass -- so the per-frame render is a plain
//! index walk that `show_rows` can virtualize.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Align, Align2, CornerRadius, Layout, Rect, Sense, Spinner, Stroke, StrokeKind, TextStyle, Ui,
    pos2, vec2,
};

use super::highlight::{LineHighlighter, Spans};
use super::{HudButton, char_w, module, text_width, truncate_right, vscroll};
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

/// Pre-flattened, pre-highlighted render model for one diff. Built once per
/// `Update::Diff` reply; per-frame rendering is an index walk over `rows`.
pub struct DiffView {
    pub adds: usize,
    pub dels: usize,
    pub rows: Vec<DiffRow>,
}

/// One virtualized row of the diff body.
pub enum DiffRow {
    File {
        path: String,
        adds: usize,
        dels: usize,
    },
    Binary,
    /// Spans are `None` for hunk headers and unrecognised languages.
    Line(DiffLine, Option<Spans>),
}

impl DiffView {
    /// One-time flatten of the whole diff, computing highlight spans in the
    /// same pass so the render path never touches syntect.
    pub fn build(data: DiffData) -> Self {
        let mut rows = Vec::new();
        for f in data.files {
            let binary = f.binary;
            let mut hl = LineHighlighter::for_path(&f.path);
            rows.push(DiffRow::File {
                path: f.path,
                adds: f.adds,
                dels: f.dels,
            });
            if binary {
                rows.push(DiffRow::Binary);
            } else {
                rows.extend(f.lines.into_iter().map(|l| {
                    let spans = (l.kind != LineKind::Hunk)
                        .then(|| hl.spans(&l.text))
                        .flatten();
                    DiffRow::Line(l, spans)
                }));
            }
        }
        Self {
            adds: data.adds,
            dels: data.dels,
            rows,
        }
    }

    /// View of one file out of an already-loaded commit diff (the right
    /// panel's file list). `None` when `path` isn't in `data`.
    pub fn build_one(data: &DiffData, path: &str) -> Option<Self> {
        let f = data.files.iter().find(|f| f.path == path)?.clone();
        Some(Self::build(DiffData {
            adds: f.adds,
            dels: f.dels,
            files: vec![f],
        }))
    }
}

/// The DIFF ANALYSIS module: header strip plus the virtualized body. ◀ GRAPH
/// backs out one level -- an open commit file returns to the graph with the
/// commit still selected, a workdir selection clears entirely.
pub fn show(ui: &mut Ui, tab: &mut RepoTab) {
    let readout = match &tab.diff {
        None => "SCANNING".to_owned(),
        Some(v) => format!("+{} −{}", v.adds, v.dels),
    };
    // The back click mutates `tab`, which the module closure borrows -- so
    // it's recorded here and applied after layout.
    let mut close = false;
    module(ui, None, "DIFF ANALYSIS", &readout, |ui| {
        // The center only routes here while something is selected.
        let Some(key) = &tab.selection else { return };
        let Some(view) = &tab.diff else {
            // Selection made but the worker hasn't answered yet.
            ui.add_space(ui.available_height() * 0.45);
            ui.vertical_centered(|ui| ui.add(Spinner::new().size(24.0).color(ACCENT)));
            return;
        };
        // Header row with the back-to-graph affordance on its right edge.
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui.add(HudButton::new("◀ GRAPH").small(true)).clicked();
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    header(ui, tab, key, view);
                });
            });
        });
        ui.add_space(4.0);
        body(ui, view);
    });
    if close {
        if tab.commit_file.is_some() {
            // Back to the graph; the commit stays selected in the right panel.
            tab.commit_file = None;
        } else {
            tab.selection = None;
        }
        tab.diff = None;
    }
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
        // Conflict selections route to the conflict view, never here.
        DiffKey::Conflict { path } => ("CONFLICT", RED, path.clone()),
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
        Stroke::new(1.0_f32, tag_color),
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
    // Fold the inter-item gap into the row itself and drop the gap, so the
    // add/del background tints on consecutive lines touch. Text pitch is
    // unchanged: same total height per row, glyphs centered in it.
    let row_h = ui.text_style_height(&TextStyle::Monospace) + 2.0 + ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    let cw = char_w(ui, &font);
    let gutter_w = cw * 2.0 * GUTTER_CHARS + 16.0;

    vscroll("diff").show_rows(ui, row_h, view.rows.len(), |ui, range| {
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
                        Stroke::new(1.0_f32, theme::alpha(ACCENT, 90)),
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
                DiffRow::Line(l, spans) => {
                    // Add/del reads off the background tint alone now that
                    // the text carries syntax colours, so it runs a touch
                    // stronger than the old 14.
                    let (color, bg) = match l.kind {
                        LineKind::Add => (TEXT, Some(theme::alpha(GREEN, 26))),
                        LineKind::Del => (TEXT, Some(theme::alpha(RED, 26))),
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
                            Stroke::new(1.0_f32, theme::alpha(BORDER, 160)),
                        );
                    }
                    match spans {
                        Some(spans) if !spans.is_empty() => {
                            let mut job = LayoutJob::default();
                            for (color, range) in spans {
                                job.append(
                                    &l.text[range.clone()],
                                    0.0,
                                    TextFormat {
                                        font_id: font.clone(),
                                        color: *color,
                                        ..Default::default()
                                    },
                                );
                            }
                            let galley = ui.fonts_mut(|f| f.layout_job(job));
                            let top = y - galley.size().y * 0.5;
                            p.galley(pos2(rect.left() + gutter_w, top), galley, TEXT);
                        }
                        _ => {
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
            }
        }
    });
}
